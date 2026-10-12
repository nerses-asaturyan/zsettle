//! zsettle-app: a web page for zsettle. It drives the `zsettle` and `zsettle-seller`
//! binaries for every action, so the page adds no cryptography of its own.
//!
//! Two modes. Local (the default): one profile, on this machine only, behind a page key in
//! the link. Hosted (`--public https://…`): a public demo for many people behind a TLS proxy;
//! each visitor creates a wallet of their own, signs in with a passkey (or a wallet code and a
//! passphrase), and can open a shop that others reach at `<public>/shop/<code>`. An optional
//! house shop restocks itself and hands out test coins. Hosted mode is meant for the testnet:
//! the server holds every demo wallet.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

/// Largest request body the page sends (JSON forms).
const MAX_BODY: usize = 64 * 1024;
/// Largest offer relayed to a shop (the shop itself refuses more than 1 MiB).
const MAX_RELAY_BODY: usize = 1024 * 1024 + 4096;
/// Largest answer relayed back from a shop.
const MAX_RELAY_ANSWER: usize = 2 * 1024 * 1024;
/// Largest request head.
const MAX_HEAD: usize = 16 * 1024;
/// Connections served at once.
const MAX_CONNECTIONS: usize = 64;
/// Shop events kept for the page.
const MAX_EVENTS: usize = 300;
/// Activity entries kept.
const MAX_ACTIVITY: usize = 200;
/// Largest listings file fetched from a seller.
const MAX_LISTINGS_BYTES: u64 = 1024 * 1024;
/// Hosted: wallets the server keeps at most, and new ones per hour.
const MAX_PROFILES: usize = 300;
const MAX_NEW_PER_HOUR: usize = 40;
/// Hosted: shops open at once (each is a process that syncs every few seconds).
const MAX_OPEN_SHOPS: usize = 25;
/// Hosted: a session lasts this long; an idle wallet is locked after this long.
const SESSION_SECS: u64 = 12 * 3600;
const IDLE_LOCK_SECS: u64 = 2 * 3600;
/// Hosted: first port of the shops behind the relay.
const FIRST_SHOP_PORT: u16 = 9100;
/// Hosted house shop: items it keeps listed, their price, and the test coins it gives once
/// to each new wallet.
const HOUSE: &str = "house";
const HOUSE_STOCK: usize = 6;
const HOUSE_PRICE: &str = "0.0005 zec";
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
    shop_listen: Option<String>,
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
    events: Arc<Mutex<Vec<Value>>>,
    last_sync: Mutex<Value>,
    last_seen: AtomicU64,
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
    created: Mutex<Vec<u64>>,
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
        let Ok(stream) = stream else { continue };
        if open.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
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
    let index: Index = fs::read(base.join("hub.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
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

fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 40
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
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

fn read_request(stream: &mut TcpStream) -> Result<Request> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
        if buf.len() > MAX_HEAD {
            return Err(anyhow!("request head too large"));
        }
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(anyhow!("connection closed"));
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let first = lines.next().unwrap_or_default();
    let mut parts = first.split(' ');
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    let headers: HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
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
        let n = stream.read(&mut chunk)?;
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
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'\r\n{extra}Connection: close\r\n\r\n",
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
        ("POST", "/api/unlock") => parse_body(&req).and_then(|b| app.unlock(&b)),
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
    let session = cookie(&req, "zs").and_then(|t| {
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
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/api/hello") => {
            let r = Ok(json!({
                "mode": "hosted", "network": h.network, "salt": h.salt,
                "signed_in": app.is_some(), "has_house": h.apps.lock().unwrap().contains_key(HOUSE),
            }));
            return json_answer(&mut stream, r, "");
        }
        ("POST", "/api/wallet/create") => {
            let r = parse_body(&req).and_then(|b| hub.hosted_create(h, &b));
            return answer_with_session(&mut stream, r);
        }
        ("POST", "/api/unlock") => {
            let r = parse_body(&req).and_then(|b| hub.hosted_unlock(h, &b));
            return answer_with_session(&mut stream, r);
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
                "Set-Cookie: zs=; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=0\r\n",
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
        ("POST", "/api/faucet") => hub.faucet(h, &app),
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
                    "the demo server has too many open shops right now; try later"
                ))
            } else {
                app.shop_start(&json!({}))
            }
        }
        ("POST", "/api/backup") => parse_body(&req).and_then(|b| app.backup(&b)),
        _ => api(&app, &req),
    };
    json_answer(&mut stream, r, "");
}

fn answer_with_session(stream: &mut TcpStream, r: Result<(Value, String)>) {
    match r {
        Ok((v, token)) => {
            let set = format!(
                "Set-Cookie: zs={token}; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age={SESSION_SECS}\r\n"
            );
            json_answer(stream, Ok(v), &set);
        }
        Err(e) => json_answer(stream, Err(e), ""),
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
    let Some(app) = app.filter(|a| a.shop_running()) else {
        return respond(&mut stream, 503, "text/plain", b"this shop is closed");
    };
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
            let _ = stream.write_all(&bytes);
        }
        Err(_) => respond(&mut stream, 502, "text/plain", b"the shop did not answer"),
    }
    let _ = app;
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
            app.shop_view()
        }
        ("POST", "/api/market") => app.market(&body),
        ("POST", "/api/buy") => app.start_buy(&body),
        ("POST", "/api/verify") => app.start_verify(&body),
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
        let bytes = serde_json::to_vec_pretty(&*h.index.lock().unwrap())?;
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

    fn hosted_create(&self, h: &Hosted, body: &Value) -> Result<(Value, String)> {
        {
            let mut created = h.created.lock().unwrap();
            created.retain(|t| *t + 3600 > now());
            if created.len() >= MAX_NEW_PER_HOUR {
                return Err(anyhow!(
                    "many wallets were created this hour; try again later"
                ));
            }
            if h.apps.lock().unwrap().len() >= MAX_PROFILES {
                return Err(anyhow!("this demo server is full"));
            }
            created.push(now());
        }
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
        if let Some(id) = b["passkey"]["id"].as_str() {
            h.index
                .lock()
                .unwrap()
                .by_cred
                .insert(id.to_string(), code.clone());
            self.save_index(h)?;
        }
        h.apps.lock().unwrap().insert(code.clone(), app);
        let token = self.new_session(h, &code);
        let mut v = r;
        v["code"] = json!(code);
        Ok((v, token))
    }

    fn hosted_unlock(&self, h: &Hosted, body: &Value) -> Result<(Value, String)> {
        let code = match field(body, "credential") {
            Ok(id) => h
                .index
                .lock()
                .unwrap()
                .by_cred
                .get(&id)
                .cloned()
                .ok_or_else(|| anyhow!("this passkey has no wallet on this server"))?,
            Err(_) => field(body, "code")?.to_ascii_lowercase(),
        };
        if code == HOUSE {
            return Err(anyhow!("no such wallet"));
        }
        let app = h
            .apps
            .lock()
            .unwrap()
            .get(&code)
            .cloned()
            .ok_or_else(|| anyhow!("no such wallet"))?;
        // A failed guess costs a second, so codes and passphrases cannot be tried quickly.
        if let Err(e) = app.unlock(body) {
            thread::sleep(Duration::from_secs(1));
            return Err(e);
        }
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
    fn faucet(&self, h: &Hosted, app: &Arc<App>) -> Result<Value> {
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
        {
            let mut idx = h.index.lock().unwrap();
            if !idx.funded.insert(app.profile.clone()) {
                return Err(anyhow!("this wallet already got its test coins"));
            }
        }
        self.save_index(h)?;
        let addr = app.address()?["address"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("no address"))?;
        let profile = app.profile.clone();
        let base = self.base.clone();
        Ok(app.job("faucet", move |app, id| {
            app.step(id, "Sending free test coins from the demo faucet");
            match house.faucet_pay(&addr, FAUCET_ZAT) {
                Ok(r) => {
                    app.add_activity(json!({ "kind": "faucet", "txid": r["txid"], "amount_zat": FAUCET_ZAT, "text": "Received free test coins" }));
                    Ok(json!({ "txid": r["txid"], "amount_zat": FAUCET_ZAT }))
                }
                Err(e) => {
                    // Give the chance back, and say what failed.
                    if let Ok(b) = fs::read(base.join("hub.json")) {
                        if let Ok(mut idx) = serde_json::from_slice::<Index>(&b) {
                            idx.funded.remove(&profile);
                            if let Ok(bytes) = serde_json::to_vec_pretty(&idx) {
                                let _ = zsettle_core::wallet::write_owner_only(&base.join("hub.json"), &bytes);
                            }
                        }
                    }
                    Err(anyhow!("the faucet could not pay right now ({e:#}); try again in a few minutes"))
                }
            }
        }))
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
    // Lock idle wallets and drop expired sessions.
    let hb = hub.clone();
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(60));
        let Mode::Hosted(h) = &hb.mode else { return };
        h.sessions.lock().unwrap().retain(|_, s| s.expires > now());
        let apps: Vec<Arc<App>> = h.apps.lock().unwrap().values().cloned().collect();
        for a in apps {
            if a.profile != HOUSE && a.last_seen.load(Ordering::SeqCst) + IDLE_LOCK_SECS < now() {
                *a.passphrase.lock().unwrap() = None;
            }
        }
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
        let cfg: Config = fs::read(dir.join("app.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
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
            events: Arc::new(Mutex::new(Vec::new())),
            last_sync: Mutex::new(last_sync),
            last_seen: AtomicU64::new(now()),
        }))
    }

    fn hosted(&self) -> bool {
        self.public_shop.is_some()
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
        let cfg = self.cfg.lock().unwrap().clone();
        zsettle_core::wallet::write_owner_only(
            &self.dir.join("app.json"),
            &serde_json::to_vec_pretty(&cfg)?,
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
        let out = cmd
            .output()
            .with_context(|| format!("running {}", bin.display()))?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let msg: Vec<&str> = stderr
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with("warning:") && !l.starts_with('#'))
                .collect();
            let text = msg.join(" ").replace("Error: ", "");
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

    /// Unspent notes of an account: (note, value, mined height or none).
    fn notes(&self, account: &str) -> Result<Vec<(String, u64, Option<u32>)>> {
        let w = self.w();
        let out = self.buyer(&["notes", "--wallet", &w, "--account", account])?;
        let text = out.as_str().unwrap_or_default().to_string();
        let mut notes = Vec::new();
        for line in text.lines() {
            let mut parts = line.split_whitespace();
            let (Some(note), Some(value)) = (parts.next(), parts.next()) else {
                continue;
            };
            let Ok(value) = value.parse::<u64>() else {
                continue;
            };
            let mined = line
                .rsplit("mined ")
                .next()
                .and_then(|h| h.trim().parse::<u32>().ok());
            notes.push((note.to_string(), value, mined));
        }
        Ok(notes)
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

    fn job(
        self: &Arc<Self>,
        kind: &str,
        f: impl FnOnce(&Arc<App>, u64) -> Result<Value> + Send + 'static,
    ) -> Value {
        let id = self.next_job.fetch_add(1, Ordering::SeqCst) + 1;
        {
            let mut jobs = self.jobs.lock().unwrap();
            // Keep the last hundred jobs.
            if jobs.len() > 100 {
                let keep: HashSet<u64> = (id.saturating_sub(100)..=id).collect();
                jobs.retain(|k, _| keep.contains(k));
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
        }
        let app = self.clone();
        thread::spawn(move || {
            let r = f(&app, id);
            let mut jobs = app.jobs.lock().unwrap();
            if let Some(j) = jobs.get_mut(&id) {
                match r {
                    Ok(v) => {
                        j.state = "done".into();
                        j.result = v;
                    }
                    Err(e) => {
                        j.state = "failed".into();
                        j.error = Some(format!("{e:#}"));
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
        let was = self.stop_shop();
        let r = f();
        if let Some(listen) = was {
            if let Err(e) = self.spawn_shop(&listen) {
                self.push_event(
                    json!({ "event": "error", "error": format!("restarting the shop: {e:#}") }),
                );
            }
        }
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
                    json!({ "kind": "sale", "txid": e["txid"], "text": format!("Sold {name}"), "at": e["at"] }),
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
        Wallet::open(self.wallet_dir())?.check_passphrase(&pass)?;
        *self.passphrase.lock().unwrap() = Some(Zeroizing::new(pass));
        Ok(json!({ "unlocked": true }))
    }

    /// The recovery words, for a passphrase given again (or a passkey asked again).
    fn backup(&self, body: &Value) -> Result<Value> {
        let pass = field(body, "passphrase")?;
        let words = Wallet::open(self.wallet_dir())?.mnemonic_phrase(&pass)?;
        Ok(json!({ "words": words.split_whitespace().collect::<Vec<_>>() }))
    }

    fn wallet_view(&self) -> Result<Value> {
        let (payout, item) = self.accounts()?;
        let names = self.item_names();
        let zec = self.notes(&payout)?;
        let items = self.notes(&item)?;
        let listed: HashMap<String, u64> = Listings::read(&self.listings_path())
            .map(|l| {
                l.items
                    .iter()
                    .map(|i| (i.note.clone(), i.price_zat))
                    .collect()
            })
            .unwrap_or_default();
        Ok(json!({
            "balance_zat": zec.iter().map(|n| n.1).sum::<u64>(),
            "notes": zec.iter().map(|(n, v, m)| json!({ "note": n, "value_zat": v, "mined": m })).collect::<Vec<_>>(),
            "items": items.iter().map(|(n, v, m)| json!({
                "note": n, "value_zat": v, "mined": m,
                "name": names.get(n), "listed_zat": listed.get(n),
            })).collect::<Vec<_>>(),
        }))
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
                app.record_sync(json!({ "report": { "by": "shop" }, "at": now() }));
                Ok(json!({ "by": "shop" }))
            }));
        }
        Ok(self.job("sync", |app, id| {
            app.step(id, "Syncing with the network");
            app.exclusive(|| app.sync())
        }))
    }

    fn start_pay(self: &Arc<Self>, body: &Value) -> Result<Value> {
        self.require_unlocked()?;
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
                let mut args = vec![
                    "pay", "--wallet", &w, "--account", &payout, "--to", &to, "--amount", &amount,
                ];
                if let Some(m) = memo.as_deref() {
                    args.extend(["--memo", m]);
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
            let r = self.seller(&[
                "pay", "--wallet", &w, "--account", &payout, "--to", to, "--amount", &amount,
                "--memo", "zsettle demo: free test coins",
            ])?;
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
                args.extend(["--name".to_string(), n.clone()]);
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
        let note = field(body, "note")?;
        let price = field(body, "price")?;
        let name = body
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|n| !n.trim().is_empty())
            .or_else(|| self.item_names().get(&note).cloned())
            .unwrap_or_default();
        self.accounts()?;
        Ok(self.job("publish", move |app, id| {
            app.publish_now(Some(id), &note, &name, &price)
        }))
    }

    fn publish_now(&self, id: Option<u64>, note: &str, name: &str, price: &str) -> Result<Value> {
        let wa = self.wallet_args()?;
        let listings = self.listings_path().display().to_string();
        self.exclusive(|| {
            if !self.shop_running() {
                self.step_opt(id, "Syncing with the network");
                self.sync()?;
            }
            self.step_opt(id, "Signing the listings file");
            let mut args = vec!["publish".to_string()];
            args.extend(wa);
            args.extend([
                "--listings".into(),
                listings,
                "--item-note".into(),
                note.to_string(),
                "--price".into(),
                price.to_string(),
            ]);
            if !name.is_empty() {
                args.extend(["--name".into(), name.to_string()]);
            }
            let r = self.run(&self.seller_bin, &args)?;
            self.add_activity(json!({ "kind": "list", "text": format!("Listed {} for {price}", if name.is_empty() { note } else { name }) }));
            Ok(r)
        })
    }

    fn start_delist(self: &Arc<Self>, body: &Value) -> Result<Value> {
        self.require_unlocked()?;
        let note = field(body, "note")?;
        let wa = self.wallet_args()?;
        let listings = self.listings_path().display().to_string();
        Ok(self.job("delist", move |app, id| {
            app.exclusive(|| {
                app.step(id, "Removing the item from the listings file");
                let mut args = vec!["publish".to_string()];
                args.extend(wa);
                args.extend([
                    "--listings".into(),
                    listings,
                    "--remove".into(),
                    note.clone(),
                ]);
                let r = app.run(&app.seller_bin, &args)?;
                app.add_activity(json!({ "kind": "delist", "text": format!("Delisted {note}") }));
                Ok(r)
            })
        }))
    }

    fn start_cancel(self: &Arc<Self>, body: &Value) -> Result<Value> {
        self.require_unlocked()?;
        let note = field(body, "note")?;
        let wa = self.wallet_args()?;
        let listings = self.listings_path();
        Ok(self.job("cancel", move |app, id| {
            app.exclusive(|| {
                app.step(id, "Moving the item to a fresh note, so no signed offer can settle");
                let mut args = vec!["cancel".to_string()];
                args.extend(wa);
                args.extend(["--item-note".into(), note.clone()]);
                if listings.exists() {
                    args.extend(["--listings".into(), listings.display().to_string()]);
                }
                let r = app.run(&app.seller_bin, &args)?;
                app.add_activity(json!({ "kind": "cancel", "txid": r["txid"], "text": format!("Cancelled the offers on {note}") }));
                Ok(r)
            })
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
        for it in ready.iter().take(HOUSE_STOCK.saturating_sub(listed)) {
            let note = it["note"].as_str().unwrap_or_default();
            let name = it["name"].as_str().unwrap_or_default();
            self.publish_now(None, note, name, HOUSE_PRICE)?;
        }
        if self.listings_path().exists() && !self.shop_running() {
            self.shop_start(&json!({}))?;
        }
        if listed + ready.len() + pending < HOUSE_STOCK {
            let n = self.cfg.lock().unwrap().minted;
            let names: Vec<String> = (1..=(HOUSE_STOCK - listed - ready.len() - pending) as u64)
                .map(|i| format!("Ticket #{}", n + i))
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
        self.cfg.lock().unwrap().shop_listen = Some(listen);
        self.save_cfg()?;
        drop(_guard);
        self.shop_view()
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
        let (base, url) = seller_urls(&field(body, "url")?)?;
        let text = http_get(&url)?;
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
            })).collect::<Vec<_>>(),
        }))
    }

    fn start_buy(self: &Arc<Self>, body: &Value) -> Result<Value> {
        self.require_unlocked()?;
        let (payout, _) = self.accounts()?;
        let (base, url) = seller_urls(&field(body, "url")?)?;
        if self.public_shop.as_deref() == Some(base.as_str()) {
            return Err(anyhow!("this is your own shop"));
        }
        let item = field(body, "item")?;
        let name = body
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let stem = self.dir.join("trades").join(format!("trade-{}", now()));
        Ok(self.job("buy", move |app, id| {
            let w = app.w();
            let full = format!("{}.pczt", stem.display());
            let tx = format!("{}.tx", stem.display());
            let built = app.exclusive(|| {
                app.step(id, "Importing the seller's viewing key");
                app.buyer(&["import-listing", "--wallet", &w, "--listings", &url])?;
                app.step(id, "Syncing with the network");
                app.sync()?;
                app.step(id, "Building and proving the trade (both sides, one transaction)");
                app.buyer(&[
                    "buy", "--wallet", &w, "--buyer-account", &payout, "--listings", &url,
                    "--item", &item, "--out", &full, "--seller-url", &base,
                ])
            })?;
            if built["reply_url"].is_null() {
                return Err(anyhow!(
                    "the offer is built but the seller could not be reached at {base}"
                ));
            }
            app.step(id, "Offer sent; waiting for the seller to check it and sign its part");
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
                cfg.origin_items.insert(format!("{t}:{i}"), item.clone());
            }
            app.add_activity(json!({
                "kind": "buy", "txid": txid, "price_zat": built["price_zat"], "fee_zat": built["fee_zat"],
                "text": format!("Bought {}", if name.is_empty() { &item } else { &name }),
            }));
            Ok(json!({ "txid": txid, "price_zat": built["price_zat"], "fee_zat": built["fee_zat"] }))
        }))
    }

    fn start_verify(self: &Arc<Self>, body: &Value) -> Result<Value> {
        let mine = field(body, "item")?;
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
            shops.push(seller_urls(c)?.1);
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
                    args.extend(["--listings".to_string(), s.clone()]);
                }
                args.extend(["--item".to_string(), item.clone()]);
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

/// The service base and listings URLs for what the user typed.
fn seller_urls(input: &str) -> Result<(String, String)> {
    let t = input.trim().trim_end_matches('/');
    if !(t.starts_with("http://") || t.starts_with("https://")) {
        return Err(anyhow!("a seller address starts with http:// or https://"));
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
