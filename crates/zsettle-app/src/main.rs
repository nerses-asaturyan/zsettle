//! zsettle-app: a local web page for zsettle. It listens on this machine only, keeps the
//! wallet here, and drives the `zsettle` and `zsettle-seller` binaries for every action, so
//! the page adds no new cryptography: it is a view on the commands.

use std::collections::HashMap;
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

/// Largest request body the page sends (JSON forms).
const MAX_BODY: usize = 64 * 1024;
/// Largest request head.
const MAX_HEAD: usize = 16 * 1024;
/// Connections served at once.
const MAX_CONNECTIONS: usize = 16;
/// Shop events kept for the page.
const MAX_EVENTS: usize = 300;
/// Activity entries kept.
const MAX_ACTIVITY: usize = 200;
/// Largest listings file fetched from a seller.
const MAX_LISTINGS_BYTES: u64 = 1024 * 1024;
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
    about = "Local web page for zsettle: a wallet, a market, buying and selling"
)]
struct Cli {
    /// Profile name: each profile has its own wallet, shop and history. Run two profiles on
    /// two ports to play buyer and seller on one machine.
    #[arg(long, default_value = "default")]
    profile: String,
    /// Directory holding the profiles (default: ~/.zsettle-app).
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Port of the page; only this machine can reach it.
    #[arg(long, default_value_t = 7070)]
    port: u16,
    /// Directory holding the `zsettle` and `zsettle-seller` binaries (default: next to this
    /// one).
    #[arg(long)]
    bin_dir: Option<PathBuf>,
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

struct App {
    dir: PathBuf,
    port: u16,
    token: String,
    profile: String,
    buyer_bin: PathBuf,
    seller_bin: PathBuf,
    passphrase: Mutex<Option<Zeroizing<String>>>,
    cfg: Mutex<Config>,
    jobs: Mutex<HashMap<u64, Job>>,
    next_job: AtomicU64,
    wallet_lock: Mutex<()>,
    shop: Mutex<Shop>,
    events: Arc<Mutex<Vec<Value>>>,
    last_sync: Mutex<Value>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.profile.is_empty()
        || !cli
            .profile
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(anyhow!("a profile name uses letters, digits, - and _"));
    }
    let base = match cli.data_dir {
        Some(d) => d,
        None => home()?.join(".zsettle-app"),
    };
    let dir = base.join(&cli.profile);
    for sub in ["trades", "inbox", "outbox"] {
        fs::create_dir_all(dir.join(sub))
            .with_context(|| format!("creating {}", dir.join(sub).display()))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700))?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    }
    let bin_dir = match cli.bin_dir {
        Some(d) => d,
        None => std::env::current_exe()?
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| anyhow!("cannot find the directory of this program"))?,
    };
    let exe = |name: &str| bin_dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    let (buyer_bin, seller_bin) = (exe("zsettle"), exe("zsettle-seller"));
    for b in [&buyer_bin, &seller_bin] {
        if !b.exists() {
            return Err(anyhow!(
                "{} not found: keep zsettle-app next to zsettle and zsettle-seller, or pass --bin-dir",
                b.display()
            ));
        }
    }
    let cfg: Config = fs::read(dir.join("app.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let token = hex::encode(rand::random::<[u8; 16]>());
    let listener = TcpListener::bind(("127.0.0.1", cli.port))
        .with_context(|| format!("listening on 127.0.0.1:{}", cli.port))?;
    let app = Arc::new(App {
        dir,
        port: cli.port,
        token,
        profile: cli.profile,
        buyer_bin,
        seller_bin,
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
        last_sync: Mutex::new(Value::Null),
    });
    if let Some(v) = app.cfg.lock().unwrap().last_sync.clone() {
        *app.last_sync.lock().unwrap() = v;
    }
    eprintln!(
        "zsettle-app, profile {:?}\nOpen this link in your browser (it carries this session's key; keep it to yourself):\n\n    http://localhost:{}/#token={}\n",
        app.profile, app.port, app.token
    );
    let open = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        if open.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
            continue;
        }
        open.fetch_add(1, Ordering::SeqCst);
        let (app, open) = (app.clone(), open.clone());
        thread::spawn(move || {
            handle(&app, stream);
            open.fetch_sub(1, Ordering::SeqCst);
        });
    }
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
    let mut chunk = [0u8; 4096];
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
    let len: usize = headers
        .get("content-length")
        .map(|v| v.parse())
        .transpose()?
        .unwrap_or(0);
    if len > MAX_BODY {
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
    let path = target.split('?').next().unwrap_or_default().to_string();
    Ok(Request {
        method,
        path,
        headers,
        body,
    })
}

fn respond(stream: &mut TcpStream, status: u16, ctype: &str, body: &[u8]) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        421 => "Misdirected Request",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

fn handle(app: &Arc<App>, mut stream: TcpStream) {
    let req = match read_request(&mut stream) {
        Ok(r) => r,
        Err(_) => return respond(&mut stream, 400, "text/plain", b"bad request"),
    };
    // Only this page, under its own name, may talk to the app: a Host check stops DNS
    // rebinding, the Origin check and the token stop other web pages.
    let ours = [
        format!("localhost:{}", app.port),
        format!("127.0.0.1:{}", app.port),
    ];
    if !req.headers.get("host").is_some_and(|h| ours.contains(h)) {
        return respond(&mut stream, 421, "text/plain", b"wrong host");
    }
    if let Some(origin) = req.headers.get("origin") {
        if !ours.iter().any(|o| origin == &format!("http://{o}")) {
            return respond(&mut stream, 403, "text/plain", b"wrong origin");
        }
    }
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") | ("GET", "/index.html") => {
            return respond(
                &mut stream,
                200,
                "text/html; charset=utf-8",
                INDEX_HTML.as_bytes(),
            )
        }
        ("GET", "/app.js") => {
            return respond(
                &mut stream,
                200,
                "text/javascript; charset=utf-8",
                APP_JS.as_bytes(),
            )
        }
        ("GET", "/qr.js") => {
            return respond(
                &mut stream,
                200,
                "text/javascript; charset=utf-8",
                QR_JS.as_bytes(),
            )
        }
        ("GET", "/style.css") => {
            return respond(
                &mut stream,
                200,
                "text/css; charset=utf-8",
                STYLE_CSS.as_bytes(),
            )
        }
        _ => {}
    }
    if !req.path.starts_with("/api/") {
        return respond(&mut stream, 404, "text/plain", b"not found");
    }
    if req.headers.get("x-zsettle-token") != Some(&app.token) {
        return respond(
            &mut stream,
            401,
            "application/json",
            br#"{"error":"open the link the app printed in its terminal"}"#,
        );
    }
    let (status, body) = match api(app, &req) {
        Ok(v) => (200, v),
        Err(e) => (400, json!({ "error": format!("{e:#}") })),
    };
    respond(
        &mut stream,
        status,
        "application/json",
        body.to_string().as_bytes(),
    );
}

fn field(body: &Value, key: &str) -> Result<String> {
    body.get(key)
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("missing {key}"))
}

fn api(app: &Arc<App>, req: &Request) -> Result<Value> {
    let body: Value = if req.body.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&req.body).context("the request is not JSON")?
    };
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/api/status") => app.status(),
        ("POST", "/api/wallet/create") => app.create_wallet(&body),
        ("POST", "/api/unlock") => app.unlock(&body),
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

// ---------------------------------------------------------------- commands

impl App {
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

    /// Run one command and return the JSON it printed last, or its text.
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
        self.jobs.lock().unwrap().insert(
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
            "shop": { "running": running, "listen": cfg.shop_listen },
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
        let wa = self.wallet_args()?;
        Ok(self.job("mint", move |app, id| {
            app.exclusive(|| {
                app.step(id, "Syncing with the network");
                app.sync()?;
                app.step(id, "Minting the items in one shielded transaction");
                let mut args = vec!["mint".to_string()];
                args.extend(wa);
                for n in &names {
                    args.extend(["--name".to_string(), n.clone()]);
                }
                let r = app.run(&app.seller_bin, &args)?;
                {
                    let mut cfg = app.cfg.lock().unwrap();
                    if let Some(notes) = r["notes"].as_array() {
                        for (note, name) in notes.iter().zip(&names) {
                            if let Some(n) = note.as_str() {
                                cfg.names.insert(n.to_string(), name.clone());
                            }
                        }
                    }
                }
                app.save_cfg()?;
                app.add_activity(json!({ "kind": "mint", "txid": r["txid"], "text": format!("Minted {}", names.join(", ")) }));
                Ok(r)
            })
        }))
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
        let wa = self.wallet_args()?;
        let listings = self.listings_path().display().to_string();
        Ok(self.job("publish", move |app, id| {
            app.exclusive(|| {
                app.step(id, "Syncing with the network");
                app.sync()?;
                app.step(id, "Signing the listings file");
                let mut args = vec!["publish".to_string()];
                args.extend(wa);
                args.extend([
                    "--listings".into(),
                    listings,
                    "--item-note".into(),
                    note.clone(),
                    "--price".into(),
                    price.clone(),
                ]);
                if !name.is_empty() {
                    args.extend(["--name".into(), name.clone()]);
                }
                let r = app.run(&app.seller_bin, &args)?;
                app.add_activity(json!({ "kind": "list", "text": format!("Listed {} for {price}", if name.is_empty() { &note } else { &name }) }));
                Ok(r)
            })
        }))
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
            "events": *self.events.lock().unwrap(),
        }))
    }

    fn shop_start(&self, body: &Value) -> Result<Value> {
        self.require_unlocked()?;
        let listen = field(body, "listen").unwrap_or_else(|_| "127.0.0.1:8900".into());
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
            "items": l.items.iter().map(|i| json!({
                "note": i.note, "name": i.name, "price_zat": i.price_zat, "max_expiry_delta": i.max_expiry_delta,
            })).collect::<Vec<_>>(),
        }))
    }

    fn start_buy(self: &Arc<Self>, body: &Value) -> Result<Value> {
        self.require_unlocked()?;
        let (payout, _) = self.accounts()?;
        let (base, url) = seller_urls(&field(body, "url")?)?;
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
