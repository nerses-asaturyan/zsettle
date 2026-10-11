//! `zsettle-seller`: the seller's half. Publishes signed listings, checks incoming trade
//! envelopes against policy, verifies their proofs, and signs only the item action.

use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, SystemTime},
};

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use orchard::circuit::VerifyingKey;
use orchard::keys::{FullViewingKey, SpendAuthorizingKey};
use orchard::note::Nullifier;
use zcash_keys::address::Address;
use zcash_keys::address::UnifiedAddress;
use zcash_protocol::consensus::{BlockHeight, BranchId};
use zsettle_core::broadcast::{check_consensus_blocking, pick_server, server_info_blocking};
use zsettle_core::listing::{ListedItem, Listings};
use zsettle_core::mint::{note_state, NoteState};
use zsettle_core::pczt as pczt_ops;
use zsettle_core::policy::{check, describe, prepared_ivk, Approved, PolicyError, SellerPolicy};
use zsettle_core::wallet::{
    account_birthday, account_orchard_fvk, create_account, heights, ironwood_note,
    max_scanned_height, next_orchard_address, parse_account, recent_ironwood_anchors,
    sync_blocking, unspent_ironwood_nullifiers, Db, NoteRef, Wallet,
};
use zsettle_core::DEFAULT_MAX_EXPIRY_DELTA;
use zsettle_core::{MAX_ENVELOPE_BYTES, MIN_PRICE_ZAT};

/// How many recent blocks' tree roots count as acceptable anchors (the wallet keeps that
/// many checkpoints).
const ANCHOR_DEPTH: u32 = 100;
/// Longest pause between passes after repeated failures.
const MAX_BACKOFF_SECS: u64 = 300;
/// Inbox files modified more recently than this are left for the next pass: they may still be
/// in the middle of being written.
const SETTLE_SECS: u64 = 2;

#[derive(Parser)]
#[command(
    name = "zsettle-seller",
    version,
    about = "Seller service for zsettle trades"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

/// Arguments shared by every command that touches the seller wallet.
#[derive(clap::Args, Clone)]
struct WalletArgs {
    /// Seller wallet directory (devtool layout).
    #[arg(long)]
    wallet: PathBuf,
    /// UUID of the item account in this wallet.
    #[arg(long)]
    item_account: String,
    /// ZIP 32 account index of the item account (used to derive its signing key).
    #[arg(long, default_value_t = 1)]
    item_account_index: u32,
    /// UUID of the account that receives payments.
    #[arg(long)]
    payout_account: String,
}

#[derive(Subcommand)]
enum Cmd {
    /// Sync the wallet with a lightwalletd server (no other tool needed).
    Sync {
        /// Wallet directory.
        #[arg(long)]
        wallet: PathBuf,
        /// lightwalletd server URL (default: the zec.rocks endpoint of the wallet's network).
        #[arg(long)]
        server: Option<String>,
    },
    /// Bring the wallet database schema up to date, with the seed for migrations that need it.
    Upgrade {
        /// Wallet directory.
        #[arg(long)]
        wallet: PathBuf,
        /// age identity file that decrypts the wallet's mnemonic.
        #[arg(long)]
        identity: PathBuf,
    },
    /// Create a new item account in this wallet. One account per item keeps a consignment to
    /// that item alone: whoever holds its listing sees nothing of your other items.
    NewItemAccount {
        /// Wallet directory.
        #[arg(long)]
        wallet: PathBuf,
        /// age identity file that decrypts the wallet's mnemonic.
        #[arg(long)]
        identity: PathBuf,
        /// Account name.
        #[arg(long)]
        name: String,
        /// lightwalletd server URL (for the account birthday; defaults to the wallet's network).
        #[arg(long)]
        server: Option<String>,
    },
    /// Mint an item: pay a small amount from the payout account to the item account, memo = name.
    Mint {
        #[command(flatten)]
        w: WalletArgs,
        /// age identity file that decrypts the seller wallet's mnemonic.
        #[arg(long)]
        identity: PathBuf,
        /// ZIP 32 account index of the payout account.
        #[arg(long, default_value_t = 0)]
        payout_account_index: u32,
        /// Item name (goes into the memo).
        #[arg(long)]
        name: String,
        /// Item note value in zatoshis.
        #[arg(long, default_value_t = zsettle_core::item::MIN_ITEM_VALUE)]
        value: u64,
        /// lightwalletd server URL (default: the zec.rocks endpoint of the wallet's network).
        #[arg(long)]
        server: Option<String>,
    },
    /// List the item account's unspent notes (the references to publish).
    Items {
        #[command(flatten)]
        w: WalletArgs,
    },
    /// Create or update the signed public listings file for this wallet's item account.
    Publish {
        #[command(flatten)]
        w: WalletArgs,
        /// age identity file that decrypts the seller wallet's mnemonic (signs the file).
        #[arg(long)]
        identity: PathBuf,
        /// Listings file to create or update.
        #[arg(long)]
        listings: PathBuf,
        /// Item note to list, as <txid>:<output index>. Without it, the existing file is only
        /// checked and signed again (after a format change, or a manual edit you do intend).
        #[arg(long)]
        item_note: Option<NoteRef>,
        /// Display name.
        #[arg(long, default_value = "")]
        name: String,
        /// Price in zatoshis (more than 5,000: smaller payments are dust no wallet spends).
        #[arg(long)]
        price: Option<u64>,
        /// Maximum blocks between the chain tip and the transaction expiry.
        #[arg(long, default_value_t = zsettle_core::DEFAULT_MAX_EXPIRY_DELTA)]
        max_expiry_delta: u32,
    },
    /// Check one trade PCZT against the listings, verify its proof, and sign the item action.
    Sign {
        #[command(flatten)]
        w: WalletArgs,
        /// age identity file that decrypts the seller wallet's mnemonic.
        #[arg(long)]
        identity: PathBuf,
        /// Listings file.
        #[arg(long)]
        listings: PathBuf,
        /// Incoming (redacted) trade PCZT.
        #[arg(long)]
        input: PathBuf,
        /// Where to write the signed PCZT.
        #[arg(long)]
        out: PathBuf,
        /// Check and report only, never sign.
        #[arg(long)]
        dry_run: bool,
        /// lightwalletd server URL for the sync that precedes the check (default: the zec.rocks endpoint of the wallet's network).
        #[arg(long)]
        server: Option<String>,
        /// Do not sync the wallet first.
        #[arg(long)]
        no_sync: bool,
    },
    /// Watch an inbox directory, sign every acceptable trade, and write results to an outbox.
    ///
    /// Each pass syncs the wallet, removes sold items from the listings (re-signing the
    /// file), then handles every envelope in the inbox: policy check, proof verification,
    /// signature of the item action. Every acceptable offer is signed; the chain settles the
    /// first one to be mined. A failed pass is logged and retried with backoff; the service
    /// does not exit on its own.
    Serve {
        #[command(flatten)]
        w: WalletArgs,
        /// age identity file that decrypts the seller wallet's mnemonic.
        #[arg(long)]
        identity: PathBuf,
        /// Listings file (kept in sync with the chain: sold items removed).
        #[arg(long)]
        listings: PathBuf,
        /// Directory watched for `*.pczt` envelopes.
        #[arg(long)]
        inbox: PathBuf,
        /// Directory where `<name>.<tag>.signed.pczt` replies are written.
        #[arg(long)]
        outbox: PathBuf,
        /// Seconds between passes.
        #[arg(long, default_value_t = 5)]
        poll_secs: u64,
        /// lightwalletd server URL (default: the zec.rocks endpoint of the wallet's network).
        #[arg(long)]
        server: Option<String>,
        /// Do not sync; something else keeps the wallet up to date.
        #[arg(long)]
        no_sync: bool,
        /// Shell command to run instead of the built-in sync.
        #[arg(long)]
        sync_cmd: Option<String>,
        /// Run one pass and exit; a failed pass exits with an error.
        #[arg(long)]
        once: bool,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(if std::env::var_os("ZSETTLE_LOG").is_some() {
            tracing::Level::INFO
        } else {
            tracing::Level::WARN
        })
        .init();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Sync { wallet, server } => {
            let wallet = Wallet::open(&wallet)?;
            let server = pick_server(server, wallet.network);
            let mut db = wallet.db()?;
            let report = sync_blocking(&wallet, &mut db, &server)?;
            println!("{}", serde_json::to_string(&report)?);
            Ok(())
        }
        Cmd::Upgrade { wallet, identity } => {
            let w = Wallet::open(&wallet)?;
            w.upgrade(&identity)?;
            println!(
                "{}",
                serde_json::json!({ "wallet": wallet, "upgraded": true })
            );
            Ok(())
        }
        Cmd::NewItemAccount {
            wallet,
            identity,
            name,
            server,
        } => {
            let wallet = Wallet::open(&wallet)?;
            let mut db = wallet.db()?;
            let server = pick_server(server, wallet.network);
            let seed = wallet.seed(&identity)?;
            let (id, index) = create_account(&wallet, &mut db, &seed, &name, &server)?;
            println!(
                "{}",
                serde_json::json!({ "account": id.expose_uuid().to_string(), "index": index, "name": name, "next": "mint into it with --item-account and --item-account-index" })
            );
            Ok(())
        }
        Cmd::Mint {
            w,
            identity,
            payout_account_index,
            name,
            value,
            server,
        } => {
            let wallet = Wallet::open(&w.wallet)?;
            let server = pick_server(server, wallet.network);
            let mut db = wallet.db()?;
            let info = server_info_blocking(&server)?;
            check_consensus_blocking(&server, wallet.network, info.block_height.saturating_add(1))?;
            let seed = wallet.seed(&identity)?;
            let req = zsettle_core::mint::MintRequest {
                from_account: parse_account(&w.payout_account)?,
                from_account_index: payout_account_index,
                item_account: parse_account(&w.item_account)?,
                value_zat: value,
                name: name.clone(),
            };
            let (bytes, txid) = zsettle_core::mint::mint(&wallet, &mut db, &seed, &req)?;
            let response = zsettle_core::broadcast::send_raw_transaction_blocking(&server, bytes)?;
            println!(
                "{}",
                serde_json::json!({ "txid": txid, "name": name, "value_zat": value, "broadcast": response })
            );
            Ok(())
        }
        Cmd::Items { w } => {
            let wallet = Wallet::open(&w.wallet)?;
            let db = wallet.db()?;
            let notes =
                zsettle_core::mint::unspent_notes(&wallet, &db, parse_account(&w.item_account)?)?;
            for n in &notes {
                println!(
                    "{}  {:>8} zat  mined {}",
                    n.note,
                    n.value_zat,
                    n.mined_height
                        .map(|h| h.to_string())
                        .unwrap_or_else(|| "pending".into())
                );
            }
            if notes.is_empty() {
                println!("(no unspent notes in the item account)");
            }
            Ok(())
        }
        Cmd::Publish {
            w,
            identity,
            listings,
            item_note,
            name,
            price,
            max_expiry_delta,
        } => {
            let wallet = Wallet::open(&w.wallet)?;
            let mut db = wallet.db()?;
            let item_account = parse_account(&w.item_account)?;
            let item_fvk = account_orchard_fvk(&db, item_account)?;
            let ask = item_ask(&wallet, &db, &identity, item_account, w.item_account_index)?;
            let mut l = if listings.exists() {
                let l = Listings::read(&listings)?;
                if l.item_fvk()? != item_fvk {
                    return Err(anyhow!(
                        "{} belongs to another item account",
                        listings.display()
                    ));
                }
                if item_note.is_some() {
                    l.verify_signature()
                        .with_context(|| format!("{} is not trustworthy; publish without --item-note to sign it again if you edited it on purpose", listings.display()))?;
                }
                l
            } else {
                if item_note.is_none() {
                    return Err(anyhow!(
                        "{} does not exist; pass --item-note to create it",
                        listings.display()
                    ));
                }
                // A payout address derived for the market, not the account's default address.
                let payout = next_orchard_address(&mut db, parse_account(&w.payout_account)?)?;
                let payout_ua =
                    UnifiedAddress::from_receivers(Some(payout), None, None, None, None)
                        .ok_or_else(|| anyhow!("cannot build payout address"))?;
                Listings {
                    version: 1,
                    network: network_name(wallet.network).into(),
                    item_ufvk: Listings::encode_item_key(&item_fvk, wallet.network)?,
                    item_birthday: account_birthday(&db, item_account)?,
                    payout_address: payout_ua.encode(&wallet.network),
                    items: vec![],
                    issued_at: 0,
                    signature: None,
                }
            };
            check_payout(&l, &payout_key(&db, &w)?, wallet.network)?;
            if let Some(item_note) = item_note {
                let price = price.ok_or_else(|| anyhow!("--price is required with --item-note"))?;
                if price < MIN_PRICE_ZAT {
                    return Err(anyhow!(
                        "price must be at least {MIN_PRICE_ZAT} zat: a payment at or below the ZIP 317 marginal fee is dust that no wallet spends"
                    ));
                }
                if max_expiry_delta != DEFAULT_MAX_EXPIRY_DELTA {
                    eprintln!(
                        "warning: an expiry limit other than {DEFAULT_MAX_EXPIRY_DELTA} blocks makes every trade of this item recognisable on chain"
                    );
                }
                // Make sure the note exists and is ours before listing it.
                let (target, _) = heights(&db, 1)?;
                let note = ironwood_note(&db, item_note, target)?;
                if item_fvk.scope_for_address(&note.note.recipient()).is_none() {
                    return Err(anyhow!("note {item_note} is not owned by the item account"));
                }
                l.upsert(ListedItem {
                    note: item_note.to_string(),
                    name,
                    price_zat: price,
                    max_expiry_delta,
                });
            }
            l.sign(&ask, max_scanned_height(&db)?.unwrap_or(0))?;
            l.verify_signature()?;
            l.write(&listings)?;
            println!(
                "{}",
                serde_json::json!({ "listings": listings, "items": l.items.len(), "signed": true, "issued_at": l.issued_at, "key_fingerprint": l.key_fingerprint() })
            );
            Ok(())
        }
        Cmd::Sign {
            w,
            identity,
            listings,
            input,
            out,
            dry_run,
            server,
            no_sync,
        } => {
            let wallet = Wallet::open(&w.wallet)?;
            let server = pick_server(server, wallet.network);
            let mut db = wallet.db()?;
            if !no_sync {
                sync_blocking(&wallet, &mut db, &server)?;
            }
            let anchors = recent_ironwood_anchors(&mut db, ANCHOR_DEPTH)?;
            let l = Listings::read_verified(&listings)?;
            check_payout(&l, &payout_key(&db, &w)?, wallet.network)?;
            let bytes = fs::read(&input).with_context(|| format!("reading {}", input.display()))?;
            let pczt = pczt_ops::from_bytes(&bytes)?;
            let vk = pczt_ops::verifying_key();
            pczt_ops::verify_proof(&pczt, &vk).context("refused: proof")?;
            println!("proof verified");
            let outcome = process(&db, &w, &l, &pczt, &anchors)?;
            println!("{}", describe(&outcome.result));
            let (item, approved) = match outcome.result {
                Ok(a) => (outcome.item.expect("matched item"), a),
                Err(e) => return Err(zsettle_core::policy::into_anyhow(e)),
            };
            if dry_run {
                return Ok(());
            }
            let ask = item_ask(
                &wallet,
                &db,
                &identity,
                parse_account(&w.item_account)?,
                w.item_account_index,
            )?;
            let signed = pczt_ops::sign_ironwood_action(pczt, approved.item_action_index, &ask)?;
            fs::write(&out, pczt_ops::to_bytes(&signed)?)
                .with_context(|| format!("writing {}", out.display()))?;
            println!(
                "{}",
                serde_json::json!({
                    "item": item.note,
                    "signed_action": approved.item_action_index,
                    "paid_zat": approved.paid_zat,
                    "expiry_height": approved.expiry_height,
                    "out": out,
                })
            );
            Ok(())
        }
        Cmd::Serve {
            w,
            identity,
            listings,
            inbox,
            outbox,
            poll_secs,
            server,
            no_sync,
            sync_cmd,
            once,
        } => {
            for dir in [
                inbox.clone(),
                inbox.join("done"),
                inbox.join("rejected"),
                outbox.clone(),
            ] {
                fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
            }
            // Configuration errors stop the service before it starts; everything after is retried.
            let wallet = Wallet::open(&w.wallet)?;
            let server = pick_server(server, wallet.network);
            {
                let db = wallet.db()?;
                account_orchard_fvk(&db, parse_account(&w.item_account)?)?;
                payout_key(&db, &w)?;
            }
            let ask = {
                let db = wallet.db()?;
                item_ask(
                    &wallet,
                    &db,
                    &identity,
                    parse_account(&w.item_account)?,
                    w.item_account_index,
                )
                .context("the identity file does not decrypt this wallet's mnemonic")?
            };
            let l = Listings::read_verified(&listings)?;
            {
                let db = wallet.db()?;
                check_payout(&l, &payout_key(&db, &w)?, wallet.network)?;
            }
            let vk = pczt_ops::verifying_key();
            println!(
                "{}",
                serde_json::json!({
                    "event": "start",
                    "listed": l.items.len(),
                    "key_fingerprint": l.key_fingerprint(),
                    "inbox": inbox,
                    "outbox": outbox,
                    "sync": if sync_cmd.is_some() { "command" } else if no_sync { "off" } else { "built-in" },
                    "server": server,
                })
            );
            let service = Service {
                w: &w,
                ask: &ask,
                vk: &vk,
                listings: &listings,
                inbox: &inbox,
                outbox: &outbox,
                server: &server,
                no_sync,
                sync_cmd: sync_cmd.as_deref(),
            };
            let poll = poll_secs.max(1);
            let mut delay = poll;
            loop {
                match service.pass() {
                    Ok(()) => delay = poll,
                    Err(e) => {
                        println!(
                            "{}",
                            serde_json::json!({ "event": "error", "error": format!("{e:#}"), "retry_in_secs": delay })
                        );
                        if once {
                            return Err(e);
                        }
                        thread::sleep(Duration::from_secs(delay));
                        delay = (delay * 2).min(MAX_BACKOFF_SECS);
                        continue;
                    }
                }
                if once {
                    return Ok(());
                }
                thread::sleep(Duration::from_secs(delay));
            }
        }
    }
}

/// One running seller service.
struct Service<'a> {
    w: &'a WalletArgs,
    ask: &'a SpendAuthorizingKey,
    vk: &'a VerifyingKey,
    listings: &'a Path,
    inbox: &'a Path,
    outbox: &'a Path,
    server: &'a str,
    no_sync: bool,
    sync_cmd: Option<&'a str>,
}

impl Service<'_> {
    /// One pass: sync, reconcile the listings with the chain, handle every envelope. An
    /// envelope that cannot be handled is logged and left for the next pass; it never fails
    /// the pass for the others.
    fn pass(&self) -> Result<()> {
        if let Some(cmd) = self.sync_cmd {
            run_sync_command(cmd)?;
        }
        let wallet = Wallet::open(&self.w.wallet)?;
        let mut db = wallet.db()?;
        if self.sync_cmd.is_none() && !self.no_sync {
            sync_blocking(&wallet, &mut db, self.server).context("syncing the seller wallet")?;
        }
        let anchors = recent_ironwood_anchors(&mut db, ANCHOR_DEPTH)?;
        let mut l = Listings::read_verified(self.listings)?;
        if reconcile(&wallet, &db, &mut l)? {
            l.sign(self.ask, max_scanned_height(&db)?.unwrap_or(0))?;
            l.write(self.listings)?;
        }
        for path in self.envelopes()? {
            if let Err(e) = self.handle(&db, &l, &anchors, &path) {
                println!(
                    "{}",
                    serde_json::json!({ "event": "error", "file": path, "error": format!("{e:#}") })
                );
            }
        }
        Ok(())
    }

    /// Envelopes waiting in the inbox, oldest name first, skipping files still being written.
    fn envelopes(&self) -> Result<Vec<PathBuf>> {
        let now = SystemTime::now();
        let mut paths: Vec<PathBuf> = fs::read_dir(self.inbox)
            .with_context(|| format!("reading {}", self.inbox.display()))?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file() && e.path().extension().is_some_and(|x| x == "pczt"))
            .filter(|e| settled(e, now))
            .map(|e| e.path())
            .collect();
        paths.sort();
        Ok(paths)
    }

    /// Handle one envelope: sign it or reject it, record the outcome, move the file. The
    /// reply is named after the envelope and a tag of its content, so two envelopes with
    /// the same name never collide and the same envelope always gets the same reply.
    fn handle(
        &self,
        db: &Db,
        l: &Listings,
        anchors: &[orchard::Anchor],
        path: &Path,
    ) -> Result<()> {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("trade")
            .to_string();
        let size = fs::metadata(path)
            .with_context(|| format!("reading {}", path.display()))?
            .len();
        if size > MAX_ENVELOPE_BYTES {
            return self.reject(path, &stem, format!("too large: {size} bytes"));
        }
        let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let tag = pczt_ops::envelope_tag(&bytes);
        let out = self.outbox.join(format!("{stem}.{tag}.signed.pczt"));
        if out.exists() {
            // Signed in an earlier pass that stopped before moving the envelope.
            println!(
                "{}",
                serde_json::json!({ "event": "already_signed", "file": path, "out": out })
            );
            return move_into(path, &self.inbox.join("done"));
        }
        match self.sign_envelope(db, l, anchors, &bytes) {
            Ok((item, approved, signed)) => {
                write_atomic(&out, &signed)?;
                move_into(path, &self.inbox.join("done"))?;
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "signed", "file": path, "item": item.note, "name": item.name,
                        "signed_action": approved.item_action_index, "paid_zat": approved.paid_zat,
                        "expiry_height": approved.expiry_height, "out": out,
                    })
                );
                Ok(())
            }
            Err(reason) => self.reject(path, &stem, reason),
        }
    }

    fn reject(&self, path: &Path, stem: &str, reason: String) -> Result<()> {
        println!(
            "{}",
            serde_json::json!({ "event": "rejected", "file": path, "reason": reason })
        );
        let rejected = self.inbox.join("rejected");
        move_into(path, &rejected)?;
        fs::write(rejected.join(format!("{stem}.reason.txt")), reason)?;
        Ok(())
    }

    /// Parse, check, verify the proof of, and sign one envelope. The error is the reason
    /// recorded for the buyer.
    fn sign_envelope(
        &self,
        db: &Db,
        l: &Listings,
        anchors: &[orchard::Anchor],
        bytes: &[u8],
    ) -> std::result::Result<(ListedItem, Approved, Vec<u8>), String> {
        let pczt = pczt_ops::from_bytes(bytes).map_err(|e| format!("unreadable: {e:#}"))?;
        pczt_ops::verify_proof(&pczt, self.vk).map_err(|e| format!("refused: {e:#}"))?;
        let outcome =
            process(db, self.w, l, &pczt, anchors).map_err(|e| format!("error: {e:#}"))?;
        let approved = outcome.result.map_err(|e| format!("refused: {e}"))?;
        let item = outcome
            .item
            .ok_or_else(|| "error: approved trade without a listed item".to_string())?;
        let signed = pczt_ops::sign_ironwood_action(pczt, approved.item_action_index, self.ask)
            .map_err(|e| format!("signing failed: {e:#}"))?;
        let bytes = pczt_ops::to_bytes(&signed).map_err(|e| format!("signing failed: {e:#}"))?;
        Ok((item, approved, bytes))
    }
}

/// Remove from the listings every item whose note the wallet has recorded as spent. A
/// note the wallet cannot see right now (reorged, unscanned, reset wallet) stays listed.
/// Returns whether anything changed.
fn reconcile(wallet: &Wallet, db: &Db, l: &mut Listings) -> Result<bool> {
    let mut changed = false;
    let mut kept = Vec::with_capacity(l.items.len());
    for item in std::mem::take(&mut l.items) {
        let Ok(r) = item.note_ref() else {
            println!(
                "{}",
                serde_json::json!({ "event": "dropped", "item": item.note, "reason": "bad note reference" })
            );
            changed = true;
            continue;
        };
        match note_state(wallet, db, r)? {
            NoteState::Spent { txid } => {
                println!(
                    "{}",
                    serde_json::json!({ "event": "sold", "item": item.note, "name": item.name, "txid": txid })
                );
                changed = true;
            }
            NoteState::Unknown => {
                println!(
                    "{}",
                    serde_json::json!({ "event": "not_in_wallet", "item": item.note, "name": item.name })
                );
                kept.push(item);
            }
            NoteState::Unspent { .. } => kept.push(item),
        }
    }
    l.items = kept;
    if changed {
        l.signature = None;
    }
    Ok(changed)
}

struct Outcome {
    item: Option<ListedItem>,
    result: std::result::Result<Approved, PolicyError>,
}

/// Match a trade against the listings and run the policy checks.
fn process(
    db: &Db,
    w: &WalletArgs,
    l: &Listings,
    pczt: &pczt::Pczt,
    anchors: &[orchard::Anchor],
) -> Result<Outcome> {
    let (target, _) = heights(db, 1)?;
    let tip: u32 =
        u32::from(zcash_protocol::consensus::BlockHeight::from(target)).saturating_sub(1);
    let item_fvk = account_orchard_fvk(db, parse_account(&w.item_account)?)?;
    let seller_nfs = unspent_ironwood_nullifiers(db)?;
    let trade_nfs = pczt_ops::action_nullifiers(pczt)?;
    // Which listed item does this trade spend?
    let mut matched: Option<(ListedItem, Nullifier)> = None;
    let mut unavailable = 0usize;
    for item in &l.items {
        let Ok(r) = item.note_ref() else { continue };
        let Ok(n) = ironwood_note(db, r, target) else {
            unavailable += 1;
            continue;
        };
        if item_fvk.scope_for_address(&n.note.recipient()).is_none() {
            continue;
        }
        let nf = n.note.nullifier(&item_fvk);
        if trade_nfs.contains(&nf) {
            matched = Some((item.clone(), nf));
            break;
        }
    }
    let Some((item, nf)) = matched else {
        if unavailable > 0 {
            return Err(anyhow!(
                "the trade does not spend any listed item that is spendable in this wallet ({unavailable} listed items are not spendable here right now: sold, unscanned, or too recent)"
            ));
        }
        return Ok(Outcome {
            item: None,
            result: Err(PolicyError::ItemNotSpent),
        });
    };
    let ivk = prepared_ivk(&payout_key(db, w)?);
    let network = Wallet::open(&w.wallet)?.network;
    let policy = SellerPolicy {
        price_zat: item.price_zat,
        max_expiry_delta: item.max_expiry_delta,
        tip,
        branch_id: u32::from(BranchId::for_height(&network, BlockHeight::from(tip + 1))),
    };
    let result = check(pczt, &nf, &seller_nfs, &ivk, &policy, anchors);
    Ok(Outcome {
        item: Some(item),
        result,
    })
}

/// The item account's spend authorizing key, derived from the encrypted seed, checked
/// against the account's viewing key so a wrong index fails here and not at the buyer.
fn item_ask(
    wallet: &Wallet,
    db: &Db,
    identity: &Path,
    item_account: zsettle_core::wallet::WalletAccount,
    item_account_index: u32,
) -> Result<SpendAuthorizingKey> {
    let seed = wallet.seed(identity)?;
    let usk = wallet.spending_key(&seed, item_account_index)?;
    let derived = FullViewingKey::from(usk.orchard());
    if derived != account_orchard_fvk(db, item_account)? {
        return Err(anyhow!(
            "account index {item_account_index} does not derive the item account {}; pass the right --item-account-index",
            item_account.expose_uuid()
        ));
    }
    Ok(SpendAuthorizingKey::from(usk.orchard()))
}

/// The listings' payout address must belong to the payout account this service decrypts
/// payments with; otherwise every offer would be refused as unpaid.
fn check_payout(
    l: &Listings,
    payout_fvk: &FullViewingKey,
    network: zcash_protocol::consensus::Network,
) -> Result<()> {
    let addr = match Address::decode(&network, &l.payout_address) {
        Some(Address::Unified(ua)) => ua.orchard().copied(),
        _ => None,
    }
    .ok_or_else(|| {
        anyhow!("the listings payout address is not a unified address with an Orchard receiver")
    })?;
    if payout_fvk.scope_for_address(&addr).is_none() {
        return Err(anyhow!(
            "the listings payout address does not belong to --payout-account; payments would never be recognised"
        ));
    }
    Ok(())
}

fn payout_key(db: &Db, w: &WalletArgs) -> Result<FullViewingKey> {
    account_orchard_fvk(db, parse_account(&w.payout_account)?)
}

fn run_sync_command(cmd: &str) -> Result<()> {
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .status()
        .context("starting the sync command")?;
    if !status.success() {
        return Err(anyhow!("sync command exited with {status}"));
    }
    Ok(())
}

/// True when the file has not been modified for `SETTLE_SECS`.
fn settled(entry: &fs::DirEntry, now: SystemTime) -> bool {
    match entry.metadata().and_then(|m| m.modified()) {
        Ok(modified) => now
            .duration_since(modified)
            .is_ok_and(|age| age >= Duration::from_secs(SETTLE_SECS)),
        Err(_) => true,
    }
}

/// Write a file through a temporary name so readers never see a partial file.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
}

fn move_into(path: &Path, dir: &Path) -> Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| anyhow!("{} has no file name", path.display()))?;
    fs::rename(path, dir.join(name))
        .with_context(|| format!("moving {} to {}", path.display(), dir.display()))
}

fn network_name(n: zcash_protocol::consensus::Network) -> &'static str {
    match n {
        zcash_protocol::consensus::Network::MainNetwork => "main",
        zcash_protocol::consensus::Network::TestNetwork => "test",
    }
}
