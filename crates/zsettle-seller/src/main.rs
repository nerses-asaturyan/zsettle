//! `zsettle-seller`: the seller's half. Publishes listings, checks incoming trade envelopes
//! against policy, and signs only the item action.

use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, SystemTime},
};

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use orchard::keys::{FullViewingKey, Scope, SpendAuthorizingKey};
use orchard::note::Nullifier;
use zcash_keys::address::UnifiedAddress;
use zsettle_core::listing::{ListedItem, Listings, Pending};
use zsettle_core::pczt as pczt_ops;
use zsettle_core::policy::{check, describe, prepared_ivk, Approved, PolicyError, SellerPolicy};
use zsettle_core::wallet::{
    account_ufvk, find_ironwood_note, heights, ironwood_note, max_scanned_height, parse_account,
    sync_blocking, unspent_ironwood_nullifiers, Db, NoteRef, Wallet,
};

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
        /// lightwalletd server URL.
        #[arg(long, default_value = zsettle_core::broadcast::DEFAULT_SERVER)]
        server: String,
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
        /// lightwalletd server URL.
        #[arg(long, default_value = zsettle_core::broadcast::DEFAULT_SERVER)]
        server: String,
    },
    /// List the item account's unspent notes (the references to publish).
    Items {
        #[command(flatten)]
        w: WalletArgs,
    },
    /// Create or update the public listings file for this wallet's item account.
    Publish {
        #[command(flatten)]
        w: WalletArgs,
        /// Listings file to create or update.
        #[arg(long)]
        listings: PathBuf,
        /// Item note to list, as <txid>:<output index>.
        #[arg(long)]
        item_note: NoteRef,
        /// Display name.
        #[arg(long, default_value = "")]
        name: String,
        /// Price in zatoshis.
        #[arg(long)]
        price: u64,
        /// Maximum blocks between the chain tip and the transaction expiry.
        #[arg(long, default_value_t = zsettle_core::DEFAULT_MAX_EXPIRY_DELTA)]
        max_expiry_delta: u32,
    },
    /// Check one trade PCZT against the listings and sign the item action.
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
    },
    /// Watch an inbox directory, sign every acceptable trade, and write results to an outbox.
    ///
    /// Each pass syncs the wallet, reconciles the listings with the chain (sold items are
    /// removed, expired offers are listed again), then handles every envelope in the inbox.
    /// A failed pass is logged and retried with backoff; the service does not exit on its own.
    Serve {
        #[command(flatten)]
        w: WalletArgs,
        /// age identity file that decrypts the seller wallet's mnemonic.
        #[arg(long)]
        identity: PathBuf,
        /// Listings file (kept in sync with the chain: sold items removed, offers tracked).
        #[arg(long)]
        listings: PathBuf,
        /// Directory watched for `*.pczt` envelopes.
        #[arg(long)]
        inbox: PathBuf,
        /// Directory where `*.signed.pczt` files are written.
        #[arg(long)]
        outbox: PathBuf,
        /// Seconds between passes.
        #[arg(long, default_value_t = 5)]
        poll_secs: u64,
        /// lightwalletd server URL for the built-in sync.
        #[arg(long, default_value = zsettle_core::broadcast::DEFAULT_SERVER)]
        server: String,
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
        .init();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Sync { wallet, server } => {
            let wallet = Wallet::open(&wallet)?;
            let mut db = wallet.db()?;
            let report = sync_blocking(&wallet, &mut db, &server)?;
            println!("{}", serde_json::to_string(&report)?);
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
            let mut db = wallet.db()?;
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
                    "{}:{}  {:>8} zat  mined {}",
                    n.note.txid,
                    n.note.index,
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
            listings,
            item_note,
            name,
            price,
            max_expiry_delta,
        } => {
            let wallet = Wallet::open(&w.wallet)?;
            let db = wallet.db()?;
            let item_ufvk = account_ufvk(&db, parse_account(&w.item_account)?)?;
            let payout_fvk = payout_key(&db, &w)?;
            let payout_ua = UnifiedAddress::from_receivers(
                Some(payout_fvk.address_at(0u32, Scope::External)),
                None,
                None,
            )
            .ok_or_else(|| anyhow!("cannot build payout address"))?;
            // Make sure the note exists and is ours before listing it.
            let (target, _) = heights(&db, 1)?;
            let note = ironwood_note(&db, item_note, target)?;
            let fvk = item_ufvk
                .orchard()
                .ok_or_else(|| anyhow!("item account has no Orchard/Ironwood key"))?;
            if fvk.scope_for_address(&note.note.recipient()).is_none() {
                return Err(anyhow!(
                    "note {} is not owned by the item account",
                    item_note_str(&item_note)
                ));
            }
            let mut l = if listings.exists() {
                Listings::read(&listings)?
            } else {
                Listings {
                    version: 1,
                    network: network_name(wallet.network).into(),
                    item_ufvk: item_ufvk.encode(&wallet.network),
                    item_birthday: wallet.birthday,
                    payout_address: payout_ua.encode(&wallet.network),
                    items: vec![],
                }
            };
            l.upsert(ListedItem {
                note: item_note_str(&item_note),
                name,
                price_zat: price,
                max_expiry_delta,
                pending: None,
            });
            l.write(&listings)?;
            println!(
                "{}",
                serde_json::json!({ "listings": listings, "items": l.items.len() })
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
        } => {
            let wallet = Wallet::open(&w.wallet)?;
            let db = wallet.db()?;
            let l = Listings::read(&listings)?;
            let pczt = pczt_ops::from_bytes(
                &fs::read(&input).with_context(|| format!("reading {}", input.display()))?,
            )?;
            let outcome = process(&db, &w, &l, &pczt)?;
            println!("{}", describe(&outcome.result));
            let (item, approved) = match outcome.result {
                Ok(a) => (outcome.item.expect("matched item"), a),
                Err(e) => return Err(zsettle_core::policy::into_anyhow(e)),
            };
            if dry_run {
                return Ok(());
            }
            let signed = sign(
                &wallet,
                &identity,
                w.item_account_index,
                pczt,
                approved.item_action_index,
            )?;
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
            {
                let db = wallet.db()?;
                account_ufvk(&db, parse_account(&w.item_account)?)?;
                payout_key(&db, &w)?;
            }
            wallet
                .seed(&identity)
                .context("the identity file does not decrypt this wallet's mnemonic")?;
            let l = Listings::read(&listings)?;
            println!(
                "{}",
                serde_json::json!({
                    "event": "start",
                    "listed": l.items.len(),
                    "under_offer": l.items.iter().filter(|i| i.pending.is_some()).count(),
                    "inbox": inbox,
                    "outbox": outbox,
                    "sync": if sync_cmd.is_some() { "command" } else if no_sync { "off" } else { "built-in" },
                    "server": server,
                })
            );
            let service = Service {
                w: &w,
                identity: &identity,
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
    identity: &'a Path,
    listings: &'a Path,
    inbox: &'a Path,
    outbox: &'a Path,
    server: &'a str,
    no_sync: bool,
    sync_cmd: Option<&'a str>,
}

impl Service<'_> {
    /// One pass: sync, reconcile the listings with the chain, handle every envelope.
    fn pass(&self) -> Result<()> {
        if let Some(cmd) = self.sync_cmd {
            run_sync_command(cmd)?;
        }
        let wallet = Wallet::open(&self.w.wallet)?;
        let mut db = wallet.db()?;
        if self.sync_cmd.is_none() && !self.no_sync {
            sync_blocking(&wallet, &mut db, self.server).context("syncing the seller wallet")?;
        }
        let mut l = Listings::read(self.listings)?;
        if reconcile(&db, &mut l)? {
            l.write(self.listings)?;
        }
        for path in self.envelopes()? {
            self.handle(&wallet, &db, &mut l, &path)?;
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

    /// Handle one envelope: sign it or reject it, record the outcome, move the file.
    fn handle(&self, wallet: &Wallet, db: &Db, l: &mut Listings, path: &Path) -> Result<()> {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("trade")
            .to_string();
        let out = self.outbox.join(format!("{stem}.signed.pczt"));
        if out.exists() {
            // Signed in an earlier pass that stopped before moving the envelope.
            println!(
                "{}",
                serde_json::json!({ "event": "already_signed", "file": path, "out": out })
            );
            return move_into(path, &self.inbox.join("done"));
        }
        match self.sign_envelope(wallet, db, l, path) {
            Ok((item, approved, bytes)) => {
                write_atomic(&out, &bytes)?;
                l.set_pending(
                    &item.note,
                    Pending {
                        expiry_height: approved.expiry_height,
                        trade: stem.clone(),
                    },
                );
                l.write(self.listings)?;
                move_into(path, &self.inbox.join("done"))?;
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "signed", "file": path, "item": item.note, "name": item.name,
                        "signed_action": approved.item_action_index, "paid_zat": approved.paid_zat,
                        "expiry_height": approved.expiry_height, "out": out,
                    })
                );
            }
            Err(reason) => {
                println!(
                    "{}",
                    serde_json::json!({ "event": "rejected", "file": path, "reason": reason })
                );
                let rejected = self.inbox.join("rejected");
                move_into(path, &rejected)?;
                fs::write(rejected.join(format!("{stem}.reason.txt")), reason)?;
            }
        }
        Ok(())
    }

    /// Read, check and sign one envelope. The error is the reason recorded for the buyer.
    fn sign_envelope(
        &self,
        wallet: &Wallet,
        db: &Db,
        l: &Listings,
        path: &Path,
    ) -> std::result::Result<(ListedItem, Approved, Vec<u8>), String> {
        let bytes = fs::read(path).map_err(|e| format!("unreadable: {e}"))?;
        let pczt = pczt_ops::from_bytes(&bytes).map_err(|e| format!("unreadable: {e:#}"))?;
        let outcome = process(db, self.w, l, &pczt).map_err(|e| format!("error: {e:#}"))?;
        let approved = outcome.result.map_err(|e| format!("refused: {e}"))?;
        let item = outcome
            .item
            .ok_or_else(|| "error: approved trade without a listed item".to_string())?;
        let signed = sign(
            wallet,
            self.identity,
            self.w.item_account_index,
            pczt,
            approved.item_action_index,
        )
        .map_err(|e| format!("signing failed: {e:#}"))?;
        let bytes = pczt_ops::to_bytes(&signed).map_err(|e| format!("signing failed: {e:#}"))?;
        Ok((item, approved, bytes))
    }
}

/// Bring the listings in line with the wallet's view of the chain. Items whose note is no
/// longer spendable are removed (sold through the service, or spent some other way); offers
/// whose expiry height has been scanned with the note still unspent are listed again.
/// Returns whether anything changed.
fn reconcile(db: &Db, l: &mut Listings) -> Result<bool> {
    let (target, _) = heights(db, 1)?;
    let scanned = max_scanned_height(db)?;
    let mut changed = false;
    let mut kept = Vec::with_capacity(l.items.len());
    for mut item in std::mem::take(&mut l.items) {
        let Ok(r) = item.note_ref() else {
            println!(
                "{}",
                serde_json::json!({ "event": "dropped", "item": item.note, "reason": "bad note reference" })
            );
            changed = true;
            continue;
        };
        if find_ironwood_note(db, r, target)?.is_none() {
            let event = if item.pending.is_some() {
                "sold"
            } else {
                "spent_elsewhere"
            };
            println!(
                "{}",
                serde_json::json!({ "event": event, "item": item.note, "name": item.name })
            );
            changed = true;
            continue;
        }
        let expired = match (&item.pending, scanned) {
            (Some(p), Some(h)) => h >= p.expiry_height,
            _ => false,
        };
        if expired {
            if let Some(p) = item.pending.take() {
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "relisted", "item": item.note, "name": item.name,
                        "expired_trade": p.trade, "expiry_height": p.expiry_height,
                    })
                );
                changed = true;
            }
        }
        kept.push(item);
    }
    l.items = kept;
    Ok(changed)
}

struct Outcome {
    item: Option<ListedItem>,
    result: std::result::Result<Approved, PolicyError>,
}

/// Match a trade against the listings and run the policy checks.
fn process(db: &Db, w: &WalletArgs, l: &Listings, pczt: &pczt::Pczt) -> Result<Outcome> {
    let (target, _) = heights(db, 1)?;
    let tip: u32 =
        u32::from(zcash_protocol::consensus::BlockHeight::from(target)).saturating_sub(1);
    let item_fvk = account_ufvk(db, parse_account(&w.item_account)?)?
        .orchard()
        .cloned()
        .ok_or_else(|| anyhow!("item account has no Orchard/Ironwood key"))?;
    let seller_nfs = unspent_ironwood_nullifiers(db)?;
    let trade_nfs = pczt_ops::action_nullifiers(pczt)?;
    // Which listed item does this trade spend?
    let mut matched: Option<(ListedItem, Nullifier)> = None;
    for item in &l.items {
        let Ok(r) = item.note_ref() else { continue };
        let Ok(n) = ironwood_note(db, r, target) else {
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
        return Ok(Outcome {
            item: None,
            result: Err(PolicyError::ItemNotSpent),
        });
    };
    if let Some(p) = &item.pending {
        let until = p.expiry_height;
        return Ok(Outcome {
            item: Some(item),
            result: Err(PolicyError::UnderOffer { until }),
        });
    }
    let ivk = prepared_ivk(&payout_key(db, w)?);
    let policy = SellerPolicy {
        price_zat: item.price_zat,
        max_expiry_delta: item.max_expiry_delta,
        tip,
    };
    let result = check(pczt, &nf, &seller_nfs, &ivk, &policy);
    Ok(Outcome {
        item: Some(item),
        result,
    })
}

fn sign(
    wallet: &Wallet,
    identity: &Path,
    item_account_index: u32,
    pczt: pczt::Pczt,
    index: usize,
) -> Result<pczt::Pczt> {
    let seed = wallet.seed(identity)?;
    let usk = wallet.spending_key(&seed, item_account_index)?;
    let ask = SpendAuthorizingKey::from(usk.orchard());
    pczt_ops::sign_ironwood_action(pczt, index, &ask)
}

fn payout_key(db: &Db, w: &WalletArgs) -> Result<FullViewingKey> {
    account_ufvk(db, parse_account(&w.payout_account)?)?
        .orchard()
        .cloned()
        .ok_or_else(|| anyhow!("payout account has no Orchard/Ironwood key"))
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

fn item_note_str(r: &NoteRef) -> String {
    format!("{}:{}", r.txid, r.index)
}

fn network_name(n: zcash_protocol::consensus::Network) -> &'static str {
    match n {
        zcash_protocol::consensus::Network::MainNetwork => "main",
        zcash_protocol::consensus::Network::TestNetwork => "test",
    }
}
