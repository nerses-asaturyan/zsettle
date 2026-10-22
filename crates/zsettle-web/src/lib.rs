//! zsettle in the browser: the wallet, its sync and the buyer's side of a trade, compiled to
//! WebAssembly and run inside the visitor's page.
//!
//! The seed never leaves the page. The server only relays public chain data
//! (`/api/chain/...`, the same requests any light wallet makes) and passes offers to shops
//! (`/shop/...`). The page keeps the wallet database itself, encrypted, and hands it back to
//! [`WebWallet::open`] on the next visit; the mnemonic is passed in for each call that signs
//! and dropped afterwards.
//!
//! Every call runs in a Web Worker: proving uses a thread pool the page starts first
//! ([`init_thread_pool`]).

use std::str::FromStr;

use anyhow::{anyhow, Context, Result};
use bip0039::{Count, English, Mnemonic};
use orchard::keys::SpendAuthorizingKey;
use prost::Message;
use secrecy::SecretVec;
use serde_json::{json, Value};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use zcash_client_backend::data_api::{
    chain::{error::Error as ChainError, scan_cached_blocks, ChainState, CommitmentTreeRoot},
    scanning::{ScanPriority, ScanRange},
    AccountBirthday, WalletCommitmentTrees, WalletRead, WalletWrite,
};
use zcash_client_backend::proto::{
    compact_formats::CompactBlock,
    service::{BlockId, SubtreeRoot, TreeState},
};
use zcash_primitives::merkle_tree::HashSer;
use zcash_protocol::consensus::{BlockHeight, BranchId, Network};

use zsettle_core::{
    listing::Listings,
    mint::{self, NoteState, PayRequest},
    ownership::{check_challenge, check_label, OwnershipProof},
    pczt as pczt_ops,
    trade::{build_self_spend, build_trade, TradeRequest},
    wallet::{
        account_birthday, account_orchard_fvk, find_account_by_ufvk, heights,
        import_viewing_key_at, largest_spendable_note, list_accounts, max_scanned_height,
        parse_account, record_transaction, release_note, reservations, reserve_note,
        reserved_notes, scanned_tip, MemBlockCache, NoteRef, Wallet, WalletAccount,
        BIRTHDAY_ROUNDING,
    },
};

pub use wasm_bindgen_rayon::init_thread_pool;

mod history;
mod seller;

/// Blocks downloaded and scanned per step: small enough for a phone's memory, large enough
/// that the per-request overhead does not dominate.
const SYNC_BATCH_BLOCKS: u32 = 500;

/// The accounts a new wallet gets, as the command-line `init` makes them: payments and change
/// in the first, items in the second, so a published item key reveals nothing about payments.
const PAY_ACCOUNT: (&str, u32) = ("buyer", 0);
const ITEM_ACCOUNT: (&str, u32) = ("item", 1);

/// How long a buyer waits for the shop's signature, in seconds.
const REPLY_WAIT_SECS: f64 = 240.0;

fn js(e: anyhow::Error) -> JsError {
    JsError::new(&format!("{e:#}"))
}

fn js_text(v: JsValue) -> anyhow::Error {
    anyhow!(v
        .as_string()
        .or_else(|| js_sys::JSON::stringify(&v).ok().and_then(|s| s.as_string()))
        .unwrap_or_else(|| "browser error".into()))
}

/// Set up error reporting; call once per worker.
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
}

/// Build the proving key now, so the first trade does not wait for it.
#[wasm_bindgen]
pub fn warm_up() {
    zsettle_core::trade::proving_key();
}

/// A fresh 24-word mnemonic.
#[wasm_bindgen]
pub fn new_mnemonic() -> String {
    Mnemonic::<English>::generate(Count::Words24)
        .phrase()
        .to_string()
}

/// Whether a phrase is a valid BIP 39 English mnemonic.
#[wasm_bindgen]
pub fn check_mnemonic(phrase: &str) -> Result<(), JsError> {
    Mnemonic::<English>::from_phrase(phrase.trim())
        .map(|_| ())
        .map_err(|e| JsError::new(&format!("not a valid recovery phrase: {e}")))
}

/// Whether `address` is one this wallet pays on `network`: a unified address with an Orchard
/// receiver (payments stay shielded).
#[wasm_bindgen]
pub fn check_address(network: &str, address: &str) -> bool {
    network_of(network).is_ok_and(|n| {
        matches!(
            zcash_keys::address::Address::decode(&n, address.trim()),
            Some(zcash_keys::address::Address::Unified(ua)) if ua.orchard().is_some()
        )
    })
}

fn seed_of(phrase: &str) -> Result<SecretVec<u8>> {
    let m = Mnemonic::<English>::from_phrase(phrase.trim())
        .map_err(|e| anyhow!("not a valid recovery phrase: {e}"))?;
    Ok(SecretVec::new(m.to_seed("").to_vec()))
}

fn network_of(name: &str) -> Result<Network> {
    match name {
        "main" => Ok(Network::MainNetwork),
        "test" => Ok(Network::TestNetwork),
        _ => Err(anyhow!("network is main or test")),
    }
}

fn network_name(n: Network) -> &'static str {
    match n {
        Network::MainNetwork => "main",
        Network::TestNetwork => "test",
    }
}

// ---- HTTP from the worker -------------------------------------------------------------

/// Longest wait for one answer from the server: past it the call fails instead of hanging.
const FETCH_LIMIT_MS: u32 = 150_000;

async fn fetch(url: &str, body: Option<&[u8]>) -> Result<(u16, Vec<u8>)> {
    let init = web_sys::RequestInit::new();
    init.set_method(if body.is_some() { "POST" } else { "GET" });
    init.set_signal(Some(&web_sys::AbortSignal::timeout_with_u32(
        FETCH_LIMIT_MS,
    )));
    if let Some(b) = body {
        // A copy: a request body cannot be a view of the shared wasm memory.
        init.set_body(&js_sys::Uint8Array::from(b).into());
    }
    let req = web_sys::Request::new_with_str_and_init(url, &init).map_err(js_text)?;
    let headers = req.headers();
    headers.set("X-Zsettle", "1").map_err(js_text)?;
    if body.is_some() {
        headers
            .set("Content-Type", "application/octet-stream")
            .map_err(js_text)?;
    }
    let scope: web_sys::WorkerGlobalScope = js_sys::global().unchecked_into();
    let resp: web_sys::Response = JsFuture::from(scope.fetch_with_request(&req))
        .await
        .map_err(|e| anyhow!("{url}: {}", js_text(e)))?
        .unchecked_into();
    let buf = JsFuture::from(resp.array_buffer().map_err(js_text)?)
        .await
        .map_err(js_text)?;
    Ok((resp.status(), js_sys::Uint8Array::new(&buf).to_vec()))
}

async fn fetch_ok(url: &str, body: Option<&[u8]>) -> Result<Vec<u8>> {
    let (status, bytes) = fetch(url, body).await?;
    if status != 200 {
        let text = String::from_utf8_lossy(&bytes);
        let msg = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_string))
            .unwrap_or_else(|| text.chars().take(200).collect());
        return Err(anyhow!("{msg} ({status})"));
    }
    Ok(bytes)
}

async fn sleep_ms(ms: i32) {
    let p = js_sys::Promise::new(&mut |resolve, _| {
        let scope: web_sys::WorkerGlobalScope = js_sys::global().unchecked_into();
        let _ = scope.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
    });
    let _ = JsFuture::from(p).await;
}

/// The chain relay of the server that served this page, for one network.
struct Relay {
    net: &'static str,
}

impl Relay {
    fn url(&self, path: &str, query: &str) -> String {
        format!("/api/chain/{path}?net={}{query}", self.net)
    }

    async fn tip(&self) -> Result<u32> {
        let b = fetch_ok(&self.url("tip", ""), None).await?;
        let id = BlockId::decode(&b[..]).context("the relay's chain tip")?;
        u32::try_from(id.height).map_err(|_| anyhow!("height out of range"))
    }

    async fn tree_state(&self, height: u32) -> Result<TreeState> {
        let b = fetch_ok(&self.url("tree-state", &format!("&height={height}")), None).await?;
        TreeState::decode(&b[..]).context("the relay's tree state")
    }

    async fn blocks(&self, start: u32, end: u32) -> Result<Vec<CompactBlock>> {
        let b = fetch_ok(
            &self.url("blocks", &format!("&start={start}&end={end}")),
            None,
        )
        .await?;
        let mut buf = &b[..];
        let mut out = Vec::new();
        while !buf.is_empty() {
            out.push(CompactBlock::decode_length_delimited(&mut buf).context("a compact block")?);
        }
        Ok(out)
    }

    async fn roots<H: HashSer>(&self, pool: &str) -> Result<Vec<CommitmentTreeRoot<H>>> {
        let b = fetch_ok(
            &self.url("subtree-roots", &format!("&pool={pool}&start=0&max=10000")),
            None,
        )
        .await?;
        let mut buf = &b[..];
        let mut out = Vec::new();
        while !buf.is_empty() {
            let r = SubtreeRoot::decode_length_delimited(&mut buf).context("a subtree root")?;
            let height = u32::try_from(r.completing_block_height)
                .map_err(|_| anyhow!("height out of range"))?;
            out.push(CommitmentTreeRoot::from_parts(
                BlockHeight::from_u32(height),
                H::read(&r.root_hash[..]).context("a subtree root hash")?,
            ));
        }
        Ok(out)
    }

    /// A mined transaction: its height and raw bytes.
    async fn raw_tx(&self, txid: &zcash_protocol::TxId) -> Result<(u32, Vec<u8>)> {
        let b = fetch_ok(&self.url("raw-tx", &format!("&txid={txid}")), None).await?;
        if b.len() < 4 {
            return Err(anyhow!("the relay sent a short answer"));
        }
        let height = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        Ok((height, b[4..].to_vec()))
    }

    /// The height a transaction was mined at, if it was.
    async fn tx_height(&self, txid: &zcash_protocol::TxId) -> Result<Option<u32>> {
        let b = fetch_ok(&self.url("tx", &format!("&txid={txid}")), None).await?;
        let v: Value = serde_json::from_slice(&b).context("the relay's answer")?;
        Ok(v["height"].as_u64().and_then(|h| u32::try_from(h).ok()))
    }

    async fn info(&self) -> Result<Value> {
        let b = fetch_ok(&self.url("info", ""), None).await?;
        serde_json::from_slice(&b).context("the relay's server info")
    }

    async fn send(&self, tx: &[u8]) -> Result<String> {
        let b = fetch_ok(&self.url("send", ""), Some(tx)).await?;
        Ok(String::from_utf8_lossy(&b).into_owned())
    }

    /// Refuse to sign for a chain or a consensus branch the server is not on.
    async fn check_consensus(&self, network: Network, height: u32) -> Result<()> {
        let info = self.info().await?;
        if info["chain_name"].as_str() != Some(network_name(network)) {
            return Err(anyhow!(
                "the server is on chain {}, not {}",
                info["chain_name"],
                network_name(network)
            ));
        }
        let theirs = info["consensus_branch_id"].as_u64().unwrap_or(0);
        let ours = u32::from(BranchId::for_height(&network, BlockHeight::from(height)));
        if theirs != u64::from(ours) {
            return Err(anyhow!(
                "the network is on consensus branch {theirs:08x} but this page signs for {ours:08x}; reload the page for a newer version"
            ));
        }
        Ok(())
    }
}

// ---- The wallet -------------------------------------------------------------------------

/// A wallet held in this worker's memory.
#[wasm_bindgen]
pub struct WebWallet {
    wallet: Wallet,
    /// Keeps the in-memory database alive between calls.
    _keep: rusqlite::Connection,
    relay: Relay,
    cache: MemBlockCache,
    /// Transactions already fetched for their memos this session.
    memo_tried: std::collections::HashSet<zcash_protocol::TxId>,
    /// The shop, while it is open: the item key and the proofs already verified.
    shop: seller::ShopState,
    /// The paying note of the purchase in progress, once its offer left, and whether its
    /// transaction was being sent (then it may be out, and is not given up).
    offer: Option<(NoteRef, bool)>,
}

fn progress(cb: &Option<js_sys::Function>, v: Value) {
    if let Some(cb) = cb {
        if let Ok(arg) = js_sys::JSON::parse(&v.to_string()) {
            let _ = cb.call1(&JsValue::NULL, &arg);
        }
    }
}

#[wasm_bindgen]
impl WebWallet {
    /// Create a wallet from a mnemonic: the two accounts `init` makes, with a birthday a
    /// little below the chain tip rounded down to a multiple of 1,000 blocks (so the request
    /// for its tree state names no height unique to this wallet). For a restored wallet pass
    /// the original birthday (`0` picks the recent one).
    pub async fn create(
        name: String,
        network: String,
        mnemonic: String,
        birthday: u32,
    ) -> Result<WebWallet, JsError> {
        Self::create_inner(&name, &network, &mnemonic, birthday)
            .await
            .map_err(js)
    }

    async fn create_inner(
        name: &str,
        network: &str,
        mnemonic: &str,
        birthday: u32,
    ) -> Result<WebWallet> {
        let network = network_of(network)?;
        let seed = seed_of(mnemonic)?;
        let relay = Relay {
            net: network_name(network),
        };
        // Rounded down like a new wallet's, so the tree-state request names no exact height.
        let height = if birthday > 0 {
            (birthday / BIRTHDAY_ROUNDING) * BIRTHDAY_ROUNDING
        } else {
            (relay.tip().await?.saturating_sub(100) / BIRTHDAY_ROUNDING) * BIRTHDAY_ROUNDING
        };
        let tree = relay.tree_state(height.saturating_sub(1)).await?;
        let account_birthday =
            AccountBirthday::from_treestate(tree, None).map_err(|e| anyhow!("birthday: {e:?}"))?;
        let wallet = Wallet::in_memory(name, network, height);
        let keep = wallet.keep_alive()?;
        let mut db = wallet.db()?;
        for (account, _) in [PAY_ACCOUNT, ITEM_ACCOUNT] {
            db.create_account(account, &seed, &account_birthday, None)
                .map_err(|e| anyhow!("creating account {account}: {e}"))?;
        }
        Ok(WebWallet {
            wallet,
            _keep: keep,
            relay,
            cache: MemBlockCache::default(),
            memo_tried: Default::default(),
            offer: None,
            shop: Default::default(),
        })
    }

    /// Open a wallet from the database bytes [`WebWallet::save`] gave.
    pub fn open(
        name: String,
        network: String,
        birthday: u32,
        bytes: &[u8],
    ) -> Result<WebWallet, JsError> {
        (|| {
            let network = network_of(&network)?;
            let wallet = Wallet::in_memory(&name, network, birthday);
            let keep = wallet.keep_alive()?;
            wallet.import(bytes)?;
            // Brings the schema up to date with this build's wallet library.
            wallet.db()?;
            Ok(WebWallet {
                wallet,
                _keep: keep,
                relay: Relay {
                    net: network_name(network),
                },
                cache: MemBlockCache::default(),
                memo_tried: Default::default(),
                offer: None,
                shop: Default::default(),
            })
        })()
        .map_err(js)
    }

    /// The wallet database as bytes, for the page to encrypt and keep.
    pub fn save(&self) -> Result<Vec<u8>, JsError> {
        self.wallet.export().map_err(js)
    }

    /// The network, `main` or `test`.
    pub fn network(&self) -> String {
        network_name(self.wallet.network).into()
    }

    /// Bring the wallet up to date with the chain. `on_progress` gets
    /// `{ scanned, tip }` after each step.
    pub async fn sync(
        &mut self,
        on_progress: Option<js_sys::Function>,
    ) -> Result<JsValue, JsError> {
        let r = self.sync_inner(&on_progress).await.map_err(js)?;
        Ok(js_sys::JSON::parse(&r.to_string()).unwrap_or(JsValue::NULL))
    }

    async fn sync_inner(&mut self, cb: &Option<js_sys::Function>) -> Result<Value> {
        let mut db = self.wallet.db()?;
        // Without an account the wallet would ask for the whole chain from its first block.
        if list_accounts(&db)?.is_empty() {
            return Err(anyhow!(
                "the wallet has no accounts; restore it from its recovery phrase"
            ));
        }
        // The subtree roots let the wallet skip to the parts of the trees its notes are in.
        let sapling = self.relay.roots::<sapling_crypto::Node>("sapling").await?;
        db.put_sapling_subtree_roots(0, &sapling)
            .map_err(|e| anyhow!("{e}"))?;
        let orchard = self
            .relay
            .roots::<orchard::tree::MerkleHashOrchard>("orchard")
            .await?;
        db.put_orchard_subtree_roots(0, &orchard)
            .map_err(|e| anyhow!("{e}"))?;
        let ironwood = self
            .relay
            .roots::<orchard::tree::MerkleHashOrchard>("ironwood")
            .await?;
        db.put_ironwood_subtree_roots(0, &ironwood)
            .map_err(|e| anyhow!("{e}"))?;
        // As the wallet library's own loop: the tip, the range to verify first if there is
        // one, then the suggested ranges in steps, starting over when scanning changes them.
        loop {
            let tip = self.relay.tip().await?;
            db.update_chain_tip(BlockHeight::from(tip))
                .map_err(|e| anyhow!("{e}"))?;
            let mut again = false;
            'ranges: loop {
                let ranges = db.suggest_scan_ranges().map_err(|e| anyhow!("{e}"))?;
                let Some(first) = ranges.first().cloned() else {
                    break 'ranges;
                };
                let verify = first.priority() == ScanPriority::Verify;
                for range in ranges {
                    if verify && range.priority() != ScanPriority::Verify {
                        break;
                    }
                    let mut rest = range;
                    while !rest.is_empty() {
                        let start = rest.block_range().start;
                        let (step, next) = match rest.split_at(start + SYNC_BATCH_BLOCKS) {
                            Some((a, b)) => (a, Some(b)),
                            None => (rest.clone(), None),
                        };
                        if self.scan(&mut db, &step).await? {
                            again = true;
                            continue 'ranges;
                        }
                        progress(
                            cb,
                            json!({ "scanned": u32::from(step.block_range().end) - 1, "tip": tip }),
                        );
                        match next {
                            Some(n) => rest = n,
                            None => break,
                        }
                    }
                }
                if !verify {
                    break 'ranges;
                }
            }
            if !again {
                break;
            }
        }
        let tip = db
            .chain_height()
            .map_err(|e| anyhow!("{e}"))?
            .map(u32::from);
        drop(db);
        // Not worth failing a sync for: names come back on a later one.
        let _ = self.fill_item_memos().await;
        Ok(json!({ "chain_tip": tip, "scanned_to": max_scanned_height(&self.wallet.db()?)? }))
    }

    /// Download and scan one range. Returns whether the suggested ranges changed, so the
    /// caller asks for them again.
    async fn scan(&mut self, db: &mut zsettle_core::wallet::Db, range: &ScanRange) -> Result<bool> {
        let start = u32::from(range.block_range().start);
        let end = u32::from(range.block_range().end) - 1;
        self.cache.clear();
        self.cache.put(self.relay.blocks(start, end).await?);
        let state: ChainState = self
            .relay
            .tree_state(start - 1)
            .await?
            .to_chain_state()
            .map_err(|_| anyhow!("the relay sent a malformed tree state"))?;
        let result = scan_cached_blocks(
            &self.wallet.network,
            &self.cache,
            db,
            range.block_range().start,
            &state,
            range.len(),
        );
        self.cache.clear();
        match result {
            Err(ChainError::Scan(err)) if err.is_continuity_error() => {
                // A reorganisation: go back a little and scan again.
                db.truncate_to_height(err.at_height().saturating_sub(10))
                    .map_err(|e| anyhow!("{e}"))?;
                Ok(true)
            }
            Ok(_) => {
                let latest = db.suggest_scan_ranges().map_err(|e| anyhow!("{e}"))?;
                Ok(latest
                    .first()
                    .is_some_and(|r| r.priority() > range.priority()))
            }
            Err(e) => Err(anyhow!("scanning blocks {start}..={end}: {e}")),
        }
    }

    /// Compact blocks carry no memos, so a restored wallet knows its items but not their
    /// names. For each item note with neither a memo nor a name from its shop, fetch the
    /// whole transaction and decrypt it with the wallet's own keys. Only item notes are
    /// looked up; the shop that sold them, on this server, already knows those transactions.
    async fn fill_item_memos(&mut self) -> Result<()> {
        let missing: Vec<NoteRef> = {
            let db = self.wallet.db()?;
            let item = self.account(&db, ITEM_ACCOUNT.0)?;
            let meta = self.item_meta()?;
            mint::unspent_notes(&self.wallet, &db, item)?
                .into_iter()
                .filter(|n| n.mined_height.is_some() && n.memo.is_none())
                .filter(|n| !meta.contains_key(&n.note.to_string()))
                .filter(|n| !self.memo_tried.contains(&n.note.txid))
                .map(|n| n.note)
                .take(10)
                .collect()
        };
        for note in missing {
            self.memo_tried.insert(note.txid);
            let (height, bytes) = self.relay.raw_tx(&note.txid).await?;
            let branch = BranchId::for_height(&self.wallet.network, BlockHeight::from(height));
            let tx = zcash_primitives::transaction::Transaction::read(&bytes[..], branch)
                .context("the relay sent a malformed transaction")?;
            if tx.txid() != note.txid {
                return Err(anyhow!("the relay sent another transaction"));
            }
            let mut db = self.wallet.db()?;
            zcash_client_backend::data_api::wallet::decrypt_and_store_transaction(
                &self.wallet.network,
                &mut db,
                &tx,
                Some(BlockHeight::from(height)),
            )
            .map_err(|e| anyhow!("{e}"))?;
        }
        Ok(())
    }

    fn account(&self, db: &zsettle_core::wallet::Db, name: &str) -> Result<WalletAccount> {
        let a = list_accounts(db)?
            .into_iter()
            .find(|a| a.name == name && a.index.is_some())
            .ok_or_else(|| anyhow!("the wallet has no {name} account"))?;
        parse_account(&a.account)
    }

    /// A fresh receiving address of the paying account (Orchard only).
    pub fn address(&self) -> Result<String, JsError> {
        (|| {
            let mut db = self.wallet.db()?;
            let account = self.account(&db, PAY_ACCOUNT.0)?;
            mint::item_address(&mut db, self.wallet.network, account)
        })()
        .map_err(js)
    }

    /// Balance, notes and items, for the page to show.
    pub fn view(&self) -> Result<JsValue, JsError> {
        let v = self.view_inner().map_err(js)?;
        Ok(js_sys::JSON::parse(&v.to_string()).unwrap_or(JsValue::NULL))
    }

    fn view_inner(&self) -> Result<Value> {
        let db = self.wallet.db()?;
        let pay = self.account(&db, PAY_ACCOUNT.0)?;
        let item = self.account(&db, ITEM_ACCOUNT.0)?;
        // A reservation ends when its note is spent on chain.
        for (note, _) in reservations(&self.wallet)? {
            if let NoteState::Spent { .. } = mint::note_state(&self.wallet, &db, note)? {
                release_note(&self.wallet, note)?;
            }
        }
        let reserved = reserved_notes(&self.wallet, 0)?;
        let notes: Vec<Value> = mint::unspent_notes(&self.wallet, &db, pay)?
            .into_iter()
            .map(|n| {
                json!({
                    "note": n.note.to_string(), "value_zat": n.value_zat,
                    "mined": n.mined_height, "change": n.change,
                    "reserved": reserved.contains(&n.note),
                })
            })
            .collect();
        let meta = self.item_meta()?;
        let items: Vec<Value> = mint::unspent_notes(&self.wallet, &db, item)?
            .into_iter()
            .map(|n| {
                let key = n.note.to_string();
                let m = meta.get(&key).cloned().unwrap_or(Value::Null);
                let name = m["name"]
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| {
                        n.memo
                            .as_deref()
                            .and_then(|t| t.strip_prefix("zsettle item: "))
                            .map(str::to_string)
                    })
                    .unwrap_or_default();
                json!({
                    "note": key, "name": name, "value_zat": n.value_zat,
                    "mined": n.mined_height, "icon": m["icon"], "description": m["description"],
                    "shop": m["shop"],
                })
            })
            .collect();
        let held = self.held()?;
        let balance: u64 = notes
            .iter()
            .filter(|n| !n["reserved"].as_bool().unwrap_or(false))
            .map(|n| n["value_zat"].as_u64().unwrap_or(0))
            .sum();
        Ok(json!({
            "network": network_name(self.wallet.network),
            "scanned_to": max_scanned_height(&db)?,
            "chain_tip": db.chain_height().map_err(|e| anyhow!("{e}"))?.map(u32::from),
            "birthday": self.wallet.birthday,
            "balance_zat": balance,
            "movable_zat": held.0,
            "waiting_zat": held.1,
            "waiting_until": held.2,
            "notes": notes,
            "items": items,
        }))
    }

    fn item_meta(&self) -> Result<std::collections::HashMap<String, Value>> {
        let conn = self.wallet.connection(false)?;
        conn.execute(ITEM_META_TABLE, [])
            .map_err(|e| anyhow!("{e}"))?;
        let mut stmt = conn
            .prepare("SELECT note, meta FROM zsettle_item_meta")
            .map_err(|e| anyhow!("{e}"))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map_err(|e| anyhow!("{e}"))?;
        Ok(rows
            .into_iter()
            .filter_map(|(k, v)| serde_json::from_str(&v).ok().map(|v| (k, v)))
            .collect())
    }

    fn set_item_meta(&self, note: &str, meta: &Value) -> Result<()> {
        let conn = self.wallet.connection(false)?;
        conn.execute(ITEM_META_TABLE, [])
            .map_err(|e| anyhow!("{e}"))?;
        conn.execute(
            "INSERT OR REPLACE INTO zsettle_item_meta (note, meta) VALUES (?1, ?2)",
            rusqlite::params![note, meta.to_string()],
        )
        .map_err(|e| anyhow!("{e}"))?;
        Ok(())
    }

    /// Pay a unified address from the paying account; returns `{ txid }` once the server
    /// accepted the transaction.
    pub async fn pay(
        &mut self,
        mnemonic: String,
        to: String,
        amount_zat: u64,
        memo: Option<String>,
    ) -> Result<JsValue, JsError> {
        let r = self
            .pay_inner(&mnemonic, &to, amount_zat, memo)
            .await
            .map_err(js)?;
        Ok(js_sys::JSON::parse(&r.to_string()).unwrap_or(JsValue::NULL))
    }

    async fn pay_inner(
        &mut self,
        mnemonic: &str,
        to: &str,
        amount_zat: u64,
        memo: Option<String>,
    ) -> Result<Value> {
        let seed = seed_of(mnemonic)?;
        let mut db = self.wallet.db()?;
        let from = self.account(&db, PAY_ACCOUNT.0)?;
        let next = max_scanned_height(&db)?.unwrap_or(0) + 1;
        self.relay
            .check_consensus(self.wallet.network, next)
            .await?;
        let (bytes, txid) = mint::pay(
            &self.wallet,
            &mut db,
            &seed,
            &PayRequest {
                from_account: from,
                from_account_index: PAY_ACCOUNT.1,
                to: to.trim().to_string(),
                value_zat: amount_zat,
                memo: memo.filter(|m| !m.is_empty()),
            },
        )?;
        self.relay.send(&bytes).await?;
        Ok(json!({ "txid": txid }))
    }

    /// Buy an item from a shop on this server (`/shop/<code>`): fetch and check its signed
    /// listings, take its item key, sync, build and prove the trade, hand the seller its copy,
    /// wait for the seller's signature, sign the payment and send the transaction.
    /// `on_progress` gets `{ step }` as it goes.
    /// `price_zat` and `payout` are what the buyer confirmed: the purchase is refused if the
    /// shop's signed listings say anything else by the time the trade is built. Before the
    /// offer leaves, `on_progress` also gets the database bytes, for the page to save the
    /// reservation of the paying note at once.
    pub async fn buy(
        &mut self,
        mnemonic: String,
        shop: String,
        item_note: String,
        price_zat: u64,
        payout: String,
        on_progress: Option<js_sys::Function>,
    ) -> Result<JsValue, JsError> {
        self.offer = None;
        let r = self
            .buy_inner(
                &mnemonic,
                &shop,
                &item_note,
                price_zat,
                &payout,
                &on_progress,
            )
            .await;
        let r = match (r, self.offer.take()) {
            (Ok(v), _) => v,
            // The offer left but its transaction can no longer be mined: the shop knows the
            // paying note's nullifier, so the note moves to a fresh one now, with the key at
            // hand, before it pays for anything else.
            (Err(e), Some((note, sending)))
                if !sending || format!("{e:#}").contains("rejected") =>
            {
                reserve_note(&self.wallet, note, 0).map_err(js)?;
                let moved = self.rotate_inner(&mnemonic).await;
                let tail = match moved {
                    Ok(t) if !t.is_empty() => "; the coins held for it moved to a fresh note",
                    _ => "; the coins held for it can be moved from the wallet page",
                };
                return Err(JsError::new(&format!("{e:#}{tail}")));
            }
            (Err(e), _) => return Err(js(e)),
        };
        Ok(js_sys::JSON::parse(&r.to_string()).unwrap_or(JsValue::NULL))
    }

    /// Move every held note whose offer can no longer be mined to a fresh note of the paying
    /// account, so the nullifier a shop saw never points at a later payment. Returns
    /// `{ moved: [txid], waiting }` (waiting: held notes whose offers can still settle).
    pub async fn rotate(&mut self, mnemonic: String) -> Result<JsValue, JsError> {
        let moved = self.rotate_inner(&mnemonic).await.map_err(js)?;
        let waiting = self.held().map_err(js)?.1;
        Ok(
            js_sys::JSON::parse(&json!({ "moved": moved, "waiting": waiting }).to_string())
                .unwrap_or(JsValue::NULL),
        )
    }

    async fn rotate_inner(&mut self, mnemonic: &str) -> Result<Vec<String>> {
        let seed = seed_of(mnemonic)?;
        let mut db = self.wallet.db()?;
        let pay = self.account(&db, PAY_ACCOUNT.0)?;
        let tip = max_scanned_height(&db)?.unwrap_or(0);
        let mut moved = Vec::new();
        for (note, expiry) in reservations(&self.wallet)? {
            match mint::note_state(&self.wallet, &db, note)? {
                NoteState::Spent { .. } => release_note(&self.wallet, note)?,
                NoteState::Unspent { .. } if expiry <= tip => {
                    self.relay
                        .check_consensus(self.wallet.network, tip + 1)
                        .await?;
                    let built = build_self_spend(&mut db, self.wallet.network, pay, note, 1)?;
                    let usk = self.wallet.spending_key(&seed, PAY_ACCOUNT.1)?;
                    let ask = SpendAuthorizingKey::from(usk.orchard());
                    let (pczt, signed) = pczt_ops::sign_matching_actions(built.pczt, &ask)?;
                    if signed == 0 {
                        return Err(anyhow!("the key signs no action of the move"));
                    }
                    let tx = pczt_ops::extract(pczt)?;
                    self.relay.send(&pczt_ops::tx_bytes(&tx)?).await?;
                    record_transaction(&self.wallet, &mut db, &tx)?;
                    // Held until the move is mined; then the view releases it as spent.
                    reserve_note(&self.wallet, note, built.expiry_height)?;
                    moved.push(tx.txid().to_string());
                }
                _ => {}
            }
        }
        Ok(moved)
    }

    /// Held notes: the value that can move now, the value still waiting for its offer to
    /// expire, and the height at which the last of those offers expires.
    fn held(&self) -> Result<(u64, u64, u32)> {
        let db = self.wallet.db()?;
        let pay = self.account(&db, PAY_ACCOUNT.0)?;
        let tip = max_scanned_height(&db)?.unwrap_or(0);
        let held = reservations(&self.wallet)?;
        let (mut movable, mut waiting, mut until) = (0, 0, 0);
        for n in mint::unspent_notes(&self.wallet, &db, pay)? {
            if let Some((_, expiry)) = held.iter().find(|(r, _)| *r == n.note) {
                if *expiry <= tip && n.mined_height.is_some() {
                    movable += n.value_zat;
                } else {
                    waiting += n.value_zat;
                    until = until.max(*expiry);
                }
            }
        }
        Ok((movable, waiting, until))
    }

    /// Take a shop's item key ahead of a purchase, so the next sync already looks for its
    /// items and buying does not wait for that scan. Returns the shop's checked listings.
    pub async fn prepare_shop(&mut self, shop: String) -> Result<JsValue, JsError> {
        let (listings, _) = self.shop_key(&shop, &None).await.map_err(js)?;
        Ok(
            js_sys::JSON::parse(&serde_json::to_string(&listings).unwrap_or_default())
                .unwrap_or(JsValue::NULL),
        )
    }

    /// The shop's listings, checked, and the wallet account holding its item key (imported
    /// on first use).
    async fn shop_key(
        &mut self,
        shop: &str,
        cb: &Option<js_sys::Function>,
    ) -> Result<(Listings, WalletAccount)> {
        // Shops on this server only: the page talks to no other site.
        if !shop.starts_with("/shop/") || shop.contains("..") || shop.contains("//") {
            return Err(anyhow!("a shop of this server is named /shop/<code>"));
        }
        let shop = shop.trim_end_matches('/');
        let network = self.wallet.network;
        progress(cb, json!({ "step": "listings" }));
        let text = fetch_ok(&format!("{shop}/listings"), None).await?;
        let listings = Listings::parse(&String::from_utf8_lossy(&text), "the shop's listings")?;
        listings.verify_signature()?;
        if listings.network()? != network {
            return Err(anyhow!("this shop sells on another network"));
        }
        // The shop's item key lets this wallet see the item note and prove its spend.
        let mut db = self.wallet.db()?;
        let account = match find_account_by_ufvk(&db, network, &listings.item_ufvk)? {
            Some(a) => a,
            None => {
                progress(cb, json!({ "step": "import" }));
                // The key is scanned from the block of the oldest listed item, rounded down
                // like a birthday, rather than from the shop's own start: a shop open for
                // weeks would otherwise cost every new buyer a scan of all those weeks.
                let mut oldest: Option<u32> = None;
                for it in &listings.items {
                    if let Ok(n) = NoteRef::from_str(&it.note) {
                        if let Some(h) = self.relay.tx_height(&n.txid).await? {
                            oldest = Some(oldest.map_or(h, |o| o.min(h)));
                        }
                    }
                }
                let start = match oldest {
                    Some(h) => ((h.saturating_sub(1) / BIRTHDAY_ROUNDING) * BIRTHDAY_ROUNDING)
                        .max(listings.item_birthday),
                    None => listings.item_birthday,
                };
                let tree = self.relay.tree_state(start.saturating_sub(1)).await?;
                let name = format!("shop {}", listings.key_fingerprint());
                import_viewing_key_at(&mut db, network, &name, &listings.item_ufvk, tree)?
            }
        };
        Ok((listings, account))
    }

    async fn buy_inner(
        &mut self,
        mnemonic: &str,
        shop: &str,
        item_note: &str,
        price_zat: u64,
        payout: &str,
        cb: &Option<js_sys::Function>,
    ) -> Result<Value> {
        let seed = seed_of(mnemonic)?;
        let item = NoteRef::from_str(item_note)?;
        let network = self.wallet.network;
        let (listings, consignment) = self.shop_key(shop, cb).await?;
        let shop = shop.trim_end_matches('/');
        let listed = listings
            .find(item_note)
            .cloned()
            .ok_or_else(|| anyhow!("the shop no longer lists this item"))?;
        if listed.price_zat != price_zat || listings.payout_address != payout {
            return Err(anyhow!(
                "the shop changed this item's price or payment address since you confirmed; open the shop again"
            ));
        }
        progress(cb, json!({ "step": "sync" }));
        self.sync_inner(cb).await?;

        let mut db = self.wallet.db()?;
        let buyer = self.account(&db, PAY_ACCOUNT.0)?;
        let receive = self.account(&db, ITEM_ACCOUNT.0)?;
        let next_block = max_scanned_height(&db)?.unwrap_or(0) + 1;
        let standard = zsettle_core::standard_expiry_delta(&network, next_block);
        let limit = zsettle_core::reference_blocks(&network, next_block, listed.max_expiry_delta);
        let expiry = standard.min(limit);
        if expiry == 0 {
            return Err(anyhow!("the shop allows no expiry"));
        }
        let (target, _) = heights(&db, MIN_CONFIRMATIONS)?;
        let target_height = u32::from(BlockHeight::from(target));
        self.relay.check_consensus(network, target_height).await?;
        let reserved = reserved_notes(&self.wallet, target_height)?;
        let paying = largest_spendable_note(&db, buyer, target, &reserved)?.ok_or_else(|| {
            anyhow!("no note can pay yet: money received needs 10 confirmations and change 3; notes held for an earlier offer are skipped")
        })?;
        let req = TradeRequest {
            buyer_account: buyer,
            item_account: consignment,
            item_note: item,
            buyer_note: paying,
            seller_address: listings.payout_address.clone(),
            price_zat: listed.price_zat,
            expiry_delta: expiry,
            min_confirmations: MIN_CONFIRMATIONS,
            memo: String::new(),
            item_memo: if listed.name.is_empty() {
                String::new()
            } else {
                format!("zsettle item: {}", listed.name)
            },
            receive_account: Some(receive),
        };
        progress(cb, json!({ "step": "prove" }));
        let built = build_trade(&mut db, network, &req)?;
        let envelope = pczt_ops::to_bytes(&pczt_ops::redact_for_seller(
            built.pczt.clone(),
            built.item_action_index,
        )?)?;
        // Held before the offer leaves: the seller learns this note's nullifier, so it must not
        // pay for a second offer while this one can still settle.
        reserve_note(&self.wallet, paying, built.expiry_height)?;
        self.offer = Some((paying, false));
        // The page saves this at once: a reload while the shop signs must not forget it.
        if let Some(cb) = cb {
            let bytes = js_sys::Uint8Array::from(&self.wallet.export()?[..]);
            let step = js_sys::JSON::parse(r#"{"step":"reserved"}"#).unwrap_or(JsValue::NULL);
            let _ = cb.call2(&JsValue::NULL, &step, &bytes);
        }

        progress(cb, json!({ "step": "offer" }));
        let answer: Value =
            serde_json::from_slice(&fetch_ok(&format!("{shop}/offer"), Some(&envelope)).await?)
                .context("the shop's answer")?;
        let tag = answer["tag"]
            .as_str()
            .ok_or_else(|| anyhow!("the shop's answer names no tag"))?;
        if tag != pczt_ops::envelope_tag(&envelope) {
            return Err(anyhow!(
                "the shop tagged the offer differently; it did not receive it as sent"
            ));
        }
        progress(cb, json!({ "step": "wait" }));
        let started = js_sys::Date::now();
        let signed = loop {
            let (status, body) = fetch(&format!("{shop}/reply/{tag}"), None).await?;
            match status {
                200 => break body,
                202 | 502..=504 => {}
                410 => {
                    return Err(anyhow!(
                        "the shop refused the offer: {}",
                        String::from_utf8_lossy(&body)
                    ))
                }
                s => return Err(anyhow!("unexpected answer {s} from the shop")),
            }
            if js_sys::Date::now() - started > REPLY_WAIT_SECS * 1000.0 {
                return Err(anyhow!(
                    "the shop did not sign in time; the offer expires by itself"
                ));
            }
            sleep_ms(3000).await;
        };

        progress(cb, json!({ "step": "sign" }));
        let combined = pczt_ops::combine(built.pczt, pczt_ops::from_bytes(&signed)?)?;
        let usk = self.wallet.spending_key(&seed, PAY_ACCOUNT.1)?;
        let ask = SpendAuthorizingKey::from(usk.orchard());
        let (pczt, n) = pczt_ops::sign_matching_actions(combined, &ask)?;
        if n == 0 {
            return Err(anyhow!("this wallet signs none of the trade's actions"));
        }
        let tx = pczt_ops::extract(pczt)?;
        let bytes = pczt_ops::tx_bytes(&tx)?;
        progress(cb, json!({ "step": "send" }));
        self.offer = Some((paying, true));
        self.relay.send(&bytes).await?;
        record_transaction(&self.wallet, &mut db, &tx)?;
        // The new item note is the receiving account's output of this transaction.
        let txid = tx.txid().to_string();
        let shop_name = shop.trim_start_matches("/shop/");
        if let Some(n) = mint::unspent_notes(&self.wallet, &db, receive)?
            .into_iter()
            .find(|n| n.note.txid.to_string() == txid)
        {
            self.set_item_meta(
                &n.note.to_string(),
                &json!({
                    "name": listed.name, "icon": listed.icon, "description": listed.description,
                    "shop": shop_name, "bought_from": item_note,
                }),
            )?;
        }
        Ok(json!({
            "txid": txid, "price_zat": listed.price_zat, "fee_zat": built.fee,
            "name": listed.name, "expiry_height": built.expiry_height,
        }))
    }

    /// A signed proof that this wallet holds an item note now, naming the last scanned block.
    /// Sync first so the block is recent.
    pub fn prove(
        &self,
        mnemonic: String,
        item_note: String,
        label: Option<String>,
        challenge: Option<String>,
    ) -> Result<String, JsError> {
        (|| {
            let label = label.filter(|l| !l.trim().is_empty());
            let challenge = challenge.filter(|c| !c.trim().is_empty());
            if let Some(l) = &label {
                check_label(l)?;
            }
            if let Some(c) = &challenge {
                check_challenge(c)?;
            }
            let note = NoteRef::from_str(&item_note)?;
            let db = self.wallet.db()?;
            let account = self.account(&db, ITEM_ACCOUNT.0)?;
            let held = mint::unspent_notes(&self.wallet, &db, account)?
                .into_iter()
                .find(|n| n.note == note)
                .ok_or_else(|| anyhow!("this wallet does not hold that item"))?;
            if held.mined_height.is_none() {
                return Err(anyhow!(
                    "the item is not in a block yet; wait for one and try again"
                ));
            }
            let seed = seed_of(&mnemonic)?;
            let usk = self.wallet.spending_key(&seed, ITEM_ACCOUNT.1)?;
            let fvk = account_orchard_fvk(&db, account)?;
            if orchard::keys::FullViewingKey::from(usk.orchard()) != fvk {
                return Err(anyhow!("the recovery phrase is not this wallet's"));
            }
            let (height, hash) =
                scanned_tip(&db)?.ok_or_else(|| anyhow!("sync the wallet first"))?;
            let mut proof = OwnershipProof::new(
                self.wallet.network,
                &fvk,
                account_birthday(&db, account)?,
                note,
                (height, &hash),
                label.as_deref(),
                challenge.as_deref(),
            )?;
            proof.sign(&SpendAuthorizingKey::from(usk.orchard()))?;
            proof.to_json()
        })()
        .map_err(js)
    }

    /// Check that a recovery phrase is this wallet's (its paying account's key matches).
    pub fn check_phrase(&self, mnemonic: String) -> Result<(), JsError> {
        (|| {
            let seed = seed_of(&mnemonic)?;
            let db = self.wallet.db()?;
            let account = self.account(&db, PAY_ACCOUNT.0)?;
            let usk = self.wallet.spending_key(&seed, PAY_ACCOUNT.1)?;
            if orchard::keys::FullViewingKey::from(usk.orchard())
                != account_orchard_fvk(&db, account)?
            {
                return Err(anyhow!("the recovery phrase is not this wallet's"));
            }
            Ok(())
        })()
        .map_err(js)
    }
}

/// Confirmations a buyer's paying note needs, as the command-line `buy` asks by default.
const MIN_CONFIRMATIONS: u32 = 3;

const ITEM_META_TABLE: &str =
    "CREATE TABLE IF NOT EXISTS zsettle_item_meta (note TEXT PRIMARY KEY, meta TEXT NOT NULL)";
