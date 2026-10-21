//! zsettle-app: a web page for zsettle. It drives the `zsettle` and `zsettle-seller`
//! binaries for every action, so the page adds no cryptography of its own.
//!
//! Two modes. Local (the default): one profile, on this machine only, behind a page key in
//! the link. Hosted (`--public https://…`): one server for many people behind a TLS proxy;
//! each visitor creates a wallet of their own, signs in with a passkey (or a wallet code and a
//! passphrase), and can open a shop that others reach at `<public>/shop/<code>`. An optional
//! house shop restocks itself and hands out test coins. Hosted mode is meant for the testnet:
//! the server holds every wallet.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use zeroize::Zeroizing;
use zsettle_core::listing::Listings;
use zsettle_core::wallet::Wallet;

const INDEX_HTML: &str = include_str!("../web/index.html");
const APP_JS: &str = include_str!("../web/app.js");
const STYLE_CSS: &str = include_str!("../web/style.css");
const QR_JS: &str = include_str!("../web/qr.js");
const FAVICON_SVG: &str = include_str!("../web/favicon.svg");
const MANIFEST: &str = include_str!("../web/manifest.webmanifest");
const ICON_192: &[u8] = include_bytes!("../web/icon-192.png");
const ICON_512: &[u8] = include_bytes!("../web/icon-512.png");

/// Largest request body the page sends (JSON forms).
const MAX_BODY: usize = 64 * 1024;
/// Largest offer relayed to a shop (the shop itself refuses more than 1 MiB).
const MAX_RELAY_BODY: usize = 1024 * 1024 + 4096;
/// Largest answer relayed back from a shop.
const MAX_RELAY_ANSWER: usize = 2 * 1024 * 1024;
/// Largest request head.
const MAX_HEAD: usize = 16 * 1024;
/// Connections served at once.
const MAX_CONNECTIONS: usize = 200;
/// Shop events kept for the page.
const MAX_EVENTS: usize = 300;
/// Activity entries kept.
const MAX_ACTIVITY: usize = 200;
/// Largest listings file fetched from a seller.
const MAX_LISTINGS_BYTES: u64 = 1024 * 1024;
/// Hosted: wallets the server keeps at most, and new ones per hour.
const MAX_PROFILES: usize = 500;
const MAX_NEW_PER_HOUR: usize = 40;
/// Hosted: a wallet nothing ever happened to (no coins, no items, nothing listed, no shop)
/// is removed after this long, so made-and-left wallets do not fill the server.
const UNUSED_WALLET_SECS: u64 = 86_400;
/// Hosted: new wallets an hour from one wider network (an IPv6 /48 or an IPv4 /24), so a
/// handful of addresses cannot fill the server either.
const MAX_NEW_PER_NET_HOUR: usize = 12;
/// Hosted: API calls a minute per signed-in wallet (a busy page makes about seventy), and
/// per visitor before sign-in, so one page cannot keep the server forking processes for
/// everyone else. Counted per wallet, not per address, so people behind one shared address
/// (an office, a phone network) do not count against each other.
const MAX_CALLS_PER_WALLET_MIN: usize = 240;
const MAX_CALLS_PER_IP_MIN: usize = 120;
/// Ownership proofs a wallet checks a minute: each reads the chain from the item's first
/// block.
const MAX_PROOF_CHECKS_PER_WALLET_MIN: usize = 3;
/// Hosted: shops open at once (each is a process that syncs every few seconds).
const MAX_OPEN_SHOPS: usize = 25;
/// Hosted: a session lasts this long; an idle wallet is locked after this long.
const SESSION_SECS: u64 = 12 * 3600;
const IDLE_LOCK_SECS: u64 = 2 * 3600;
/// Hosted: first port of the shops behind the relay (a range nothing common uses).
const FIRST_SHOP_PORT: u16 = 19100;
/// Work a server takes on at once: jobs per wallet and in all, passphrase checks (age scrypt
/// is heavy on CPU and memory).
const MAX_JOBS_PER_WALLET: usize = 2;
const MAX_JOBS_TOTAL: usize = 12;
const MAX_SCRYPT: usize = 4;
/// Commands that decrypt the seed and prove (pay, mint, buy, ...) running at once; each takes
/// a few hundred MB.
const MAX_HEAVY: usize = 3;
/// Hosted: faucet payments waiting at once (they wait outside the job limit, since the
/// house's coins can take minutes to confirm), and in all per hour.
const MAX_FAUCET_WAITING: usize = 4;
const MAX_FAUCET_PER_HOUR: usize = 12;
/// Hosted: passphrase checks (sign-ins, backups) per visitor and per wallet a minute, wrong
/// or right, so nobody keeps the checks busy for everyone else.
const MAX_CHECKS_PER_IP_MIN: usize = 10;
const MAX_CHECKS_PER_WALLET_MIN: usize = 6;
/// A request must arrive whole within this time.
const REQUEST_DEADLINE: Duration = Duration::from_secs(30);
/// Hosted: per visitor (by the proxy's X-Forwarded-For), new wallets an hour and test coins a
/// day; failed sign-ins before a wallet or a visitor must wait.
const MAX_NEW_PER_IP_HOUR: usize = 5;
const MAX_FAUCET_PER_IP_DAY: usize = 4;
const FREE_FAILURES: u32 = 5;
/// Hosted passphrases are at least this long.
const MIN_HOSTED_PASSPHRASE: usize = 12;

static RUNNING_JOBS: AtomicUsize = AtomicUsize::new(0);
static SCRYPT_RUNNING: AtomicUsize = AtomicUsize::new(0);
static HEAVY_RUNNING: AtomicUsize = AtomicUsize::new(0);
static FAUCET_WAITING: AtomicUsize = AtomicUsize::new(0);

/// A place in a counter capped at `max`, given back on drop.
struct Slot(&'static AtomicUsize);
impl Slot {
    fn try_take(counter: &'static AtomicUsize, max: usize) -> Option<Self> {
        if counter.fetch_add(1, Ordering::SeqCst) >= max {
            counter.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Slot(counter))
    }

    /// Wait up to `wait` for a place, in turn with the others waiting.
    fn take(counter: &'static AtomicUsize, max: usize, wait: Duration) -> Option<Self> {
        let until = Instant::now() + wait;
        loop {
            if let Some(s) = Self::try_take(counter, max) {
                return Some(s);
            }
            if Instant::now() >= until {
                return None;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// One of the few passphrase checks allowed at once (age scrypt is heavy on CPU and memory).
fn scrypt_slot() -> Result<Slot> {
    Slot::take(&SCRYPT_RUNNING, MAX_SCRYPT, Duration::from_secs(10))
        .ok_or_else(|| anyhow!("the server is busy; try again in a moment"))
}
/// Hosted house shop: items it keeps listed, their price, and the test coins it gives once
/// to each new wallet.
const HOUSE: &str = "house";
const HOUSE_STOCK: usize = 8;
/// What the house sells: a small catalogue, so the market looks like a market. Items are
/// minted as "<name> #<number>" and listed with the entry's icon, description and price.
struct HouseItem {
    name: &'static str,
    icon: &'static str,
    description: &'static str,
    price: &'static str,
}
const HOUSE_ITEMS: &[HouseItem] = &[
    HouseItem { name: "Concert ticket", icon: "\u{1f39f}\u{fe0f}", description: "One seat, Saturday night. The ticket is a shielded note; whoever holds the note holds the seat.", price: "0.0008 zec" },
    HouseItem { name: "Museum pass", icon: "\u{1f3ab}", description: "A day pass for the museum. Show an ownership proof at the door.", price: "0.0004 zec" },
    HouseItem { name: "Album code", icon: "\u{1f3b5}", description: "A download code for the album, kept in the note's encrypted memo.", price: "0.0003 zec" },
    HouseItem { name: "Art print", icon: "\u{1f3a8}", description: "One print of a small series. Its history can be traced on chain from the first owner.", price: "0.0012 zec" },
    HouseItem { name: "Beta key", icon: "\u{1f3ae}", description: "Early access to the game. Resell it when you are done.", price: "0.0005 zec" },
    HouseItem { name: "E-book", icon: "\u{1f4da}", description: "One copy of the e-book, one note.", price: "0.0006 zec" },
    HouseItem { name: "Tournament seat", icon: "\u{1f3c6}", description: "A seat at the tournament. Sell it on if you cannot come.", price: "0.001 zec" },
    HouseItem { name: "Collectible", icon: "\u{1f48e}", description: "A numbered collectible with a chain history from its first owner.", price: "0.0015 zec" },
    HouseItem { name: "Members pass", icon: "\u{1f511}", description: "A month of the members area. The door checks your proof, never your balance.", price: "0.0007 zec" },
    HouseItem { name: "Mystery box", icon: "\u{1f381}", description: "Who knows. A shielded note, like everything else here.", price: "0.0002 zec" },
    HouseItem { name: "Ticket", icon: "\u{1f39f}\u{fe0f}", description: "A plain ticket. Like everything here, a shielded note.", price: "0.0005 zec" },
];

/// One unspent note: reference, value in zatoshis, mined height (none while pending), and
/// whether it is change (received at the account's own change address).
type NoteRow = (String, u64, Option<u32>, bool);

/// The lines `zsettle notes` prints: `<note>  <value> zat (<zec>)  mined <height|pending>`, with
/// `  change` at the end of a change note.
fn parse_notes(text: &str) -> Vec<NoteRow> {
    let mut notes = Vec::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(note), Some(value)) = (parts.next(), parts.next()) else {
            continue;
        };
        let Ok(value) = value.parse::<u64>() else {
            continue;
        };
        let line = line.trim_end();
        let change = line.ends_with(" change");
        let mined = line
            .trim_end_matches("change")
            .rsplit("mined ")
            .next()
            .and_then(|h| h.trim().parse::<u32>().ok());
        notes.push((note.to_string(), value, mined, change));
    }
    notes
}

/// The catalogue entry an item name belongs to; the first entry for a name it does not know.
fn house_item(name: &str) -> &'static HouseItem {
    HOUSE_ITEMS
        .iter()
        .find(|h| {
            name.strip_prefix(h.name)
                .is_some_and(|r| r.is_empty() || r.starts_with(' '))
        })
        .unwrap_or(&HOUSE_ITEMS[0])
}
const FAUCET_ZAT: u64 = 300_000;
/// Variables the commands read; the app passes everything explicitly instead, so none of the
/// user's shell settings can redirect a command to another wallet.
const ENV_VARS: &[&str] = &[
    "ZSETTLE_WALLET",
    "ZSETTLE_IDENTITY",
    "ZSETTLE_ITEM_ACCOUNT",
    "ZSETTLE_ITEM_ACCOUNT_INDEX",
    "ZSETTLE_PAYOUT_ACCOUNT",
    "ZSETTLE_BUYER_ACCOUNT",
    "ZSETTLE_LISTINGS",
    "ZSETTLE_PASSPHRASE",
];

#[derive(Parser)]
#[command(
    name = "zsettle-app",
    version,
    about = "Web page for zsettle: a wallet, a market, buying and selling"
)]
struct Cli {
    /// Profile name (local mode): each profile has its own wallet, shop and history. Run two
    /// profiles on two ports to play buyer and seller on one machine.
    #[arg(long, default_value = "default")]
    profile: String,
    /// Directory holding the profiles (default: ~/.zsettle-app).
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Port of the page on 127.0.0.1.
    #[arg(long, default_value_t = 7070)]
    port: u16,
    /// Directory holding the `zsettle` and `zsettle-seller` binaries (default: next to this
    /// one).
    #[arg(long)]
    bin_dir: Option<PathBuf>,
    /// Local mode: replace this profile's page key, so links printed before stop working.
    #[arg(long)]
    new_token: bool,
    /// Hosted mode: the public address of the page, as visitors type it
    /// (https://demo.example). A TLS proxy in front forwards it to 127.0.0.1:<port>.
    #[arg(long)]
    public: Option<String>,
    /// Hosted mode: the network new wallets are made on.
    #[arg(long, default_value = "test")]
    network: String,
    /// Hosted mode: a file holding the house shop's passphrase. With it, a house shop is
    /// created if missing, unlocked at start, kept stocked, and pays the test coins.
    #[arg(long)]
    house_passphrase_file: Option<PathBuf>,
    /// Hosted mode on mainnet: the server would hold real money for its visitors. Refused
    /// unless this is given.
    #[arg(long)]
    allow_hosted_mainnet: bool,
}

/// What the app remembers about a profile. Nothing secret: the wallet's mnemonic stays
/// encrypted in the wallet directory, and a passkey entry is only its id and salt.
#[derive(Serialize, Deserialize, Default, Clone)]
struct Config {
    network: Option<String>,
    payout: Option<String>,
    item: Option<String>,
    passkey: Option<Value>,
    #[serde(default)]
    names: HashMap<String, String>,
    #[serde(default)]
    activity: Vec<Value>,
    #[serde(default)]
    sellers: Vec<String>,
    /// Shop each bought item came from, for its history.
    #[serde(default)]
    origins: HashMap<String, String>,
    /// The listed note each bought item came from: its history starts there, since only the
    /// seller's key opens it.
    #[serde(default)]
    origin_items: HashMap<String, String>,
    /// Icon and description of items bought, as their seller listed them.
    #[serde(default)]
    details: HashMap<String, Value>,
    shop_listen: Option<String>,
    /// The owner opened the shop and has not closed it: after a restart of the server it
    /// opens again when the owner next signs in.
    #[serde(default)]
    shop_wanted: bool,
    /// The last sync: its report and when.
    last_sync: Option<Value>,
    /// Hosted: how many items the house shop has made, for their names.
    #[serde(default)]
    minted: u64,
}

#[derive(Clone, Serialize)]
struct Job {
    id: u64,
    kind: String,
    state: String,
    steps: Vec<String>,
    result: Value,
    error: Option<String>,
}

struct Shop {
    child: Option<Child>,
    listen: Option<String>,
}

struct Bins {
    dir: PathBuf,
    buyer: PathBuf,
    seller: PathBuf,
}

/// One profile: a wallet, its shop, its jobs.
struct App {
    dir: PathBuf,
    profile: String,
    buyer_bin: PathBuf,
    seller_bin: PathBuf,
    /// Hosted: the shop's public address, and the fixed port it listens on behind the relay.
    public_shop: Option<String>,
    passphrase: Mutex<Option<Zeroizing<String>>>,
    cfg: Mutex<Config>,
    jobs: Mutex<HashMap<u64, Job>>,
    next_job: AtomicU64,
    wallet_lock: Mutex<()>,
    shop: Mutex<Shop>,
    /// The shop is stopped for a moment while a wallet action runs, and opens again after.
    paused: AtomicBool,
    events: Arc<Mutex<Vec<Value>>>,
    last_sync: Mutex<Value>,
    last_seen: AtomicU64,
    /// When this wallet checked ownership proofs lately.
    proof_checks: Mutex<Vec<u64>>,
}

/// The hosted side: every profile, who may use which, and the sessions.
#[derive(Serialize, Deserialize, Default)]
struct Index {
    /// Passkey credential id -> profile.
    by_cred: HashMap<String, String>,
    /// Profile -> its shop port.
    ports: HashMap<String, u16>,
    /// Profiles that got their test coins.
    funded: HashSet<String>,
    /// Profiles whose faucet payment was under way: after a restart each is looked at, and a
    /// wallet that never received anything gets its chance back.
    #[serde(default)]
    faucet_pending: HashSet<String>,
}

struct Session {
    profile: String,
    expires: u64,
}

struct Hosted {
    origin: String,
    host: String,
    network: String,
    /// The PRF salt every passkey of this site is asked for (base64url).
    salt: String,
    apps: Mutex<HashMap<String, Arc<App>>>,
    index: Mutex<Index>,
    sessions: Mutex<HashMap<String, Session>>,
    /// New wallets: when, and from which visitor.
    created: Mutex<Vec<(u64, String)>>,
    /// Test coins handed out per visitor, when; and in all, when.
    faucet_by_ip: Mutex<HashMap<String, Vec<u64>>>,
    faucet_all: Mutex<Vec<u64>>,
    /// Failed sign-ins (and backup requests) per visitor, and per wallet and visitor: count,
    /// the time before which the next try is refused, and the last failure. A count is
    /// forgotten an hour after its last failure.
    fails: Mutex<HashMap<String, (u32, u64, u64)>>,
    /// Passphrase checks per visitor and per wallet, when.
    checks: Mutex<HashMap<String, Vec<u64>>>,
}

impl Hosted {
    fn check_fails(&self, keys: &[String]) -> Result<()> {
        let f = self.fails.lock().unwrap();
        for k in keys {
            if let Some((_, until, _)) = f.get(k) {
                if *until > now() {
                    let mins = (*until - now()).div_ceil(60);
                    return Err(anyhow!(
                        "too many wrong tries; wait {mins} minute{} and try again",
                        if mins == 1 { "" } else { "s" }
                    ));
                }
            }
        }
        Ok(())
    }

    fn record_fail(&self, keys: &[String]) {
        let mut f = self.fails.lock().unwrap();
        let t = now();
        for k in keys {
            let e = f.entry(k.clone()).or_insert((0, 0, 0));
            if e.2 + 3600 < t {
                *e = (0, 0, 0);
            }
            e.0 += 1;
            e.2 = t;
            if e.0 >= FREE_FAILURES {
                let wait = 60u64 << (e.0 - FREE_FAILURES).min(6);
                e.1 = t + wait.min(3600);
            }
        }
    }

    fn clear_fails(&self, key: &str) {
        self.fails.lock().unwrap().remove(key);
    }

    /// Count one passphrase check against each (key, limit a minute); refused when one is
    /// over its limit.
    fn count_check(&self, keys: &[(String, usize)]) -> Result<()> {
        let mut c = self.checks.lock().unwrap();
        let t = now();
        for (k, max) in keys {
            let times = c.entry(k.clone()).or_default();
            times.retain(|x| *x + 60 > t);
            if times.len() >= *max {
                return Err(anyhow!("too many tries in a minute; wait a moment"));
            }
        }
        for (k, _) in keys {
            c.entry(k.clone()).or_default().push(t);
        }
        Ok(())
    }

    /// Drop what no limit needs any more.
    fn prune(&self) {
        let t = now();
        self.fails
            .lock()
            .unwrap()
            .retain(|_, e| e.1 > t || e.2 + 3600 > t);
        self.checks.lock().unwrap().retain(|_, v| {
            v.retain(|x| *x + 60 > t);
            !v.is_empty()
        });
        self.faucet_by_ip.lock().unwrap().retain(|_, v| {
            v.retain(|x| *x + 86_400 > t);
            !v.is_empty()
        });
        self.faucet_all.lock().unwrap().retain(|x| *x + 3600 > t);
    }
}

/// The visitor as the limits count them: an IPv4 address, or an IPv6 /64 (one home or one
/// server usually holds a whole /64).
fn visitor(ip: &str) -> String {
    match ip.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V6(a)) => match a.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let s = a.segments();
                format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
            }
        },
        Ok(a) => a.to_string(),
        Err(_) => ip.to_string(),
    }
}

/// The wider network a visitor key belongs to: the /48 of an IPv6 /64, the /24 of an IPv4
/// address.
fn wider_net(key: &str) -> String {
    if let Some(v6) = key.strip_suffix("::/64") {
        let segs: Vec<&str> = v6.split(':').collect();
        if segs.len() == 4 {
            return format!("{}:{}:{}::/48", segs[0], segs[1], segs[2]);
        }
    }
    if let Ok(std::net::IpAddr::V4(a)) = key.parse::<std::net::IpAddr>() {
        let o = a.octets();
        return format!("{}.{}.{}.0/24", o[0], o[1], o[2]);
    }
    key.to_string()
}

enum Mode {
    Local { token: String, app: Arc<App> },
    Hosted(Box<Hosted>),
}

struct Hub {
    base: PathBuf,
    bins: Bins,
    port: u16,
    mode: Mode,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let base = match &cli.data_dir {
        Some(d) => d.clone(),
        None => home()?.join(".zsettle-app"),
    };
    fs::create_dir_all(&base).with_context(|| format!("creating {}", base.display()))?;
    owner_only_dir(&base)?;
    let bin_dir = match &cli.bin_dir {
        Some(d) => d.clone(),
        None => std::env::current_exe()?
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| anyhow!("cannot find the directory of this program"))?,
    };
    let exe = |name: &str| bin_dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    let bins = Bins {
        buyer: exe("zsettle"),
        seller: exe("zsettle-seller"),
        dir: bin_dir.clone(),
    };
    for b in [&bins.buyer, &bins.seller] {
        if !b.exists() {
            return Err(anyhow!(
                "{} not found: keep zsettle-app next to zsettle and zsettle-seller, or pass --bin-dir",
                b.display()
            ));
        }
    }
    let listener = TcpListener::bind(("127.0.0.1", cli.port))
        .with_context(|| format!("listening on 127.0.0.1:{}", cli.port))?;
    let hub = match &cli.public {
        None => local_hub(&cli, base, bins)?,
        Some(public) => hosted_hub(&cli, public, base, bins)?,
    };
    let hub = Arc::new(hub);
    if let Mode::Hosted(_) = hub.mode {
        start_hosted_threads(&hub);
    }
    let open = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        if open.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
            let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
            respond(&mut stream, 503, "text/plain", b"busy, try again");
            continue;
        }
        open.fetch_add(1, Ordering::SeqCst);
        let (hub, open) = (hub.clone(), open.clone());
        thread::spawn(move || {
            handle(&hub, stream);
            open.fetch_sub(1, Ordering::SeqCst);
        });
    }
    Ok(())
}

fn local_hub(cli: &Cli, base: PathBuf, bins: Bins) -> Result<Hub> {
    if !valid_name(&cli.profile) {
        return Err(anyhow!("a profile name uses letters, digits, - and _"));
    }
    let dir = base.join(&cli.profile);
    let app = App::open(&dir, &cli.profile, &bins, None)?;
    // The page key is kept with the profile (owner-only), so the link stays the same across
    // restarts; it only keeps other web pages away from the app.
    let token_path = dir.join("page.token");
    let token = match fs::read_to_string(&token_path) {
        Ok(t)
            if !cli.new_token
                && t.trim().len() == 32
                && t.trim().bytes().all(|b| b.is_ascii_hexdigit()) =>
        {
            t.trim().to_string()
        }
        _ => {
            let t = hex::encode(rand::random::<[u8; 16]>());
            zsettle_core::wallet::write_owner_only(&token_path, t.as_bytes())?;
            t
        }
    };
    eprintln!(
        "zsettle-app, profile {:?}\nOpen this link in your browser (it carries this profile's page key; keep it to yourself; --new-token replaces it):\n\n    http://localhost:{}/#token={}\n",
        cli.profile, cli.port, token
    );
    Ok(Hub {
        base,
        bins,
        port: cli.port,
        mode: Mode::Local { token, app },
    })
}

fn hosted_hub(cli: &Cli, public: &str, base: PathBuf, bins: Bins) -> Result<Hub> {
    let origin = public.trim().trim_end_matches('/').to_string();
    let host = origin
        .strip_prefix("https://")
        .or_else(|| {
            origin
                .strip_prefix("http://")
                .filter(|h| h.starts_with("localhost") || h.starts_with("127.0.0.1"))
        })
        .ok_or_else(|| {
            anyhow!("--public is an https:// address (http:// only for localhost tests)")
        })?
        .to_string();
    if host.is_empty() || host.contains('/') {
        return Err(anyhow!(
            "--public is a bare origin such as https://demo.example"
        ));
    }
    if cli.network != "test" && cli.network != "main" {
        return Err(anyhow!("--network is test or main"));
    }
    if cli.network == "main" && !cli.allow_hosted_mainnet {
        return Err(anyhow!(
            "a hosted server holds its visitors' wallets: on mainnet that is real money in your custody; pass --allow-hosted-mainnet if you mean it"
        ));
    }
    if cli.network == "test" && !zsettle_core::broadcast::TESTNET_BUILD {
        return Err(anyhow!(
            "this build syncs mainnet only: build the hosted testnet server from the nu7-testnet branch"
        ));
    }
    let index: Index = read_state(&base.join("hub.json"))?;
    // One PRF salt for the whole site, kept with the data: a passkey of this site always
    // gives the same passphrase.
    let salt_path = base.join("prf.salt");
    let salt = match fs::read_to_string(&salt_path) {
        Ok(s) if s.trim().len() >= 40 => s.trim().to_string(),
        _ => {
            let s = b64u(&rand::random::<[u8; 32]>());
            zsettle_core::wallet::write_owner_only(&salt_path, s.as_bytes())?;
            s
        }
    };
    let mut apps = HashMap::new();
    for entry in fs::read_dir(&base)?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if entry.path().join("app.json").exists() && valid_name(&name) {
            let public_shop = Some(format!("{origin}/shop/{name}"));
            apps.insert(
                name.clone(),
                App::open(&entry.path(), &name, &bins, public_shop)?,
            );
        }
    }
    let hosted = Hosted {
        origin: origin.clone(),
        host,
        network: cli.network.clone(),
        salt,
        apps: Mutex::new(apps),
        index: Mutex::new(index),
        sessions: Mutex::new(HashMap::new()),
        created: Mutex::new(Vec::new()),
        faucet_by_ip: Mutex::new(HashMap::new()),
        faucet_all: Mutex::new(Vec::new()),
        fails: Mutex::new(HashMap::new()),
        checks: Mutex::new(HashMap::new()),
    };
    let hub = Hub {
        base,
        bins,
        port: cli.port,
        mode: Mode::Hosted(Box::new(hosted)),
    };
    if let Some(file) = &cli.house_passphrase_file {
        let pass = Zeroizing::new(
            fs::read_to_string(file)
                .with_context(|| format!("reading {}", file.display()))?
                .trim()
                .to_string(),
        );
        hub.open_house(&pass)?;
    }
    eprintln!(
        "zsettle-app, hosted at {origin} ({} network), listening on 127.0.0.1:{} behind the TLS proxy",
        cli.network, cli.port
    );
    Ok(hub)
}

/// A note reference as the tools print it: a transaction id and an output index.
fn valid_note(s: &str) -> bool {
    s.split_once(':').is_some_and(|(txid, i)| {
        txid.len() == 64
            && txid.bytes().all(|b| b.is_ascii_hexdigit())
            && !i.is_empty()
            && i.len() <= 4
            && i.bytes().all(|b| b.is_ascii_digit())
    })
}

fn note_field(body: &Value, key: &str) -> Result<String> {
    let n = field(body, key)?;
    if !valid_note(&n) {
        return Err(anyhow!("that is not an item reference"));
    }
    Ok(n)
}

fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 40
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Read a state file: missing gives the default; a file that does not parse stops the app
/// rather than be replaced, since it holds what no one can rebuild (which passkey opens which
/// wallet, for one).
fn read_state<T: serde::de::DeserializeOwned + Default>(path: &Path) -> Result<T> {
    match fs::read(path) {
        Ok(b) => serde_json::from_slice(&b).with_context(|| {
            format!(
                "{} is damaged; restore it from a backup, or move it away to start empty",
                path.display()
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn owner_only_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    let _ = dir;
    Ok(())
}

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("no home directory; pass --data-dir"))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn b64u(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |a, (i, &b)| a | (b as u32) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(A[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

// ---------------------------------------------------------------- HTTP

struct Request {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

/// Read once, waiting no longer than the time left before the deadline.
fn read_some(stream: &mut TcpStream, started: Instant, chunk: &mut [u8]) -> Result<usize> {
    let left = REQUEST_DEADLINE
        .checked_sub(started.elapsed())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| anyhow!("request too slow"))?;
    stream.set_read_timeout(Some(left.min(Duration::from_secs(10))))?;
    Ok(stream.read(chunk)?)
}

fn read_request(stream: &mut TcpStream) -> Result<Request> {
    let started = Instant::now();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
        if buf.len() > MAX_HEAD {
            return Err(anyhow!("request head too large"));
        }
        let n = read_some(stream, started, &mut chunk)?;
        if n == 0 {
            return Err(anyhow!("connection closed"));
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let (method, target, headers) = parse_head(&head)?;
    let path = target.split('?').next().unwrap_or_default().to_string();
    let len: usize = headers
        .get("content-length")
        .map(|v| v.parse())
        .transpose()?
        .unwrap_or(0);
    let limit = if path.starts_with("/shop/") {
        MAX_RELAY_BODY
    } else {
        MAX_BODY
    };
    if len > limit {
        return Err(anyhow!("request body too large"));
    }
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len {
        let n = read_some(stream, started, &mut chunk)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(len);
    Ok(Request {
        method,
        path,
        headers,
        body,
    })
}

/// The request line and headers. Bodies come with a Content-Length only: a chunked body, or
/// a Host or Content-Length given twice, is refused rather than guessed at.
fn parse_head(head: &str) -> Result<(String, String, HashMap<String, String>)> {
    let mut lines = head.split("\r\n");
    let first = lines.next().unwrap_or_default();
    let mut parts = first.split(' ');
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    let mut headers = HashMap::new();
    for (k, v) in lines.filter_map(|l| l.split_once(':')) {
        let k = k.trim().to_ascii_lowercase();
        if k == "transfer-encoding" {
            return Err(anyhow!("chunked bodies are not accepted"));
        }
        if headers.contains_key(&k) && (k == "host" || k == "content-length") {
            return Err(anyhow!("repeated {k}"));
        }
        headers.insert(k, v.trim().to_string());
    }
    Ok((method, target, headers))
}

fn respond(stream: &mut TcpStream, status: u16, ctype: &str, body: &[u8]) {
    respond_with(stream, status, ctype, body, "");
}

fn respond_with(stream: &mut TcpStream, status: u16, ctype: &str, body: &[u8], extra: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        413 => "Payload Too Large",
        421 => "Misdirected Request",
        429 => "Too Many Requests",
        202 => "Accepted",
        410 => "Gone",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; manifest-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'\r\n{extra}Connection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

fn static_file(path: &str) -> Option<(&'static str, &'static [u8])> {
    Some(match path {
        "/" | "/index.html" => ("text/html; charset=utf-8", INDEX_HTML.as_bytes()),
        "/app.js" => ("text/javascript; charset=utf-8", APP_JS.as_bytes()),
        "/qr.js" => ("text/javascript; charset=utf-8", QR_JS.as_bytes()),
        "/style.css" => ("text/css; charset=utf-8", STYLE_CSS.as_bytes()),
        "/favicon.svg" => ("image/svg+xml", FAVICON_SVG.as_bytes()),
        // The page can be added to a phone's home screen and opens like an app.
        "/manifest.webmanifest" => ("application/manifest+json", MANIFEST.as_bytes()),
        "/icon-192.png" => ("image/png", ICON_192),
        "/icon-512.png" => ("image/png", ICON_512),
        _ => return None,
    })
}

fn handle(hub: &Arc<Hub>, mut stream: TcpStream) {
    let req = match read_request(&mut stream) {
        Ok(r) => r,
        Err(_) => return respond(&mut stream, 400, "text/plain", b"bad request"),
    };
    match &hub.mode {
        Mode::Local { token, app } => handle_local(hub, app, token, req, stream),
        Mode::Hosted(h) => handle_hosted(hub, h, req, stream),
    }
}

fn json_answer(stream: &mut TcpStream, r: Result<Value>, extra: &str) {
    let (status, body) = match r {
        Ok(v) => (200, v),
        Err(e) => (400, json!({ "error": format!("{e:#}") })),
    };
    respond_with(
        stream,
        status,
        "application/json",
        body.to_string().as_bytes(),
        extra,
    );
}

fn handle_local(hub: &Arc<Hub>, app: &Arc<App>, token: &str, req: Request, mut stream: TcpStream) {
    // Only this page, under its own name, may talk to the app: a Host check stops DNS
    // rebinding, the Origin check and the key stop other web pages.
    let ours = [
        format!("localhost:{}", hub.port),
        format!("127.0.0.1:{}", hub.port),
    ];
    if !req.headers.get("host").is_some_and(|h| ours.contains(h)) {
        return respond(&mut stream, 421, "text/plain", b"wrong host");
    }
    if let Some(origin) = req.headers.get("origin") {
        if !ours.iter().any(|o| origin == &format!("http://{o}")) {
            return respond(&mut stream, 403, "text/plain", b"wrong origin");
        }
    }
    if req.method == "GET" {
        if let Some((ctype, body)) = static_file(&req.path) {
            return respond(&mut stream, 200, ctype, body);
        }
    }
    if req.path == "/api/hello" {
        return json_answer(&mut stream, Ok(json!({ "mode": "local" })), "");
    }
    if !req.path.starts_with("/api/") {
        return respond(&mut stream, 404, "text/plain", b"not found");
    }
    if req.headers.get("x-zsettle-token").map(String::as_str) != Some(token) {
        return respond(
            &mut stream,
            401,
            "application/json",
            br#"{"error":"open the link the app printed in its terminal"}"#,
        );
    }
    let r = match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/api/wallet/create") => parse_body(&req).and_then(|b| app.create_wallet(&b)),
        ("POST", "/api/unlock") => parse_body(&req).and_then(|b| {
            let r = app.unlock(&b)?;
            app.reopen_shop(true);
            Ok(r)
        }),
        ("POST", "/api/backup") => parse_body(&req).and_then(|b| app.backup(&b)),
        _ => api(app, &req),
    };
    json_answer(&mut stream, r, "");
}

fn parse_body(req: &Request) -> Result<Value> {
    if req.body.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_slice(&req.body).context("the request is not JSON")
}

fn cookie(req: &Request, name: &str) -> Option<String> {
    req.headers.get("cookie").and_then(|c| {
        c.split(';')
            .filter_map(|p| p.trim().split_once('='))
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.to_string())
    })
}

fn handle_hosted(hub: &Arc<Hub>, h: &Hosted, req: Request, mut stream: TcpStream) {
    if req.headers.get("host").map(String::as_str) != Some(h.host.as_str()) {
        return respond(&mut stream, 421, "text/plain", b"wrong host");
    }
    if let Some(origin) = req.headers.get("origin") {
        if origin != &h.origin {
            return respond(&mut stream, 403, "text/plain", b"wrong origin");
        }
    }
    if let Some(rest) = req.path.strip_prefix("/shop/") {
        return relay_shop(h, rest, &req, stream);
    }
    // The proxy in front sets X-Forwarded-For; the app listens on 127.0.0.1 only, so nobody
    // else can.
    let ip = req
        .headers
        .get("x-forwarded-for")
        .and_then(|v| v.split(',').next())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(|v| visitor(&v))
        .unwrap_or_else(|| "local".into());
    if req.method == "GET" {
        if let Some((ctype, body)) = static_file(&req.path) {
            return respond(&mut stream, 200, ctype, body);
        }
    }
    if !req.path.starts_with("/api/") {
        return respond(&mut stream, 404, "text/plain", b"not found");
    }
    // A cross-site form cannot set this header, and a cross-site script cannot send it
    // without a CORS preflight this server never answers.
    if req.method == "POST" && req.headers.get("x-zsettle").map(String::as_str) != Some("1") {
        return respond(&mut stream, 403, "text/plain", b"missing header");
    }
    let session = cookie(&req, "__Host-zs").and_then(|t| {
        let mut s = h.sessions.lock().unwrap();
        match s.get(&t) {
            Some(x) if x.expires > now() => Some((t, x.profile.clone())),
            Some(_) => {
                s.remove(&t);
                None
            }
            None => None,
        }
    });
    let app = session
        .as_ref()
        .and_then(|(_, p)| h.apps.lock().unwrap().get(p).cloned());
    if let Some(a) = &app {
        a.last_seen.store(now(), Ordering::SeqCst);
    }
    let call_key = match &session {
        Some((_, p)) => (format!("c:w:{p}"), MAX_CALLS_PER_WALLET_MIN),
        None => (format!("c:{ip}"), MAX_CALLS_PER_IP_MIN),
    };
    if h.count_check(&[call_key]).is_err() {
        return respond(
            &mut stream,
            429,
            "application/json",
            br#"{"error":"too many requests; wait a moment"}"#,
        );
    }
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/api/hello") => {
            let r = Ok(json!({
                "mode": "hosted", "network": h.network, "salt": h.salt,
                "signed_in": app.is_some(), "has_house": h.apps.lock().unwrap().contains_key(HOUSE),
            }));
            return json_answer(&mut stream, r, "");
        }
        ("POST", "/api/wallet/create") => {
            let r = parse_body(&req)
                .and_then(|b| hub.hosted_create(h, &b, &ip))
                .map(|(mut v, token)| {
                    // A new wallet gets its test coins at once: one step fewer for a visitor.
                    // A refusal (limits, no house) just leaves the "Get coins" button.
                    let app = v["code"]
                        .as_str()
                        .and_then(|c| h.apps.lock().unwrap().get(c).cloned());
                    if let Some(app) = app {
                        if let Ok(j) = hub.faucet(h, &app, &ip) {
                            v["faucet"] = j;
                        }
                    }
                    (v, token)
                });
            return answer_with_session(hub, &mut stream, r);
        }
        ("POST", "/api/unlock") => {
            let r = parse_body(&req).and_then(|b| hub.hosted_unlock(h, &b, &ip));
            return answer_with_session(hub, &mut stream, r);
        }
        _ => {}
    }
    let Some(app) = app else {
        return respond(
            &mut stream,
            401,
            "application/json",
            br#"{"error":"sign in first","signin":true}"#,
        );
    };
    let r = match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/api/lock") => {
            if let Some((t, _)) = &session {
                h.sessions.lock().unwrap().remove(t);
            }
            *app.passphrase.lock().unwrap() = None;
            return json_answer(
                &mut stream,
                Ok(json!({ "locked": true })),
                "Set-Cookie: __Host-zs=; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=0\r\n",
            );
        }
        ("GET", "/api/status") => app.status().map(|mut v| {
            v["hosted"] = json!(true);
            v["code"] = json!(app.profile);
            v["public_shop"] = json!(app.public_shop);
            v["faucet"] = json!(
                h.apps.lock().unwrap().contains_key(HOUSE)
                    && !h.index.lock().unwrap().funded.contains(&app.profile)
            );
            v
        }),
        ("GET", "/api/shops") => Ok(hub.open_shops(h)),
        ("POST", "/api/faucet") => hub.faucet(h, &app, &ip),
        ("POST", "/api/shop/start") => {
            let open = h
                .apps
                .lock()
                .unwrap()
                .values()
                .filter(|a| a.shop_running())
                .count();
            if open >= MAX_OPEN_SHOPS && !app.shop_running() {
                Err(anyhow!(
                    "the server has too many open shops right now; try later"
                ))
            } else {
                app.shop_start(&json!({}))
            }
        }
        ("POST", "/api/backup") => {
            let keys = [format!("w:{}:{ip}", app.profile), format!("ip:{ip}")];
            h.check_fails(&keys)
                .and_then(|_| {
                    h.count_check(&[
                        (format!("ip:{ip}"), MAX_CHECKS_PER_IP_MIN),
                        (format!("w:{}", app.profile), MAX_CHECKS_PER_WALLET_MIN),
                    ])
                })
                .and_then(|_| {
                    parse_body(&req)
                        .and_then(|b| app.backup(&b))
                        .inspect_err(|_| {
                            h.record_fail(&keys);
                            thread::sleep(Duration::from_secs(1));
                        })
                })
        }
        _ => api(&app, &req),
    };
    json_answer(&mut stream, scrub(hub, r), "");
}

/// No server paths in what a visitor sees.
fn scrub(hub: &Hub, r: Result<Value>) -> Result<Value> {
    r.map_err(|e| {
        let mut text = format!("{e:#}");
        for p in [&hub.base, &hub.bins.dir] {
            text = text.replace(&p.display().to_string(), "");
        }
        anyhow!(text)
    })
}

fn answer_with_session(hub: &Hub, stream: &mut TcpStream, r: Result<(Value, String)>) {
    match r {
        Ok((v, token)) => {
            let set = format!(
                "Set-Cookie: __Host-zs={token}; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age={SESSION_SECS}\r\n"
            );
            json_answer(stream, Ok(v), &set);
        }
        Err(e) => json_answer(stream, scrub(hub, Err(e)), ""),
    }
}

/// Forward `/shop/<code>/<rest>` to that profile's shop on its local port. Only the three
/// requests a shop answers pass: its listings, an offer, and a reply.
fn relay_shop(h: &Hosted, rest: &str, req: &Request, mut stream: TcpStream) {
    let (code, tail) = rest.split_once('/').unwrap_or((rest, ""));
    let allowed = matches!(
        (req.method.as_str(), tail),
        ("GET", "listings") | ("POST", "offer")
    ) || (req.method == "GET" && tail.starts_with("reply/") && tail.len() <= 6 + 64);
    if !valid_name(code) || !allowed {
        return respond(&mut stream, 404, "text/plain", b"not found");
    }
    let app = h.apps.lock().unwrap().get(code).cloned();
    let Some(app) = app else {
        return respond(&mut stream, 503, "text/plain", b"this shop is closed");
    };
    // A shop paused for a wallet action still shows its signed listings file (the very bytes
    // it serves), and an offer or a reply waits a while for the shop to open again.
    if !app.shop_running() && app.paused.load(Ordering::SeqCst) {
        if tail == "listings" {
            return match fs::read(app.listings_path()) {
                Ok(b) => respond(&mut stream, 200, "application/json", &b),
                Err(_) => respond(&mut stream, 503, "text/plain", b"this shop is closed"),
            };
        }
        let until = Instant::now() + Duration::from_secs(25);
        while !app.shop_running() && app.paused.load(Ordering::SeqCst) && Instant::now() < until {
            thread::sleep(Duration::from_millis(500));
        }
    }
    if !app.shop_running() {
        return respond(&mut stream, 503, "text/plain", b"this shop is closed");
    }
    let port = h.index.lock().unwrap().ports.get(code).copied();
    let Some(port) = port else {
        return respond(&mut stream, 503, "text/plain", b"this shop is closed");
    };
    let relay = || -> Result<Vec<u8>> {
        let mut up = TcpStream::connect(("127.0.0.1", port))?;
        up.set_read_timeout(Some(Duration::from_secs(20)))?;
        let mut head = format!(
            "{} /{} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Length: {}\r\n",
            req.method,
            tail,
            req.body.len()
        );
        if req.method == "POST" {
            head.push_str("Content-Type: application/octet-stream\r\n");
        }
        head.push_str("\r\n");
        up.write_all(head.as_bytes())?;
        up.write_all(&req.body)?;
        let mut out = Vec::new();
        up.take(MAX_RELAY_ANSWER as u64).read_to_end(&mut out)?;
        Ok(out)
    };
    match relay() {
        Ok(bytes) => {
            // Rebuild the answer from the shop's status and body, so it carries this server's
            // headers like every other answer.
            let split = bytes.windows(4).position(|w| w == b"\r\n\r\n");
            let Some(end) = split else {
                return respond(&mut stream, 502, "text/plain", b"the shop did not answer");
            };
            let head = String::from_utf8_lossy(&bytes[..end]);
            let status: u16 = head
                .split_whitespace()
                .nth(1)
                .and_then(|c| c.parse().ok())
                .unwrap_or(502);
            let ctype = match head
                .lines()
                .find_map(|l| {
                    l.split_once(':')
                        .filter(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                })
                .map(|(_, v)| v.trim().to_ascii_lowercase())
                .as_deref()
            {
                Some("application/json") => "application/json",
                Some("application/octet-stream") => "application/octet-stream",
                _ => "text/plain; charset=utf-8",
            };
            respond(&mut stream, status, ctype, &bytes[end + 4..]);
        }
        Err(_) => respond(&mut stream, 502, "text/plain", b"the shop did not answer"),
    }
    let _ = app;
}

/// An optional text field: absent or blank gives none.
fn opt_text(body: &Value, key: &str) -> Option<String> {
    body.get(key)
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn field(body: &Value, key: &str) -> Result<String> {
    body.get(key)
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("missing {key}"))
}

/// The requests one profile answers, in either mode.
fn api(app: &Arc<App>, req: &Request) -> Result<Value> {
    let body = parse_body(req)?;
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/api/status") => app.status(),
        ("POST", "/api/lock") => {
            *app.passphrase.lock().unwrap() = None;
            Ok(json!({ "locked": true }))
        }
        ("GET", "/api/wallet") => app.wallet_view(),
        ("POST", "/api/address") => app.address(),
        ("POST", "/api/sync") => app.start_sync(),
        ("POST", "/api/pay") => app.start_pay(&body),
        ("POST", "/api/mint") => app.start_mint(&body),
        ("GET", "/api/listings") => app.own_listings(),
        ("POST", "/api/publish") => app.start_publish(&body),
        ("POST", "/api/delist") => app.start_delist(&body),
        ("POST", "/api/cancel") => app.start_cancel(&body),
        ("GET", "/api/shop") => app.shop_view(),
        ("POST", "/api/shop/start") => app.shop_start(&body),
        ("POST", "/api/shop/stop") => {
            app.stop_shop();
            app.set_shop_wanted(false);
            app.shop_view()
        }
        ("POST", "/api/market") => app.market(&body),
        ("POST", "/api/buy") => app.start_buy(&body),
        ("POST", "/api/verify") => app.start_verify(&body),
        ("POST", "/api/prove") => app.start_prove(&body),
        ("POST", "/api/check-proof") => app.start_check_proof(&body),
        ("GET", "/api/activity") => {
            app.absorb_sales();
            Ok(json!(app.cfg.lock().unwrap().activity))
        }
        ("GET", p) if p.starts_with("/api/jobs/") => {
            let id: u64 = p["/api/jobs/".len()..].parse().context("job id")?;
            let jobs = app.jobs.lock().unwrap();
            let job = jobs.get(&id).ok_or_else(|| anyhow!("no such job"))?;
            Ok(serde_json::to_value(job)?)
        }
        _ => Err(anyhow!("unknown request")),
    }
}

// ---------------------------------------------------------------- hosted

impl Hub {
    fn save_index(&self, h: &Hosted) -> Result<()> {
        // Written while the lock is held, so two saves never mix.
        let idx = h.index.lock().unwrap();
        let bytes = serde_json::to_vec_pretty(&*idx)?;
        zsettle_core::wallet::write_owner_only(&self.base.join("hub.json"), &bytes)
    }

    fn new_session(&self, h: &Hosted, profile: &str) -> String {
        let token = hex::encode(rand::random::<[u8; 24]>());
        h.sessions.lock().unwrap().insert(
            token.clone(),
            Session {
                profile: profile.to_string(),
                expires: now() + SESSION_SECS,
            },
        );
        token
    }

    /// A port for a profile's shop, kept for good.
    fn shop_port(&self, h: &Hosted, profile: &str) -> Result<u16> {
        let port = {
            let mut idx = h.index.lock().unwrap();
            if let Some(p) = idx.ports.get(profile) {
                return Ok(*p);
            }
            let used: HashSet<u16> = idx.ports.values().copied().collect();
            let port = (FIRST_SHOP_PORT..FIRST_SHOP_PORT + 2000)
                .find(|p| !used.contains(p))
                .ok_or_else(|| anyhow!("no free shop port"))?;
            idx.ports.insert(profile.to_string(), port);
            port
        };
        self.save_index(h)?;
        Ok(port)
    }

    fn new_app(&self, h: &Hosted, code: &str) -> Result<Arc<App>> {
        let port = self.shop_port(h, code)?;
        let app = App::open(
            &self.base.join(code),
            code,
            &self.bins,
            Some(format!("{}/shop/{code}", h.origin)),
        )?;
        app.cfg.lock().unwrap().shop_listen = Some(format!("127.0.0.1:{port}"));
        Ok(app)
    }

    fn hosted_create(&self, h: &Hosted, body: &Value, ip: &str) -> Result<(Value, String)> {
        if field(body, "passphrase").map(|p| p.len()).unwrap_or(0) < MIN_HOSTED_PASSPHRASE {
            return Err(anyhow!(
                "use a passphrase of at least {MIN_HOSTED_PASSPHRASE} characters"
            ));
        }
        // The light scrypt factor is for passkey secrets only: the `passkey:` shape comes
        // with a passkey, never typed.
        let with_passkey = body["passkey"]["id"]
            .as_str()
            .is_some_and(|id| !id.is_empty());
        if field(body, "passphrase")?.starts_with("passkey:") != with_passkey {
            return Err(anyhow!("that passphrase shape is reserved for passkeys"));
        }
        let cred = body["passkey"]["id"].as_str().map(str::to_string);
        if let Some(id) = &cred {
            if id.len() > 1400 || h.index.lock().unwrap().by_cred.contains_key(id) {
                return Err(anyhow!("this passkey already belongs to a wallet here"));
            }
        }
        // A place is held while the wallet is made, and given back if that fails, so only
        // wallets that exist count against the limits.
        let stamp = (now(), ip.to_string());
        {
            let mut created = h.created.lock().unwrap();
            created.retain(|(t, _)| *t + 3600 > now());
            if created.len() >= MAX_NEW_PER_HOUR {
                return Err(anyhow!(
                    "many wallets were created this hour; try again later"
                ));
            }
            if created.iter().filter(|(_, who)| who == ip).count() >= MAX_NEW_PER_IP_HOUR {
                return Err(anyhow!(
                    "you created several wallets this hour; try again later"
                ));
            }
            let net = wider_net(ip);
            if created
                .iter()
                .filter(|(_, who)| wider_net(who) == net)
                .count()
                >= MAX_NEW_PER_NET_HOUR
            {
                return Err(anyhow!(
                    "many wallets were created from your network this hour; try again later"
                ));
            }
            if h.apps.lock().unwrap().len() >= MAX_PROFILES {
                return Err(anyhow!("this server is full"));
            }
            created.push(stamp.clone());
        }
        let give_back = || {
            let mut created = h.created.lock().unwrap();
            if let Some(i) = created.iter().position(|c| *c == stamp) {
                created.remove(i);
            }
        };
        let r = self.hosted_create_inner(h, body, cred);
        if r.is_err() {
            give_back();
        }
        r
    }

    fn hosted_create_inner(
        &self,
        h: &Hosted,
        body: &Value,
        cred: Option<String>,
    ) -> Result<(Value, String)> {
        let _slot = scrypt_slot()?;
        let code = loop {
            const ALPHA: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
            let c: String = rand::random::<[u8; 8]>()
                .iter()
                .map(|b| ALPHA[*b as usize % ALPHA.len()] as char)
                .collect();
            if !self.base.join(&c).exists() {
                break c;
            }
        };
        let app = self.new_app(h, &code)?;
        let mut b = body.clone();
        b["network"] = json!(h.network);
        if let Some(pk) = b.get_mut("passkey").filter(|v| v.is_object()) {
            pk["salt"] = json!(h.salt);
        }
        let r = match app.create_wallet(&b) {
            Ok(r) => r,
            Err(e) => {
                let _ = fs::remove_dir_all(self.base.join(&code));
                h.index.lock().unwrap().ports.remove(&code);
                let _ = self.save_index(h);
                return Err(e);
            }
        };
        if let Some(id) = cred {
            // Never re-point a passkey that already names a wallet.
            h.index
                .lock()
                .unwrap()
                .by_cred
                .entry(id)
                .or_insert_with(|| code.clone());
            self.save_index(h)?;
        }
        h.apps.lock().unwrap().insert(code.clone(), app);
        let token = self.new_session(h, &code);
        let mut v = r;
        v["code"] = json!(code);
        Ok((v, token))
    }

    fn hosted_unlock(&self, h: &Hosted, body: &Value, ip: &str) -> Result<(Value, String)> {
        let ip_key = format!("ip:{ip}");
        h.check_fails(std::slice::from_ref(&ip_key))?;
        let (code, by_passkey) = match field(body, "credential") {
            Ok(id) => (
                h.index
                    .lock()
                    .unwrap()
                    .by_cred
                    .get(&id)
                    .cloned()
                    .ok_or_else(|| anyhow!("this passkey has no wallet on this server"))?,
                true,
            ),
            Err(_) => (field(body, "code")?.to_ascii_lowercase(), false),
        };
        // Wrong tries count per visitor, and per wallet for that visitor only: wallet codes
        // are public (they name the shops), so a count shared by everyone would let anyone
        // lock an owner out. A passkey's secret cannot be guessed, so it is never held back
        // by the wallet's count.
        let wallet_key = format!("w:{code}:{ip}");
        let keys = [wallet_key.clone(), ip_key.clone()];
        if !by_passkey {
            h.check_fails(&keys)?;
        }
        // Every check counts against the visitor, right or wrong (not against the wallet:
        // that again would let anyone hold an owner back).
        h.count_check(&[(ip_key, MAX_CHECKS_PER_IP_MIN)])?;
        let app = h.apps.lock().unwrap().get(&code).cloned();
        let app = match app {
            Some(a) if code != HOUSE => a,
            _ => {
                h.record_fail(&keys[1..]);
                thread::sleep(Duration::from_secs(1));
                return Err(anyhow!("wrong wallet code or passphrase"));
            }
        };
        // A failed guess costs a second and counts against the visitor, so codes and
        // passphrases cannot be tried quickly. The check's slot is given back first.
        let ok = {
            let _slot = scrypt_slot()?;
            app.unlock(body).is_ok()
        };
        if !ok {
            h.record_fail(&keys);
            thread::sleep(Duration::from_secs(1));
            return Err(anyhow!("wrong wallet code or passphrase"));
        }
        h.clear_fails(&wallet_key);
        let open = h
            .apps
            .lock()
            .unwrap()
            .values()
            .filter(|a| a.shop_running())
            .count();
        app.reopen_shop(open < MAX_OPEN_SHOPS);
        let token = self.new_session(h, &code);
        Ok((json!({ "unlocked": true, "code": code }), token))
    }

    fn open_shops(&self, h: &Hosted) -> Value {
        let apps: Vec<Arc<App>> = h.apps.lock().unwrap().values().cloned().collect();
        let mut shops: Vec<Value> = apps
            .iter()
            .filter(|a| a.shop_running())
            .map(|a| {
                let items = Listings::read(&a.listings_path())
                    .map(|l| l.items.len())
                    .unwrap_or(0);
                json!({
                    "base": a.public_shop,
                    "code": a.profile,
                    "name": if a.profile == HOUSE { "House shop".to_string() } else { format!("Shop {}", a.profile) },
                    "items": items,
                    "house": a.profile == HOUSE,
                })
            })
            .collect();
        shops.sort_by_key(|s| {
            (
                !s["house"].as_bool().unwrap_or(false),
                std::cmp::Reverse(s["items"].as_u64().unwrap_or(0)),
            )
        });
        json!(shops)
    }

    /// Test coins from the house wallet, once per wallet.
    fn faucet(self: &Arc<Self>, h: &Hosted, app: &Arc<App>, ip: &str) -> Result<Value> {
        let house = h
            .apps
            .lock()
            .unwrap()
            .get(HOUSE)
            .cloned()
            .ok_or_else(|| anyhow!("this server gives no test coins"))?;
        if h.network != "test" {
            return Err(anyhow!("test coins exist on the testnet only"));
        }
        if h.index.lock().unwrap().funded.contains(&app.profile) {
            return Err(anyhow!("this wallet already got its test coins"));
        }
        // Everything that can refuse comes before the wallet is marked as paid, so a refusal
        // never uses up its chance.
        let addr = app.address()?["address"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("no address"))?;
        // Faucet payments wait outside the job limit (the house's coins can take minutes to
        // confirm), a few at a time.
        let slot = Slot::try_take(&FAUCET_WAITING, MAX_FAUCET_WAITING).ok_or_else(|| {
            anyhow!("many people are getting test coins right now; try again in a minute")
        })?;
        let stamp = now();
        {
            let mut all = h.faucet_all.lock().unwrap();
            all.retain(|t| *t + 3600 > stamp);
            if all.len() >= MAX_FAUCET_PER_HOUR {
                return Err(anyhow!(
                    "the faucet gave out many coins this hour; try again later"
                ));
            }
            let mut by_ip = h.faucet_by_ip.lock().unwrap();
            let times = by_ip.entry(ip.to_string()).or_default();
            times.retain(|t| *t + 86_400 > stamp);
            if times.len() >= MAX_FAUCET_PER_IP_DAY {
                return Err(anyhow!(
                    "you got test coins for several wallets today; try again tomorrow"
                ));
            }
            {
                let mut idx = h.index.lock().unwrap();
                if !idx.funded.insert(app.profile.clone()) {
                    return Err(anyhow!("this wallet already got its test coins"));
                }
                idx.faucet_pending.insert(app.profile.clone());
            }
            times.push(stamp);
            all.push(stamp);
        }
        let profile = app.profile.clone();
        let ip = ip.to_string();
        let hub = self.clone();
        if let Err(e) = self.save_index(h) {
            hub.unfund(&profile, &ip, stamp);
            return Err(e);
        }
        app.free_job("faucet", move |app, id| {
            let _slot = slot;
            app.step(id, "Sending free test coins");
            // The house's coins may be confirming after a restock: wait for them a while
            // rather than fail at once. A failure once the payment may have been sent is not
            // tried again, so the faucet never pays twice.
            let started = now();
            let mut said = false;
            let paid = loop {
                match house.faucet_pay(&addr, FAUCET_ZAT) {
                    Ok(r) => break Ok(r),
                    Err(e) if now() < started + 600 && !maybe_sent(&e) => {
                        if !said {
                            app.step(id, "Waiting for the faucet's coins to confirm (a few minutes)");
                            said = true;
                        }
                        house.push_event(json!({ "event": "log", "text": format!("faucet waits: {e:#}") }));
                        thread::sleep(Duration::from_secs(30));
                    }
                    Err(e) => break Err(e),
                }
            };
            let r = match paid {
                Ok(r) => {
                    app.add_activity(json!({ "kind": "faucet", "txid": r["txid"], "amount_zat": FAUCET_ZAT, "text": "Received free test coins" }));
                    Ok(json!({ "txid": r["txid"], "amount_zat": FAUCET_ZAT }))
                }
                Err(e) if maybe_sent(&e) => {
                    // It may have gone out: keep the wallet marked, and say to look again.
                    house.push_event(json!({ "event": "log", "text": format!("faucet, maybe sent: {e:#}") }));
                    Err(anyhow!("the faucet's payment may still arrive; sync in a few minutes and look again"))
                }
                Err(e) => {
                    // Give the chance back; the detail goes to the house log, not to the visitor.
                    hub.unfund(&profile, &ip, stamp);
                    house.push_event(json!({ "event": "log", "text": format!("faucet: {e:#}") }));
                    Err(anyhow!("the faucet cannot pay right now (its coins may still be confirming); try again in a few minutes"))
                }
            };
            hub.faucet_settled(&profile);
            r
        })
    }

    /// The faucet payment of a wallet ended, one way or the other.
    fn faucet_settled(&self, profile: &str) {
        if let Mode::Hosted(h) = &self.mode {
            if h.index.lock().unwrap().faucet_pending.remove(profile) {
                let _ = self.save_index(h);
            }
        }
    }

    /// After a restart: every wallet whose faucet payment was under way is looked at. One
    /// that holds a note was paid; one that holds nothing gets its chance back.
    fn reconcile_faucet(&self, h: &Hosted) {
        let pending: Vec<String> = h
            .index
            .lock()
            .unwrap()
            .faucet_pending
            .iter()
            .cloned()
            .collect();
        for profile in pending {
            let app = h.apps.lock().unwrap().get(&profile).cloned();
            let Some(a) = app else {
                h.index.lock().unwrap().faucet_pending.remove(&profile);
                let _ = self.save_index(h);
                continue;
            };
            // A payment still in the mempool is not a note yet: a wallet is judged only once
            // it is an hour old and the look at the chain worked; until then it stays pending
            // and is looked at again a minute later.
            let synced = a.exclusive(|| a.sync()).is_ok();
            let has_note = a
                .accounts()
                .and_then(|(payout, _)| a.notes(&payout))
                .map(|n| !n.is_empty())
                .unwrap_or(true);
            let made = fs::metadata(a.dir.join("app.json"))
                .and_then(|m| m.created().or_else(|_| m.modified()))
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(u64::MAX);
            let mut idx = h.index.lock().unwrap();
            if has_note {
                idx.faucet_pending.remove(&profile);
            } else if synced && made.saturating_add(3600) < now() {
                idx.faucet_pending.remove(&profile);
                idx.funded.remove(&profile);
                eprintln!(
                    "faucet: {profile} was marked paid but holds nothing; its chance is given back"
                );
            }
            drop(idx);
            let _ = self.save_index(h);
        }
    }

    /// Give back a faucet chance: the wallet's, the visitor's and the hour's.
    fn unfund(&self, profile: &str, ip: &str, stamp: u64) {
        if let Mode::Hosted(h) = &self.mode {
            {
                let mut idx = h.index.lock().unwrap();
                idx.funded.remove(profile);
                idx.faucet_pending.remove(profile);
            }
            let _ = self.save_index(h);
            if let Some(v) = h.faucet_by_ip.lock().unwrap().get_mut(ip) {
                if let Some(i) = v.iter().position(|t| *t == stamp) {
                    v.remove(i);
                }
            }
            let mut all = h.faucet_all.lock().unwrap();
            if let Some(i) = all.iter().position(|t| *t == stamp) {
                all.remove(i);
            }
        }
    }

    /// Remove wallets made and never used: no test coins, nothing in the activity log, nothing
    /// listed, no shop, no one signed in, older than a day, and (a last look at the wallet
    /// itself) no note in either account.
    fn remove_unused(&self, h: &Hosted) {
        let apps: Vec<Arc<App>> = h.apps.lock().unwrap().values().cloned().collect();
        let funded = h.index.lock().unwrap().funded.clone();
        let signed_in: HashSet<String> = h
            .sessions
            .lock()
            .unwrap()
            .values()
            .map(|s| s.profile.clone())
            .collect();
        for a in apps {
            let made = fs::metadata(a.dir.join("app.json"))
                .and_then(|m| m.created().or_else(|_| m.modified()))
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(u64::MAX);
            let unused = a.profile != HOUSE
                && made.saturating_add(UNUSED_WALLET_SECS) < now()
                && a.last_seen.load(Ordering::SeqCst) + UNUSED_WALLET_SECS < now()
                && !funded.contains(&a.profile)
                && !signed_in.contains(&a.profile)
                && !a.listings_path().exists()
                && !a.shop_running()
                && a.cfg
                    .lock()
                    .unwrap()
                    .activity
                    .iter()
                    .all(|e| e["kind"] == "wallet");
            if !unused {
                continue;
            }
            // Only under the wallet's lock, so no job of it is running.
            let Ok(_g) = a.wallet_lock.try_lock() else {
                continue;
            };
            // Coins sent from elsewhere leave no activity entry, but they do leave notes:
            // look at the chain once more before deciding, and keep the wallet when that
            // look fails.
            if a.sync().is_err() {
                continue;
            }
            let holds_something = a.accounts().ok().is_none_or(|(payout, item)| {
                [payout, item]
                    .iter()
                    .any(|acct| a.notes(acct).map(|n| !n.is_empty()).unwrap_or(true))
            });
            if holds_something {
                continue;
            }
            if a.jobs
                .lock()
                .unwrap()
                .values()
                .any(|j| j.state == "running")
            {
                continue;
            }
            h.apps.lock().unwrap().remove(&a.profile);
            {
                let mut idx = h.index.lock().unwrap();
                idx.by_cred.retain(|_, p| p != &a.profile);
                idx.ports.remove(&a.profile);
            }
            let _ = self.save_index(h);
            let _ = fs::remove_dir_all(&a.dir);
            eprintln!("removed the unused wallet {}", a.profile);
        }
    }

    fn open_house(&self, pass: &str) -> Result<()> {
        let Mode::Hosted(h) = &self.mode else {
            return Ok(());
        };
        let existing = h.apps.lock().unwrap().get(HOUSE).cloned();
        let app = match existing {
            Some(a) => a,
            None => {
                let app = self.new_app(h, HOUSE)?;
                app.create_wallet(&json!({ "network": h.network, "passphrase": pass }))?;
                h.apps.lock().unwrap().insert(HOUSE.into(), app.clone());
                eprintln!(
                    "house wallet created; fund it at {}",
                    app.address()?["address"].as_str().unwrap_or("?")
                );
                app
            }
        };
        app.unlock(&json!({ "passphrase": pass }))?;
        if app.listings_path().exists() {
            let _ = app.shop_start(&json!({}));
        }
        Ok(())
    }
}

fn start_hosted_threads(hub: &Arc<Hub>) {
    // Faucet payments cut short by the last stop are looked at once, first.
    let hb = hub.clone();
    thread::spawn(move || {
        if let Mode::Hosted(h) = &hb.mode {
            hb.reconcile_faucet(h);
        }
    });
    // Lock idle wallets and drop expired sessions.
    let hb = hub.clone();
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(60));
        let Mode::Hosted(h) = &hb.mode else { return };
        h.sessions.lock().unwrap().retain(|_, s| s.expires > now());
        h.prune();
        let apps: Vec<Arc<App>> = h.apps.lock().unwrap().values().cloned().collect();
        for a in apps {
            if a.profile != HOUSE && a.last_seen.load(Ordering::SeqCst) + IDLE_LOCK_SECS < now() {
                *a.passphrase.lock().unwrap() = None;
            }
            a.prune_trade_files();
        }
        hb.remove_unused(h);
        hb.reconcile_faucet(h);
    });
    // Keep the house shop stocked and open.
    let hb = hub.clone();
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(45));
        let Mode::Hosted(h) = &hb.mode else { return };
        let house = h.apps.lock().unwrap().get(HOUSE).cloned();
        if let Some(house) = house {
            if let Err(e) = house.restock() {
                house.push_event(json!({ "event": "log", "text": format!("restock: {e:#}") }));
            }
        }
    });
}

// ---------------------------------------------------------------- one profile

impl App {
    fn open(
        dir: &Path,
        profile: &str,
        bins: &Bins,
        public_shop: Option<String>,
    ) -> Result<Arc<App>> {
        for sub in ["trades", "inbox", "outbox"] {
            fs::create_dir_all(dir.join(sub))
                .with_context(|| format!("creating {}", dir.join(sub).display()))?;
        }
        owner_only_dir(dir)?;
        let cfg: Config = read_state(&dir.join("app.json"))?;
        let last_sync = cfg.last_sync.clone().unwrap_or(Value::Null);
        Ok(Arc::new(App {
            dir: dir.to_path_buf(),
            profile: profile.to_string(),
            buyer_bin: bins.buyer.clone(),
            seller_bin: bins.seller.clone(),
            public_shop,
            passphrase: Mutex::new(None),
            cfg: Mutex::new(cfg),
            jobs: Mutex::new(HashMap::new()),
            next_job: AtomicU64::new(0),
            wallet_lock: Mutex::new(()),
            shop: Mutex::new(Shop {
                child: None,
                listen: None,
            }),
            paused: AtomicBool::new(false),
            events: Arc::new(Mutex::new(Vec::new())),
            last_sync: Mutex::new(last_sync),
            last_seen: AtomicU64::new(now()),
            proof_checks: Mutex::new(Vec::new()),
        }))
    }

    /// No server paths in what a visitor sees.
    fn scrub_text(&self, text: &str) -> String {
        let mut paths = vec![self.dir.display().to_string()];
        for p in [self.dir.parent(), self.buyer_bin.parent()]
            .into_iter()
            .flatten()
        {
            paths.push(p.display().to_string());
        }
        // The longest first, so a directory is removed before its parent.
        paths.sort_by_key(|p| std::cmp::Reverse(p.len()));
        let mut t = text.to_string();
        for p in paths.iter().filter(|p| p.len() > 1) {
            t = t.replace(p, "");
        }
        t
    }

    /// Files a trade or a check leaves in `trades/` are kept an hour at most.
    fn prune_trade_files(&self) {
        let Ok(entries) = fs::read_dir(self.dir.join("trades")) else {
            return;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let old = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|d| d.as_secs() > 3600);
            if old && (name.starts_with("proof-") || name.starts_with("faucet-")) {
                let _ = fs::remove_file(e.path());
            }
        }
    }

    fn hosted(&self) -> bool {
        self.public_shop.is_some()
    }

    /// The shop and listings URLs for what the visitor typed. A hosted server talks to its
    /// own shops only, so a visitor cannot make it fetch or post anywhere else.
    fn shop_urls(&self, input: &str) -> Result<(String, String)> {
        let (base, url) = seller_urls(input)?;
        if let Some(own) = &self.public_shop {
            let prefix = own
                .rsplit_once("/shop/")
                .map(|(o, _)| format!("{o}/shop/"))
                .unwrap_or_default();
            let ok = base.strip_prefix(&prefix).is_some_and(valid_name);
            if !ok {
                return Err(anyhow!("on this server you can open its own shops only"));
            }
        }
        Ok((base, url))
    }

    fn wallet_dir(&self) -> PathBuf {
        self.dir.join("wallet")
    }

    fn listings_path(&self) -> PathBuf {
        self.dir.join("listings.json")
    }

    fn w(&self) -> String {
        self.wallet_dir().display().to_string()
    }

    fn save_cfg(&self) -> Result<()> {
        // Written while the lock is held, so two saves never mix.
        let cfg = self.cfg.lock().unwrap();
        zsettle_core::wallet::write_owner_only(
            &self.dir.join("app.json"),
            &serde_json::to_vec_pretty(&*cfg)?,
        )
    }

    fn accounts(&self) -> Result<(String, String)> {
        let cfg = self.cfg.lock().unwrap();
        match (&cfg.payout, &cfg.item) {
            (Some(p), Some(i)) => Ok((p.clone(), i.clone())),
            _ => Err(anyhow!("create a wallet first")),
        }
    }

    fn require_unlocked(&self) -> Result<()> {
        if self.passphrase.lock().unwrap().is_none() {
            return Err(anyhow!("the wallet is locked"));
        }
        Ok(())
    }

    /// A wallet opened by a passkey confirms each transaction with the passkey again: the
    /// page derives the secret once more and sends it; it must be the one that opened the
    /// wallet. A passphrase wallet confirms with its confirm step alone.
    fn check_confirm(&self, body: &Value) -> Result<()> {
        if self.cfg.lock().unwrap().passkey.is_none() {
            return Ok(());
        }
        let given = body
            .get("confirm")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let held = self.passphrase.lock().unwrap();
        let same = held.as_ref().is_some_and(|p| {
            p.len() == given.len()
                && p.bytes()
                    .zip(given.bytes())
                    .fold(0u8, |acc, (x, y)| acc | (x ^ y))
                    == 0
        });
        if !same {
            return Err(anyhow!("confirm with your passkey first"));
        }
        Ok(())
    }

    /// Run one command and return the JSON it printed, or its text.
    fn run(&self, bin: &Path, args: &[String]) -> Result<Value> {
        let mut cmd = Command::new(bin);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for v in ENV_VARS {
            cmd.env_remove(v);
        }
        if let Some(p) = self.passphrase.lock().unwrap().as_ref() {
            cmd.env("ZSETTLE_PASSPHRASE", p.as_str());
        }
        // Commands that open the seed and prove take a few hundred MB each: a few at a time.
        let heavy = matches!(
            args.first().map(String::as_str),
            Some("init" | "pay" | "mint" | "publish" | "cancel" | "buy" | "prove" | "rotate")
        );
        let _slot = if heavy {
            Some(
                Slot::take(&HEAVY_RUNNING, MAX_HEAVY, Duration::from_secs(300))
                    .ok_or_else(|| anyhow!("the server is busy; try again in a moment"))?,
            )
        } else {
            None
        };
        let name = bin
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let out = cmd.output().with_context(|| format!("running {name}"))?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let msg: Vec<&str> = stderr
                .lines()
                .map(str::trim)
                .filter(|l| {
                    !l.is_empty()
                        && !l.starts_with("warning:")
                        && !l.starts_with("note:")
                        && !l.starts_with('#')
                })
                .collect();
            let mut text = msg.join(" ").replace("Error: ", "");
            if self.hosted() {
                // No server paths in what a visitor sees.
                text = text.replace(&self.dir.display().to_string(), "");
            }
            return Err(anyhow!(if text.is_empty() {
                "the command failed".to_string()
            } else {
                text.chars().take(800).collect()
            }));
        }
        // One JSON document (some commands print it over several lines), or a last JSON
        // line after other output, or plain text.
        if let Ok(v) = serde_json::from_str::<Value>(stdout.trim()) {
            return Ok(v);
        }
        let last = stdout.lines().rev().find(|l| !l.trim().is_empty());
        Ok(last
            .and_then(|l| serde_json::from_str(l).ok())
            .unwrap_or_else(|| Value::String(stdout.to_string())))
    }

    /// Run a command that prints its JSON report whether or not it succeeds (exit status 1
    /// with a report means "checked, and the answer is no").
    fn run_report(&self, bin: &Path, args: &[String]) -> Result<Value> {
        let mut cmd = Command::new(bin);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for v in ENV_VARS {
            cmd.env_remove(v);
        }
        let out = cmd.output().context("running the check")?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        if let Ok(v) = serde_json::from_str::<Value>(stdout.trim()) {
            return Ok(v);
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        let mut text: String = stderr
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with("warning:"))
            .collect::<Vec<_>>()
            .join(" ")
            .replace("Error: ", "");
        if self.hosted() {
            text = text.replace(&self.dir.display().to_string(), "");
        }
        Err(anyhow!(if text.is_empty() {
            "the check failed".to_string()
        } else {
            text.chars().take(800).collect()
        }))
    }

    fn seller(&self, args: &[&str]) -> Result<Value> {
        self.run(
            &self.seller_bin,
            &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
    }

    fn buyer(&self, args: &[&str]) -> Result<Value> {
        self.run(
            &self.buyer_bin,
            &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
    }

    /// Unspent notes of an account.
    fn notes(&self, account: &str) -> Result<Vec<NoteRow>> {
        let w = self.w();
        let out = self.buyer(&["notes", "--wallet", &w, "--account", account])?;
        Ok(parse_notes(out.as_str().unwrap_or_default()))
    }

    fn add_activity(&self, mut entry: Value) {
        let mut cfg = self.cfg.lock().unwrap();
        entry["at"] = json!(now());
        cfg.activity.insert(0, entry);
        cfg.activity.truncate(MAX_ACTIVITY);
        drop(cfg);
        let _ = self.save_cfg();
    }

    // ------------------------------------------------------------ jobs

    /// Run `f` as a background job the page follows. When this wallet, or the server, runs
    /// too many already, the job is shown as failed at once.
    fn job(
        self: &Arc<Self>,
        kind: &str,
        f: impl FnOnce(&Arc<App>, u64) -> Result<Value> + Send + 'static,
    ) -> Value {
        match self.start_job(kind, true) {
            Ok((id, slot)) => self.run_job(id, slot, f),
            Err(e) => {
                let id = self.next_job.fetch_add(1, Ordering::SeqCst) + 1;
                self.jobs.lock().unwrap().insert(
                    id,
                    Job {
                        id,
                        kind: kind.to_string(),
                        state: "failed".into(),
                        steps: Vec::new(),
                        result: Value::Null,
                        error: Some(e.to_string()),
                    },
                );
                json!({ "job": id })
            }
        }
    }

    /// A job outside the limits, for work that has a limit of its own (the faucet).
    fn free_job(
        self: &Arc<Self>,
        kind: &str,
        f: impl FnOnce(&Arc<App>, u64) -> Result<Value> + Send + 'static,
    ) -> Result<Value> {
        let (id, slot) = self.start_job(kind, false)?;
        Ok(self.run_job(id, slot, f))
    }

    /// Register a running job; the check and the count happen under one lock, so parallel
    /// requests cannot pass the limits together.
    fn start_job(&self, kind: &str, counted: bool) -> Result<(u64, Option<Slot>)> {
        let mut jobs = self.jobs.lock().unwrap();
        let slot = if counted {
            let running_here = jobs
                .values()
                .filter(|j| j.state == "running" && j.kind != "faucet")
                .count();
            if running_here >= MAX_JOBS_PER_WALLET {
                return Err(anyhow!(
                    "another task of this wallet is still running; wait for it to finish"
                ));
            }
            Some(
                Slot::try_take(&RUNNING_JOBS, MAX_JOBS_TOTAL)
                    .ok_or_else(|| anyhow!("the server is busy; try again in a moment"))?,
            )
        } else {
            None
        };
        let id = self.next_job.fetch_add(1, Ordering::SeqCst) + 1;
        // Keep the last hundred jobs.
        if jobs.len() > 100 {
            let keep: HashSet<u64> = (id.saturating_sub(100)..=id).collect();
            jobs.retain(|k, j| keep.contains(k) || j.state == "running");
        }
        jobs.insert(
            id,
            Job {
                id,
                kind: kind.to_string(),
                state: "running".into(),
                steps: Vec::new(),
                result: Value::Null,
                error: None,
            },
        );
        Ok((id, slot))
    }

    fn run_job(
        self: &Arc<Self>,
        id: u64,
        slot: Option<Slot>,
        f: impl FnOnce(&Arc<App>, u64) -> Result<Value> + Send + 'static,
    ) -> Value {
        let app = self.clone();
        thread::spawn(move || {
            let r = f(&app, id);
            drop(slot);
            let mut jobs = app.jobs.lock().unwrap();
            if let Some(j) = jobs.get_mut(&id) {
                match r {
                    Ok(v) => {
                        j.state = "done".into();
                        j.result = v;
                    }
                    Err(e) => {
                        j.state = "failed".into();
                        j.error = Some(app.scrub_text(&format!("{e:#}")));
                    }
                }
            }
        });
        json!({ "job": id })
    }

    fn step(&self, id: u64, text: &str) {
        if let Some(j) = self.jobs.lock().unwrap().get_mut(&id) {
            j.steps.push(text.to_string());
        }
    }

    fn step_opt(&self, id: Option<u64>, text: &str) {
        if let Some(id) = id {
            self.step(id, text);
        }
    }

    /// Run `f` alone on the wallet: the shop is paused meanwhile, since it syncs the same
    /// database every few seconds.
    fn exclusive<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        let _guard = self.wallet_lock.lock().unwrap();
        self.paused.store(true, Ordering::SeqCst);
        let was = self.stop_shop();
        let r = f();
        // The shop restarts only with the passphrase it needs; a wallet locked meanwhile keeps
        // its shop closed.
        if let Some(listen) = was.filter(|_| self.passphrase.lock().unwrap().is_some()) {
            if let Err(e) = self.spawn_shop(&listen) {
                self.push_event(
                    json!({ "event": "error", "error": format!("restarting the shop: {e:#}") }),
                );
            }
        }
        self.paused.store(false, Ordering::SeqCst);
        r
    }

    fn sync(&self) -> Result<Value> {
        let w = self.w();
        let r = self.seller(&["sync", "--wallet", &w])?;
        self.record_sync(json!({ "report": r, "at": now() }));
        Ok(r)
    }

    fn record_sync(&self, v: Value) {
        *self.last_sync.lock().unwrap() = v.clone();
        self.cfg.lock().unwrap().last_sync = Some(v);
        let _ = self.save_cfg();
    }

    /// Copy the shop's sales into the activity log, once each.
    fn absorb_sales(&self) {
        let sold: Vec<Value> = self
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e["event"] == "sold")
            .cloned()
            .collect();
        let mut added = false;
        {
            let mut cfg = self.cfg.lock().unwrap();
            for e in sold {
                let known = cfg
                    .activity
                    .iter()
                    .any(|a| a["kind"] == "sale" && a["txid"] == e["txid"]);
                if known {
                    continue;
                }
                let name = e["name"]
                    .as_str()
                    .filter(|n| !n.is_empty())
                    .unwrap_or("an item");
                cfg.activity.insert(
                    0,
                    json!({ "kind": "sale", "txid": e["txid"], "price_zat": e["price_zat"], "text": format!("Sold {name}"), "at": e["at"] }),
                );
                added = true;
            }
            cfg.activity.truncate(MAX_ACTIVITY);
        }
        if added {
            let _ = self.save_cfg();
        }
    }

    // ------------------------------------------------------------ wallet

    fn status(&self) -> Result<Value> {
        self.absorb_sales();
        let cfg = self.cfg.lock().unwrap().clone();
        let running = self.shop_running();
        Ok(json!({
            "profile": self.profile,
            "wallet": self.wallet_dir().join("keys.toml").exists(),
            "unlocked": self.passphrase.lock().unwrap().is_some(),
            "network": cfg.network,
            "payout": cfg.payout,
            "item": cfg.item,
            "passkey": cfg.passkey,
            "sellers": cfg.sellers,
            "shop": { "running": running, "listen": cfg.shop_listen, "public": self.public_shop },
            "last_sync": *self.last_sync.lock().unwrap(),
            "has_listings": self.listings_path().exists(),
        }))
    }

    fn create_wallet(&self, body: &Value) -> Result<Value> {
        if self.wallet_dir().join("keys.toml").exists() {
            return Err(anyhow!("this profile already has a wallet"));
        }
        let network = field(body, "network")?;
        if network != "main" && network != "test" {
            return Err(anyhow!("network is main or test"));
        }
        let pass = field(body, "passphrase")?;
        if pass.len() < 10 {
            return Err(anyhow!("use a passphrase of at least ten characters"));
        }
        *self.passphrase.lock().unwrap() = Some(Zeroizing::new(pass));
        let w = self.w();
        let created = match self.seller(&["init", "--wallet", &w, "--network", &network]) {
            Ok(v) => v,
            Err(e) => {
                *self.passphrase.lock().unwrap() = None;
                let _ = fs::remove_dir_all(self.wallet_dir());
                return Err(e);
            }
        };
        let acct = |i: usize| {
            created["accounts"][i]["account"]
                .as_str()
                .map(str::to_string)
        };
        {
            let mut cfg = self.cfg.lock().unwrap();
            cfg.network = Some(network);
            cfg.payout = acct(0);
            cfg.item = acct(1);
            cfg.passkey = body.get("passkey").cloned().filter(|v| v.is_object());
        }
        self.save_cfg()?;
        self.add_activity(json!({ "kind": "wallet", "text": "Wallet created" }));
        Ok(json!({ "fund": created["fund"], "birthday": created["birthday"] }))
    }

    fn unlock(&self, body: &Value) -> Result<Value> {
        let pass = field(body, "passphrase")?;
        if pass.len() > 512 {
            return Err(anyhow!("wrong passphrase"));
        }
        Wallet::open(self.wallet_dir())?.check_passphrase(&pass)?;
        *self.passphrase.lock().unwrap() = Some(Zeroizing::new(pass));
        Ok(json!({ "unlocked": true }))
    }

    /// The recovery words, for a passphrase given again (or a passkey asked again).
    fn backup(&self, body: &Value) -> Result<Value> {
        let pass = field(body, "passphrase")?;
        if pass.len() > 512 {
            return Err(anyhow!("wrong passphrase"));
        }
        let _slot = scrypt_slot()?;
        let words = Wallet::open(self.wallet_dir())?.mnemonic_phrase(&pass)?;
        Ok(json!({ "words": words.split_whitespace().collect::<Vec<_>>() }))
    }

    fn wallet_view(&self) -> Result<Value> {
        let (payout, item) = self.accounts()?;
        let names = self.item_names();
        let zec = self.notes(&payout)?;
        let items = self.notes(&item)?;
        let own = Listings::read(&self.listings_path()).ok();
        let listed: HashMap<String, u64> = own
            .as_ref()
            .map(|l| {
                l.items
                    .iter()
                    .map(|i| (i.note.clone(), i.price_zat))
                    .collect()
            })
            .unwrap_or_default();
        let details = self.item_details(own.as_ref());
        Ok(json!({
            "balance_zat": zec.iter().map(|n| n.1).sum::<u64>(),
            "notes": zec.iter().map(|(n, v, m, c)| json!({ "note": n, "value_zat": v, "mined": m, "change": c })).collect::<Vec<_>>(),
            "items": items.iter().map(|(n, v, m, _)| json!({
                "note": n, "value_zat": v, "mined": m,
                "name": names.get(n), "listed_zat": listed.get(n),
                "icon": details.get(n).and_then(|d| d["icon"].as_str()),
                "description": details.get(n).and_then(|d| d["description"].as_str()),
            })).collect::<Vec<_>>(),
        }))
    }

    /// Icon and description per item note: from this wallet's own listings, else as the seller
    /// listed an item bought.
    fn item_details(&self, own: Option<&Listings>) -> HashMap<String, Value> {
        let mut d = self.cfg.lock().unwrap().details.clone();
        if let Some(l) = own {
            for i in &l.items {
                if i.icon.is_some() || i.description.is_some() {
                    d.insert(
                        i.note.clone(),
                        json!({ "icon": i.icon, "description": i.description }),
                    );
                }
            }
        }
        d
    }

    fn item_names(&self) -> HashMap<String, String> {
        let mut names = self.cfg.lock().unwrap().names.clone();
        if let Ok(l) = Listings::read(&self.listings_path()) {
            for i in l.items {
                if !i.name.is_empty() {
                    names.entry(i.note).or_insert(i.name);
                }
            }
        }
        names
    }

    fn address(&self) -> Result<Value> {
        let (payout, _) = self.accounts()?;
        let w = self.w();
        self.seller(&["address", "--wallet", &w, "--account", &payout])
    }

    fn start_sync(self: &Arc<Self>) -> Result<Value> {
        self.accounts()?;
        if self.shop_running() {
            // The open shop syncs the same wallet every few seconds; pausing it would only
            // leave a gap in its service.
            return Ok(self.job("sync", |app, id| {
                app.step(id, "The open shop keeps the wallet in sync");
                // The shop syncs this wallet every few seconds: read the height it reached,
                // so the page can count confirmations.
                let w = app.w();
                let mut report = app
                    .buyer(&["height", "--wallet", &w])
                    .unwrap_or_else(|_| json!({}));
                report["by"] = json!("shop");
                app.record_sync(json!({ "report": report, "at": now() }));
                app.rotate_expired(&report);
                Ok(report)
            }));
        }
        Ok(self.job("sync", |app, id| {
            app.step(id, "Syncing with the network");
            let r = app.exclusive(|| app.sync())?;
            app.rotate_expired(&r);
            Ok(r)
        }))
    }

    /// A note reserved for an offer that can no longer be mined is retired (or released, if
    /// spent), so the wallet can buy again and the seller cannot link its next offer. Runs
    /// after a sync, only when there is such a note and the wallet is unlocked.
    fn rotate_expired(&self, report: &Value) {
        // Judged against the height the wallet scanned to, as `rotate` judges it, so a
        // backlog does not start rotations that then find nothing to do.
        let Some(tip) = report["scanned_to"]
            .as_u64()
            .or(report["chain_tip"].as_u64())
        else {
            return;
        };
        if self.passphrase.lock().unwrap().is_none() {
            return;
        }
        let path = PathBuf::from(self.w()).join("reserved-notes.json");
        let Ok(text) = fs::read_to_string(&path) else {
            return;
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            return;
        };
        let expired = v["notes"].as_array().is_some_and(|n| {
            n.iter()
                .any(|x| x["expiry_height"].as_u64().is_some_and(|e| e <= tip))
        });
        if !expired {
            return;
        }
        let Ok((payout, _)) = self.accounts() else {
            return;
        };
        let w = self.w();
        match self.exclusive(|| {
            self.buyer(&["rotate", "--wallet", &w, "--account", &payout, "--no-sync"])
        }) {
            Ok(r) if r["rotated"].as_u64().unwrap_or(0) > 0 => {
                self.add_activity(json!({ "kind": "rotate", "text": "Coins moved to a fresh note after an offer that was not settled" }));
            }
            Ok(_) => {}
            Err(e) => self.push_event(json!({ "event": "log", "text": format!("rotate: {e:#}") })),
        }
    }

    /// After an offer that will never settle: drop the note's reservation, or, when the
    /// seller saw the offer (and so the note's nullifier), move the note to a fresh one.
    fn free_note(&self, w: &str, payout: &str, note: &str, retire: bool) -> bool {
        if note.is_empty() {
            return false;
        }
        let flag = if retire {
            format!("--abandon={note}")
        } else {
            format!("--release={note}")
        };
        match self.exclusive(|| self.buyer(&["rotate", "--wallet", w, "--account", payout, &flag]))
        {
            Ok(r) if r["rotated"].as_u64().unwrap_or(0) > 0 => {
                self.add_activity(json!({ "kind": "rotate", "text": "Coins moved to a fresh note after a failed offer" }));
                true
            }
            Ok(_) => true,
            Err(e) => {
                self.push_event(
                    json!({ "event": "log", "text": format!("freeing the note: {e:#}") }),
                );
                false
            }
        }
    }

    fn start_pay(self: &Arc<Self>, body: &Value) -> Result<Value> {
        self.require_unlocked()?;
        self.check_confirm(body)?;
        let (payout, _) = self.accounts()?;
        let (to, amount) = (field(body, "to")?, field(body, "amount")?);
        let memo = body
            .get("memo")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|m| !m.trim().is_empty());
        Ok(self.job("pay", move |app, id| {
            app.exclusive(|| {
                let w = app.w();
                app.step(id, "Syncing with the network");
                app.sync()?;
                app.step(id, "Building, proving and sending the payment");
                let (to_a, amount_a) = (format!("--to={to}"), format!("--amount={amount}"));
                let memo_a = memo.as_deref().map(|m| format!("--memo={m}"));
                let mut args = vec!["pay", "--wallet", &w, "--account", &payout, &to_a, &amount_a];
                if let Some(m) = memo_a.as_deref() {
                    args.push(m);
                }
                let r = app.seller(&args)?;
                app.add_activity(json!({ "kind": "pay", "txid": r["txid"], "amount_zat": r["amount_zat"], "text": "Sent ZEC" }));
                Ok(r)
            })
        }))
    }

    /// The house wallet pays test coins (zatoshis) to an address.
    fn faucet_pay(&self, to: &str, zat: u64) -> Result<Value> {
        self.require_unlocked()?;
        let (payout, _) = self.accounts()?;
        self.exclusive(|| {
            let w = self.w();
            if !self.shop_running() {
                self.sync()?;
            }
            let amount = zat.to_string();
            let to_a = format!("--to={to}");
            let amount_a = format!("--amount={amount}");
            // The raw transaction is written before the broadcast: if the paying process is
            // lost (killed, or the server stopped) after the transaction was made, the file
            // says so, and the payment is sent again instead of made again.
            let tx_file = self.dir.join("trades").join(format!(
                "faucet-{}.tx",
                hex::encode(rand::random::<[u8; 4]>())
            ));
            let tx_out = format!("--tx-out={}", tx_file.display());
            let paid = self.seller(&[
                "pay", "--wallet", &w, "--account", &payout, &to_a, &amount_a,
                "--memo=zsettle: free test coins", &tx_out,
            ]);
            let txid_file = PathBuf::from(format!("{}.txid", tx_file.display()));
            let r = match paid {
                Ok(r) => Ok(r),
                Err(e) if txid_file.exists() => {
                    let txid = fs::read_to_string(&txid_file).unwrap_or_default().trim().to_string();
                    let network = self.cfg.lock().unwrap().network.clone().unwrap_or_else(|| "main".into());
                    let tx_a = format!("--tx={}", tx_file.display());
                    let net_a = format!("--network={network}");
                    match self.buyer(&["broadcast", &tx_a, &net_a]) {
                        Ok(_) => Ok(json!({ "txid": txid, "resent": true })),
                        // "already exists in mempool", "already in block chain": the first
                        // broadcast did go through.
                        Err(e2) if format!("{e2:#}").contains("already") => {
                            Ok(json!({ "txid": txid, "resent": false }))
                        }
                        // The node refused it outright: nothing was sent, the chance goes back.
                        Err(e2) if format!("{e2:#}").contains("server rejected transaction") => {
                            Err(anyhow!("{e:#}; resend refused: {e2:#}"))
                        }
                        Err(e2) => Err(anyhow!(
                            "send_transaction: the payment {txid} was made but its broadcast is uncertain ({e:#}; resend: {e2:#})"
                        )),
                    }
                }
                Err(e) => Err(e),
            };
            let _ = fs::remove_file(&tx_file);
            let _ = fs::remove_file(&txid_file);
            let r = r?;
            self.add_activity(json!({ "kind": "pay", "txid": r["txid"], "amount_zat": zat, "text": "Faucet payment" }));
            Ok(r)
        })
    }

    // ------------------------------------------------------------ selling

    fn wallet_args(&self) -> Result<Vec<String>> {
        let (payout, item) = self.accounts()?;
        Ok(vec![
            "--wallet".into(),
            self.w(),
            "--item-account".into(),
            item,
            "--item-account-index".into(),
            "1".into(),
            "--payout-account".into(),
            payout,
        ])
    }

    fn start_mint(self: &Arc<Self>, body: &Value) -> Result<Value> {
        self.require_unlocked()?;
        self.check_confirm(body)?;
        let names: Vec<String> = body
            .get("names")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        if names.is_empty() || names.len() > 20 {
            return Err(anyhow!("give between one and twenty item names"));
        }
        if names.iter().any(|n| n.len() > 80) {
            return Err(anyhow!("an item name has at most 80 characters"));
        }
        self.accounts()?;
        Ok(self.job("mint", move |app, id| app.mint_now(Some(id), &names)))
    }

    fn mint_now(&self, id: Option<u64>, names: &[String]) -> Result<Value> {
        let wa = self.wallet_args()?;
        self.exclusive(|| {
            self.step_opt(id, "Syncing with the network");
            self.sync()?;
            self.step_opt(id, "Minting the items in one shielded transaction");
            let mut args = vec!["mint".to_string()];
            args.extend(wa);
            for n in names {
                args.push(format!("--name={n}"));
            }
            let r = self.run(&self.seller_bin, &args)?;
            {
                let mut cfg = self.cfg.lock().unwrap();
                cfg.minted += names.len() as u64;
                if let Some(notes) = r["notes"].as_array() {
                    for (note, name) in notes.iter().zip(names) {
                        if let Some(n) = note.as_str() {
                            cfg.names.insert(n.to_string(), name.clone());
                        }
                    }
                }
            }
            self.save_cfg()?;
            self.add_activity(json!({ "kind": "mint", "txid": r["txid"], "text": format!("Minted {}", names.join(", ")) }));
            Ok(r)
        })
    }

    fn own_listings(&self) -> Result<Value> {
        if !self.listings_path().exists() {
            return Ok(json!({ "items": [] }));
        }
        Ok(serde_json::from_slice(&fs::read(self.listings_path())?)?)
    }

    fn start_publish(self: &Arc<Self>, body: &Value) -> Result<Value> {
        self.require_unlocked()?;
        let note = note_field(body, "note")?;
        let price = field(body, "price")?;
        let description = opt_text(body, "description");
        let icon = opt_text(body, "icon");
        zsettle_core::listing::check_details(description.as_deref(), icon.as_deref())?;
        let name = body
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|n| !n.trim().is_empty())
            .or_else(|| self.item_names().get(&note).cloned())
            .unwrap_or_default();
        if name.chars().count() > 80 {
            return Err(anyhow!("a name has at most 80 characters"));
        }
        self.accounts()?;
        Ok(self.job("publish", move |app, id| {
            app.publish_now(
                Some(id),
                &note,
                &name,
                &price,
                description.as_deref(),
                icon.as_deref(),
            )
        }))
    }

    fn publish_now(
        &self,
        id: Option<u64>,
        note: &str,
        name: &str,
        price: &str,
        description: Option<&str>,
        icon: Option<&str>,
    ) -> Result<Value> {
        self.exclusive(|| {
            self.step_opt(id, "Syncing with the network");
            self.sync()?;
            self.publish_one(id, note, name, price, description, icon)
        })
    }

    /// Add one item to the signed listings file; the caller holds the wallet.
    fn publish_one(
        &self,
        id: Option<u64>,
        note: &str,
        name: &str,
        price: &str,
        description: Option<&str>,
        icon: Option<&str>,
    ) -> Result<Value> {
        let wa = self.wallet_args()?;
        let listings = self.listings_path().display().to_string();
        self.step_opt(id, "Signing the listings file");
        let mut args = vec!["publish".to_string()];
        args.extend(wa);
        args.extend([
            format!("--listings={listings}"),
            format!("--item-note={note}"),
            format!("--price={price}"),
        ]);
        if !name.is_empty() {
            args.push(format!("--name={name}"));
        }
        if let Some(d) = description {
            args.push(format!("--description={d}"));
        }
        if let Some(i) = icon {
            args.push(format!("--icon={i}"));
        }
        let r = self.run(&self.seller_bin, &args)?;
        self.add_activity(json!({ "kind": "list", "text": format!("Listed {} for {price}", if name.is_empty() { note } else { name }) }));
        Ok(r)
    }

    fn start_delist(self: &Arc<Self>, body: &Value) -> Result<Value> {
        self.require_unlocked()?;
        let note = note_field(body, "note")?;
        let wa = self.wallet_args()?;
        let listings = self.listings_path().display().to_string();
        Ok(self.job("delist", move |app, id| {
            app.exclusive(|| {
                app.step(id, "Removing the item from the listings file");
                let mut args = vec!["publish".to_string()];
                args.extend(wa);
                args.extend([format!("--listings={listings}"), format!("--remove={note}")]);
                let r = app.run(&app.seller_bin, &args)?;
                app.add_activity(json!({ "kind": "delist", "text": format!("Delisted {note}") }));
                Ok(r)
            })
        }))
    }

    fn start_cancel(self: &Arc<Self>, body: &Value) -> Result<Value> {
        self.require_unlocked()?;
        self.check_confirm(body)?;
        let note = note_field(body, "note")?;
        let wa = self.wallet_args()?;
        let listings = self.listings_path();
        Ok(self.job("cancel", move |app, id| {
            app.exclusive(|| {
                app.step(id, "Moving the item to a fresh note, so no signed offer can settle");
                let mut args = vec!["cancel".to_string()];
                args.extend(wa);
                args.push(format!("--item-note={note}"));
                if listings.exists() {
                    args.extend(["--listings".into(), listings.display().to_string()]);
                }
                let r = app.run(&app.seller_bin, &args)?;
                app.add_activity(json!({ "kind": "cancel", "txid": r["txid"], "text": format!("Cancelled the offers on {note}") }));
                Ok(r)
            })
        }))
    }

    /// Prove that this wallet holds an item right now, answering a checker's challenge.
    fn start_prove(self: &Arc<Self>, body: &Value) -> Result<Value> {
        self.require_unlocked()?;
        let note = note_field(body, "note")?;
        // Both optional: a label signed into the proof, and a checker's code for a live check.
        let challenge = opt_text(body, "challenge");
        if let Some(c) = &challenge {
            zsettle_core::ownership::check_challenge(c)?;
        }
        let label = opt_text(body, "label");
        if let Some(l) = &label {
            zsettle_core::ownership::check_label(l)?;
        }
        let (_, item) = self.accounts()?;
        Ok(self.job("prove", move |app, id| {
            app.exclusive(|| {
                app.step(id, "Syncing with the network");
                app.step(id, "Signing the proof with the item's key");
                let mut args = vec![
                    "prove".to_string(),
                    "--wallet".into(),
                    app.w(),
                    "--item-account".into(),
                    item.clone(),
                    "--item-account-index".into(),
                    "1".into(),
                    format!("--item-note={note}"),
                ];
                if let Some(c) = &challenge {
                    args.push(format!("--challenge={c}"));
                }
                if let Some(l) = &label {
                    args.push(format!("--label={l}"));
                }
                let proof = app.run(&app.seller_bin, &args)?;
                app.add_activity(json!({ "kind": "prove", "text": format!("Made an ownership proof for {}", app.item_names().get(&note).cloned().unwrap_or_else(|| note.clone())) }));
                Ok(json!({ "proof": proof }))
            })
        }))
    }

    /// Check an ownership proof someone gave, against the challenge given them. The proof is
    /// read from a file: the app never fetches a proof from an address.
    fn start_check_proof(self: &Arc<Self>, body: &Value) -> Result<Value> {
        self.require_unlocked()?;
        let proof = field(body, "proof")?;
        if proof.len() > 16 * 1024 {
            return Err(anyhow!("that proof is too large"));
        }
        serde_json::from_str::<Value>(&proof)
            .map_err(|_| anyhow!("that is not an ownership proof"))?;
        // A checker's code only for a live check; a shared proof is checked without one.
        let challenge = opt_text(body, "challenge");
        if let Some(c) = &challenge {
            zsettle_core::ownership::check_challenge(c)?;
        }
        // A check reads the chain from the item's first block: a few a minute per wallet.
        {
            let mut t = self.proof_checks.lock().unwrap();
            t.retain(|x| *x + 60 > now());
            if t.len() >= MAX_PROOF_CHECKS_PER_WALLET_MIN {
                return Err(anyhow!(
                    "several proofs were checked this minute; wait a moment"
                ));
            }
            t.push(now());
        }
        let network = self
            .cfg
            .lock()
            .unwrap()
            .network
            .clone()
            .unwrap_or_else(|| "main".into());
        let path = self.dir.join("trades").join(format!(
            "proof-{}.json",
            hex::encode(rand::random::<[u8; 6]>())
        ));
        Ok(self.job("check-proof", move |app, id| {
            // Written here, so a job refused by the limits leaves no file behind.
            zsettle_core::wallet::write_owner_only(&path, proof.as_bytes())?;
            app.step(id, "Checking the signature and the block the proof names");
            app.step(id, "Looking on the chain whether the item is still held");
            let mut args = vec![
                "check-proof".to_string(),
                format!("--proof={}", path.display()),
                format!("--network={network}"),
            ];
            if let Some(c) = &challenge {
                args.push(format!("--challenge={c}"));
            }
            let r = app.run_report(&app.buyer_bin, &args);
            let _ = fs::remove_file(&path);
            r
        }))
    }

    /// The house shop: keep a few items listed, make new ones when they run low, stay open.
    fn restock(&self) -> Result<()> {
        if self.passphrase.lock().unwrap().is_none() {
            return Ok(());
        }
        let Ok(_probe) = self.wallet_lock.try_lock() else {
            return Ok(());
        };
        drop(_probe);
        if !self.shop_running() {
            self.exclusive(|| self.sync())?;
        }
        let w = self.wallet_view()?;
        let items = w["items"].as_array().cloned().unwrap_or_default();
        let listed = items.iter().filter(|i| !i["listed_zat"].is_null()).count();
        let ready: Vec<&Value> = items
            .iter()
            .filter(|i| i["listed_zat"].is_null() && !i["mined"].is_null())
            .collect();
        let pending = items.iter().filter(|i| i["mined"].is_null()).count();
        // All new listings in one pause of the shop, not one pause each. Items listed before
        // the catalogue existed (no icon yet) are listed again with their details.
        let mut batch: Vec<(String, String)> = ready
            .iter()
            .take(HOUSE_STOCK.saturating_sub(listed))
            .map(|it| {
                (
                    it["note"].as_str().unwrap_or_default().to_string(),
                    it["name"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        if let Ok(l) = Listings::read(&self.listings_path()) {
            for i in l.items.iter().filter(|i| i.icon.is_none()) {
                batch.push((i.note.clone(), i.name.clone()));
            }
        }
        if !batch.is_empty() {
            self.exclusive(|| {
                for (note, name) in &batch {
                    let h = house_item(name);
                    self.publish_one(None, note, name, h.price, Some(h.description), Some(h.icon))?;
                }
                Ok(())
            })?;
        }
        if self.listings_path().exists() && !self.shop_running() {
            self.shop_start(&json!({}))?;
        }
        // Make new items in one batch when stock runs to half, not one by one after each sale:
        // every mint spends the house's coins for a few blocks.
        let have = listed + ready.len() + pending;
        if have <= HOUSE_STOCK / 2 {
            let n = self.cfg.lock().unwrap().minted;
            let names: Vec<String> = (1..=(HOUSE_STOCK - have) as u64)
                .map(|i| {
                    let h = &HOUSE_ITEMS[((n + i) as usize) % HOUSE_ITEMS.len()];
                    format!("{} #{}", h.name, n + i)
                })
                .collect();
            self.mint_now(None, &names)?;
        }
        Ok(())
    }

    // ------------------------------------------------------------ shop

    fn push_event(&self, mut e: Value) {
        e["at"] = json!(now());
        let mut ev = self.events.lock().unwrap();
        ev.push(e);
        let n = ev.len();
        if n > MAX_EVENTS {
            ev.drain(..n - MAX_EVENTS);
        }
    }

    fn shop_running(&self) -> bool {
        let mut shop = self.shop.lock().unwrap();
        match shop.child.as_mut().map(|c| c.try_wait()) {
            Some(Ok(None)) => true,
            Some(_) => {
                shop.child = None;
                false
            }
            None => false,
        }
    }

    fn shop_view(&self) -> Result<Value> {
        self.absorb_sales();
        let running = self.shop_running();
        let listen = self.cfg.lock().unwrap().shop_listen.clone();
        Ok(json!({
            "running": running,
            "listen": listen,
            "public": self.public_shop,
            "events": *self.events.lock().unwrap(),
        }))
    }

    fn shop_start(&self, body: &Value) -> Result<Value> {
        self.require_unlocked()?;
        // Hosted: the shop listens on its own fixed port behind the relay.
        let listen = if self.hosted() {
            self.cfg
                .lock()
                .unwrap()
                .shop_listen
                .clone()
                .ok_or_else(|| anyhow!("no port for this shop"))?
        } else {
            field(body, "listen").unwrap_or_else(|_| "127.0.0.1:8900".into())
        };
        if listen.parse::<std::net::SocketAddr>().is_err() {
            return Err(anyhow!(
                "listen on an address such as 127.0.0.1:8900 or 0.0.0.0:8900"
            ));
        }
        if !self.listings_path().exists() {
            return Err(anyhow!(
                "list an item first: the shop serves the listings file"
            ));
        }
        let _guard = self.wallet_lock.lock().unwrap();
        self.stop_shop();
        self.spawn_shop(&listen)?;
        {
            let mut cfg = self.cfg.lock().unwrap();
            cfg.shop_listen = Some(listen);
            cfg.shop_wanted = true;
        }
        self.save_cfg()?;
        drop(_guard);
        self.shop_view()
    }

    fn set_shop_wanted(&self, wanted: bool) {
        self.cfg.lock().unwrap().shop_wanted = wanted;
        let _ = self.save_cfg();
    }

    /// After a restart the server has no passphrase, so every shop is closed until its owner
    /// signs in. Open it again then, if the owner never closed it. `room` says whether the
    /// server has a place for one more open shop.
    fn reopen_shop(&self, room: bool) {
        let wanted = self.cfg.lock().unwrap().shop_wanted;
        if !wanted || self.shop_running() || !self.listings_path().exists() {
            return;
        }
        if !room {
            self.push_event(json!({ "event": "log", "text": "the shop stays closed: the server has too many open shops right now; open it again later" }));
            return;
        }
        let listen = self.cfg.lock().unwrap().shop_listen.clone();
        match self.shop_start(&json!({ "listen": listen })) {
            Ok(_) => self.push_event(json!({ "event": "log", "text": "the shop is open again after a restart of the server" })),
            Err(e) => self.push_event(json!({ "event": "error", "error": format!("reopening the shop: {e:#}") })),
        }
    }

    fn spawn_shop(&self, listen: &str) -> Result<()> {
        let mut args = vec!["serve".to_string()];
        args.extend(self.wallet_args()?);
        args.extend([
            "--listings".into(),
            self.listings_path().display().to_string(),
            "--inbox".into(),
            self.dir.join("inbox").display().to_string(),
            "--outbox".into(),
            self.dir.join("outbox").display().to_string(),
            "--listen".into(),
            listen.to_string(),
        ]);
        let mut cmd = Command::new(&self.seller_bin);
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for v in ENV_VARS {
            cmd.env_remove(v);
        }
        if let Some(p) = self.passphrase.lock().unwrap().as_ref() {
            cmd.env("ZSETTLE_PASSPHRASE", p.as_str());
        }
        let mut child = cmd.spawn().context("starting the shop")?;
        let out = child.stdout.take();
        let err = child.stderr.take();
        let events = self.events.clone();
        let push = move |events: &Arc<Mutex<Vec<Value>>>, mut e: Value| {
            e["at"] = json!(now());
            let mut ev = events.lock().unwrap();
            ev.push(e);
            let n = ev.len();
            if n > MAX_EVENTS {
                ev.drain(..n - MAX_EVENTS);
            }
        };
        if let Some(out) = out {
            let events = events.clone();
            thread::spawn(move || {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    let e = serde_json::from_str::<Value>(&line)
                        .unwrap_or_else(|_| json!({ "event": "log", "text": line }));
                    push(&events, e);
                }
                push(&events, json!({ "event": "stopped" }));
            });
        }
        if let Some(err) = err {
            thread::spawn(move || {
                for line in BufReader::new(err).lines().map_while(Result::ok) {
                    if !line.trim().is_empty() {
                        push(&events, json!({ "event": "log", "text": line }));
                    }
                }
            });
        }
        let mut shop = self.shop.lock().unwrap();
        shop.child = Some(child);
        shop.listen = Some(listen.to_string());
        Ok(())
    }

    /// Stop the shop if it runs; returns the address it listened on.
    fn stop_shop(&self) -> Option<String> {
        let mut shop = self.shop.lock().unwrap();
        let mut child = shop.child.take()?;
        let running = matches!(child.try_wait(), Ok(None));
        let _ = child.kill();
        let _ = child.wait();
        let listen = shop.listen.take();
        running.then_some(listen).flatten()
    }

    // ------------------------------------------------------------ buying

    fn market(&self, body: &Value) -> Result<Value> {
        let (base, url) = self.shop_urls(&field(body, "url")?)?;
        let text = http_get(&url).map_err(|e| {
            if self.hosted() {
                anyhow!("could not reach that shop; is it open?")
            } else {
                e
            }
        })?;
        let l = Listings::parse(&text, &url)?;
        l.verify_signature()?;
        {
            let mut cfg = self.cfg.lock().unwrap();
            cfg.sellers.retain(|s| s != &base);
            cfg.sellers.insert(0, base.clone());
            cfg.sellers.truncate(12);
        }
        let _ = self.save_cfg();
        Ok(json!({
            "base": base,
            "listings_url": url,
            "network": l.network,
            "payout_address": l.payout_address,
            "issued_at": l.issued_at,
            "fingerprint": l.key_fingerprint(),
            "own": self.public_shop.as_deref() == Some(base.as_str()),
            "items": l.items.iter().map(|i| json!({
                "note": i.note, "name": i.name, "price_zat": i.price_zat, "max_expiry_delta": i.max_expiry_delta,
                "icon": i.icon, "description": i.description,
            })).collect::<Vec<_>>(),
        }))
    }

    fn start_buy(self: &Arc<Self>, body: &Value) -> Result<Value> {
        self.require_unlocked()?;
        self.check_confirm(body)?;
        let (payout, _) = self.accounts()?;
        let (base, url) = self.shop_urls(&field(body, "url")?)?;
        if self.public_shop.as_deref() == Some(base.as_str()) {
            return Err(anyhow!("this is your own shop"));
        }
        let item = note_field(body, "item")?;
        let name = body
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let (description, icon) = (opt_text(body, "description"), opt_text(body, "icon"));
        if zsettle_core::listing::check_details(description.as_deref(), icon.as_deref()).is_err() {
            return Err(anyhow!("that item's details are not valid"));
        }
        let stem = self.dir.join("trades").join(format!(
            "trade-{}-{}",
            now(),
            hex::encode(rand::random::<[u8; 4]>())
        ));
        Ok(self.job("buy", move |app, id| {
            let w = app.w();
            let full = format!("{}.pczt", stem.display());
            let tx = format!("{}.tx", stem.display());
            let built = app.exclusive(|| {
                app.step(id, "Importing the seller's viewing key");
                let listings_a = format!("--listings={url}");
                app.buyer(&["import-listing", "--wallet", &w, &listings_a])?;
                app.step(id, "Syncing with the network");
                app.sync()?;
                app.step(id, "Building and proving the trade (both sides, one transaction)");
                let (item_a, seller_a) = (format!("--item={item}"), format!("--seller-url={base}"));
                app.buyer(&[
                    "buy", "--wallet", &w, "--buyer-account", &payout, &listings_a, &item_a,
                    "--out", &full, &seller_a,
                ])
            })?;
            let note = built["buyer_note"].as_str().unwrap_or_default().to_string();
            if built["reply_url"].is_null() {
                // The seller never saw the offer: the note can pay again at once.
                app.free_note(&w, &payout, &note, false);
                return Err(anyhow!(
                    "the offer is built but the seller could not be reached at {base}"
                ));
            }
            app.step(id, "Offer sent; waiting for the seller to check it and sign its part");
            let finished = (|| -> Result<Value> {
            let done = app.buyer(&[
                "finish", "--wallet", &w, "--full", &full, "--out", &tx, "--broadcast", "--wait",
                "300",
            ])?;
            app.step(id, "Signed by both sides and broadcast");
            let txid = done["txid"].clone();
            if let (Some(t), Some(i)) = (txid.as_str(), built["item_action_index"].as_u64()) {
                let mut cfg = app.cfg.lock().unwrap();
                if !name.is_empty() {
                    cfg.names.insert(format!("{t}:{i}"), name.clone());
                }
                cfg.origins.insert(format!("{t}:{i}"), base.clone());
                if description.is_some() || icon.is_some() {
                    cfg.details.insert(
                        format!("{t}:{i}"),
                        json!({ "icon": icon, "description": description }),
                    );
                }
                cfg.origin_items.insert(format!("{t}:{i}"), item.clone());
            }
            app.add_activity(json!({
                "kind": "buy", "txid": txid, "price_zat": built["price_zat"], "fee_zat": built["fee_zat"],
                "text": format!("Bought {}", if name.is_empty() { &item } else { &name }),
            }));
            Ok(json!({ "txid": txid, "price_zat": built["price_zat"], "fee_zat": built["fee_zat"] }))
            })();
            if let Err(e) = &finished {
                // The seller saw the offer but the trade will not be finished by this wallet
                // (refused, no answer, or a definite rejection): retire the note, so the next
                // purchase has a fresh one and this seller cannot link it to the offer. After
                // a broadcast whose outcome is unknown the note stays reserved; the next sync
                // retires it once the offer has expired.
                if !maybe_sent(e) {
                    app.step(id, "Freeing your coins for the next purchase");
                    if app.free_note(&w, &payout, &note, true) {
                        return Err(anyhow!("{e:#}; your coins were freed for the next purchase"));
                    }
                    return Err(anyhow!(
                        "{e:#}; your coins stay set aside until the offer expires, about 40 blocks"
                    ));
                }
            }
            finished
        }))
    }

    fn start_verify(self: &Arc<Self>, body: &Value) -> Result<Value> {
        let mine = note_field(body, "item")?;
        let item = self
            .cfg
            .lock()
            .unwrap()
            .origin_items
            .get(&mine)
            .cloned()
            .unwrap_or_else(|| mine.clone());
        // Which shops to read the item's keys from: the one named, else the one it was bought
        // from, else the shops opened lately, tried in turn.
        let named = field(body, "url").ok();
        let cfg = self.cfg.lock().unwrap().clone();
        let candidates: Vec<String> = match named.or_else(|| cfg.origins.get(&mine).cloned()) {
            Some(u) => vec![u],
            None => cfg.sellers.iter().take(5).cloned().collect(),
        };
        let mut shops = Vec::new();
        for c in &candidates {
            if let Ok((_, url)) = self.shop_urls(c) {
                shops.push(url);
            }
        }
        let own = self
            .listings_path()
            .exists()
            .then(|| self.listings_path().display().to_string());
        if shops.is_empty() && own.is_none() {
            return Err(anyhow!(
                "a history needs the seller's listings: open the seller's shop once"
            ));
        }
        Ok(self.job("verify", move |app, id| {
            app.step(
                id,
                "Tracing the item through the chain with viewing keys only",
            );
            let attempt = |shop: Option<&String>| {
                let mut args = vec!["verify".to_string()];
                for s in shop.into_iter().chain(own.as_ref()) {
                    args.push(format!("--listings={s}"));
                }
                args.push(format!("--item={item}"));
                app.run(&app.buyer_bin, &args)
            };
            if shops.is_empty() {
                return attempt(None);
            }
            let mut last = None;
            for shop in &shops {
                match attempt(Some(shop)) {
                    Ok(v) => return Ok(v),
                    Err(e) => last = Some(e),
                }
            }
            Err(last.unwrap_or_else(|| anyhow!("no shop could trace this item")))
        }))
    }
}

/// A payment error after which the transaction may have reached the network: the broadcast
/// call itself failed, so lightwalletd may or may not have taken it.
fn maybe_sent(e: &anyhow::Error) -> bool {
    format!("{e:#}").contains("send_transaction")
}

/// The service base and listings URLs for what the user typed.
fn seller_urls(input: &str) -> Result<(String, String)> {
    let t = input.trim().trim_end_matches('/');
    if !(t.starts_with("http://") || t.starts_with("https://")) {
        return Err(anyhow!("a seller address starts with http:// or https://"));
    }
    if t.len() > 300
        || t.chars()
            .any(|c| c.is_whitespace() || c == '?' || c == '#' || c.is_control())
    {
        return Err(anyhow!("that is not a shop address"));
    }
    let base = t.strip_suffix("/listings").unwrap_or(t).to_string();
    let url = format!("{base}/listings");
    Ok((base, url))
}

/// GET with no redirects and a usable proxy or none, as the command-line tools do.
fn http_get(url: &str) -> Result<String> {
    let mut builder = ureq::builder()
        .redirects(0)
        .try_proxy_from_env(false)
        .timeout(Duration::from_secs(30));
    for var in [
        "ALL_PROXY",
        "all_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ] {
        let Ok(value) = std::env::var(var) else {
            continue;
        };
        if value.trim().is_empty() {
            continue;
        }
        let proxy =
            ureq::Proxy::new(value.trim().replacen("socks5h://", "socks5://", 1)).map_err(|e| {
                anyhow!("{var} names a proxy this app cannot use ({e}); nothing was sent")
            })?;
        builder = builder.proxy(proxy);
        break;
    }
    let resp = builder
        .build()
        .get(url)
        .call()
        .map_err(|e| anyhow!("fetching {url}: {e}"))?;
    if resp.status() != 200 {
        return Err(anyhow!("fetching {url}: answer {}", resp.status()));
    }
    let mut body = String::new();
    resp.into_reader()
        .take(MAX_LISTINGS_BYTES + 1)
        .read_to_string(&mut body)?;
    if body.len() as u64 > MAX_LISTINGS_BYTES {
        return Err(anyhow!("the listings at {url} are too large"));
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosted() -> Hosted {
        Hosted {
            origin: "https://demo.test".into(),
            host: "demo.test".into(),
            network: "test".into(),
            salt: String::new(),
            apps: Mutex::new(HashMap::new()),
            index: Mutex::new(Index::default()),
            sessions: Mutex::new(HashMap::new()),
            created: Mutex::new(Vec::new()),
            faucet_by_ip: Mutex::new(HashMap::new()),
            faucet_all: Mutex::new(Vec::new()),
            fails: Mutex::new(HashMap::new()),
            checks: Mutex::new(HashMap::new()),
        }
    }

    #[test]
    fn visitors_group_ipv6_by_64() {
        assert_eq!(visitor("203.0.113.7"), "203.0.113.7");
        assert_eq!(
            visitor("2001:db8:1:2:aaaa::1"),
            visitor("2001:db8:1:2:bbbb::9")
        );
        assert_ne!(visitor("2001:db8:1:2::1"), visitor("2001:db8:1:3::1"));
        assert_eq!(visitor("::ffff:198.51.100.1"), "198.51.100.1");
        assert_eq!(visitor("local"), "local");
    }

    #[test]
    fn heads_with_chunked_or_repeated_fields_are_refused() {
        assert!(
            parse_head("POST /api/x HTTP/1.1\r\nHost: a\r\nTransfer-Encoding: chunked").is_err()
        );
        assert!(parse_head("GET / HTTP/1.1\r\nHost: a\r\nHost: b").is_err());
        assert!(parse_head("POST / HTTP/1.1\r\nContent-Length: 1\r\ncontent-length: 2").is_err());
        let (m, t, h) = parse_head("GET /shop/x/listings?a=1 HTTP/1.1\r\nHOST: demo.test").unwrap();
        assert_eq!((m.as_str(), t.as_str()), ("GET", "/shop/x/listings?a=1"));
        assert_eq!(h["host"], "demo.test");
    }

    #[test]
    fn notes_and_shop_addresses() {
        let txid = "ab".repeat(32);
        assert!(valid_note(&format!("{txid}:0")));
        assert!(!valid_note(&format!("{txid}:")));
        assert!(!valid_note(&format!("{txid}:12345")));
        assert!(!valid_note("ab:0"));
        let (base, url) = seller_urls("https://demo.test/shop/abc/listings").unwrap();
        assert_eq!(base, "https://demo.test/shop/abc");
        assert_eq!(url, "https://demo.test/shop/abc/listings");
        assert!(seller_urls("ftp://x").is_err());
        assert!(seller_urls("https://x/?a").is_err());
    }

    #[test]
    fn wrong_tries_lock_then_are_forgotten() {
        let h = hosted();
        let keys = ["w:abc:1.2.3.4".to_string()];
        for _ in 0..FREE_FAILURES {
            assert!(h.check_fails(&keys).is_ok());
            h.record_fail(&keys);
        }
        assert!(h.check_fails(&keys).is_err());
        // Another visitor is not held back by these tries.
        assert!(h.check_fails(&["w:abc:5.6.7.8".to_string()]).is_ok());
        // An hour after the last failure the count starts again.
        h.fails.lock().unwrap().get_mut(&keys[0]).unwrap().1 = 0;
        h.fails.lock().unwrap().get_mut(&keys[0]).unwrap().2 = now() - 3601;
        h.record_fail(&keys);
        assert_eq!(h.fails.lock().unwrap()[&keys[0]].0, 1);
        assert!(h.check_fails(&keys).is_ok());
        h.prune();
        assert!(h.fails.lock().unwrap().contains_key(&keys[0]));
    }

    #[test]
    fn checks_are_limited_per_key() {
        let h = hosted();
        let k = [("ip:x".to_string(), 3)];
        for _ in 0..3 {
            assert!(h.count_check(&k).is_ok());
        }
        assert!(h.count_check(&k).is_err());
        assert!(h.count_check(&[("ip:y".to_string(), 3)]).is_ok());
    }

    #[test]
    fn slots_are_capped_and_given_back() {
        static C: AtomicUsize = AtomicUsize::new(0);
        let a = Slot::try_take(&C, 2).unwrap();
        let _b = Slot::try_take(&C, 2).unwrap();
        assert!(Slot::try_take(&C, 2).is_none());
        drop(a);
        assert!(Slot::take(&C, 2, Duration::from_millis(10)).is_some());
        assert_eq!(C.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn state_files_missing_damaged_and_whole() {
        let dir = std::env::temp_dir().join(format!("zsettle-app-test-{}", rand::random::<u32>()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("hub.json");
        let empty: Index = read_state(&p).unwrap();
        assert!(empty.ports.is_empty());
        fs::write(&p, b"{\"funded\":[\"a\"],\"ports\":{}}cc\"]").unwrap();
        assert!(read_state::<Index>(&p).is_err());
        let mut idx = Index::default();
        idx.ports.insert("abc".into(), 19100);
        zsettle_core::wallet::write_owner_only(&p, &serde_json::to_vec(&idx).unwrap()).unwrap();
        let back: Index = read_state(&p).unwrap();
        assert_eq!(back.ports["abc"], 19100);
        // No temporary files left behind.
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn shop_wanted_survives_a_restart_of_the_server() {
        let dir = std::env::temp_dir().join(format!("zsettle-app-test-{}", rand::random::<u32>()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("app.json");
        // A profile written before the flag existed reads as "not wanted".
        fs::write(
            &p,
            b"{\"network\":\"test\",\"shop_listen\":\"127.0.0.1:19100\"}",
        )
        .unwrap();
        let old: Config = read_state(&p).unwrap();
        assert!(!old.shop_wanted);
        let mut cfg = old.clone();
        cfg.shop_wanted = true;
        zsettle_core::wallet::write_owner_only(&p, &serde_json::to_vec(&cfg).unwrap()).unwrap();
        let back: Config = read_state(&p).unwrap();
        assert!(back.shop_wanted);
        assert_eq!(back.shop_listen.as_deref(), Some("127.0.0.1:19100"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn wider_nets_group_addresses() {
        assert_eq!(wider_net("203.0.113.9"), "203.0.113.0/24");
        assert_eq!(wider_net("2001:db8:1:2::/64"), "2001:db8:1::/48");
        assert_eq!(wider_net("local"), "local");
    }

    #[test]
    fn notes_are_parsed_with_their_change_flag() {
        let text = "aa:0      300000 zat (0.003 ZEC)  mined 4501000\n\
                    bb:1      235000 zat (0.00235 ZEC)  mined 4501200  change\n\
                    cc:2        1000 zat (0.00001 ZEC)  mined pending\n\
                    (no unspent notes)\n";
        let n = parse_notes(text);
        assert_eq!(
            n,
            vec![
                ("aa:0".to_string(), 300_000, Some(4_501_000), false),
                ("bb:1".to_string(), 235_000, Some(4_501_200), true),
                ("cc:2".to_string(), 1_000, None, false),
            ]
        );
        assert!(parse_notes("(no unspent notes)\n").is_empty());
    }

    #[test]
    fn house_items_are_found_by_name() {
        assert_eq!(house_item("Art print #12").name, "Art print");
        assert_eq!(house_item("Ticket #4").name, "Ticket");
        assert_eq!(house_item("Museum pass").price, "0.0004 zec");
        assert_eq!(house_item("Something else").name, HOUSE_ITEMS[0].name);
        for h in HOUSE_ITEMS {
            zsettle_core::listing::check_details(Some(h.description), Some(h.icon)).unwrap();
            assert!(zsettle_core::parse_zat(h.price).unwrap() > 5_000);
        }
    }

    #[test]
    fn broadcast_failures_are_never_retried() {
        assert!(maybe_sent(&anyhow!("send_transaction: timeout")));
        assert!(!maybe_sent(&anyhow!("insufficient balance")));
    }
}
