//! `zsettle-seller`: the seller's half. Publishes listings, checks incoming trade envelopes
//! against policy, and signs only the item action.

use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use orchard::keys::{FullViewingKey, Scope, SpendAuthorizingKey};
use orchard::note::Nullifier;
use zcash_keys::address::UnifiedAddress;
use zsettle_core::listing::{ListedItem, Listings};
use zsettle_core::pczt as pczt_ops;
use zsettle_core::policy::{check, describe, prepared_ivk, SellerPolicy};
use zsettle_core::wallet::{
    account_ufvk, heights, ironwood_note, parse_account, unspent_ironwood_nullifiers, Db, NoteRef,
    Wallet,
};

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
    Serve {
        #[command(flatten)]
        w: WalletArgs,
        /// age identity file that decrypts the seller wallet's mnemonic.
        #[arg(long)]
        identity: PathBuf,
        /// Listings file (sold items are removed from it).
        #[arg(long)]
        listings: PathBuf,
        /// Directory watched for `*.pczt` envelopes.
        #[arg(long)]
        inbox: PathBuf,
        /// Directory where `*.signed.pczt` files are written.
        #[arg(long)]
        outbox: PathBuf,
        /// Seconds between inbox scans.
        #[arg(long, default_value_t = 5)]
        poll_secs: u64,
        /// Shell command run before each scan (for example a wallet sync).
        #[arg(long)]
        sync_cmd: Option<String>,
        /// Process the inbox once and exit.
        #[arg(long)]
        once: bool,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    match cli.cmd {
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
            let (target, _) = heights(&db, 1)?;
            let notes =
                zsettle_core::mint::unspent_notes(&db, parse_account(&w.item_account)?, target)?;
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
            let outcome = process(&wallet, &db, &w, &l, &pczt)?;
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
            sync_cmd,
            once,
        } => {
            fs::create_dir_all(&inbox)?;
            fs::create_dir_all(inbox.join("done"))?;
            fs::create_dir_all(inbox.join("rejected"))?;
            fs::create_dir_all(&outbox)?;
            loop {
                if let Some(cmd) = &sync_cmd {
                    let status = std::process::Command::new("sh").arg("-c").arg(cmd).status();
                    if let Err(e) = status {
                        eprintln!("sync command failed to start: {e}");
                    }
                }
                let wallet = Wallet::open(&w.wallet)?;
                let db = wallet.db()?;
                let mut l = Listings::read(&listings)?;
                let mut entries: Vec<PathBuf> = fs::read_dir(&inbox)?
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.is_file() && p.extension().map(|x| x == "pczt").unwrap_or(false))
                    .collect();
                entries.sort();
                for path in entries {
                    let stem = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("trade")
                        .to_string();
                    let outcome = match fs::read(&path)
                        .map_err(anyhow::Error::from)
                        .and_then(|b| pczt_ops::from_bytes(&b))
                    {
                        Ok(pczt) => match process(&wallet, &db, &w, &l, &pczt) {
                            Ok(o) => match o.result {
                                Ok(approved) => {
                                    let item = o.item.expect("matched item");
                                    match sign(
                                        &wallet,
                                        &identity,
                                        w.item_account_index,
                                        pczt,
                                        approved.item_action_index,
                                    )
                                    .and_then(|signed| pczt_ops::to_bytes(&signed))
                                    {
                                        Ok(bytes) => {
                                            let out = outbox.join(format!("{stem}.signed.pczt"));
                                            fs::write(&out, bytes)?;
                                            l.remove(&item.note);
                                            l.write(&listings)?;
                                            Ok(serde_json::json!({
                                                "file": path, "item": item.note, "signed_action": approved.item_action_index,
                                                "paid_zat": approved.paid_zat, "expiry_height": approved.expiry_height, "out": out,
                                            }))
                                        }
                                        Err(e) => Err(format!("signing failed: {e}")),
                                    }
                                }
                                Err(e) => Err(format!("refused: {e}")),
                            },
                            Err(e) => Err(format!("error: {e}")),
                        },
                        Err(e) => Err(format!("unreadable: {e}")),
                    };
                    match outcome {
                        Ok(v) => {
                            println!("{v}");
                            fs::rename(&path, inbox.join("done").join(path.file_name().unwrap()))?;
                        }
                        Err(reason) => {
                            println!(
                                "{}",
                                serde_json::json!({ "file": path, "rejected": reason })
                            );
                            fs::rename(
                                &path,
                                inbox.join("rejected").join(path.file_name().unwrap()),
                            )?;
                            fs::write(
                                inbox.join("rejected").join(format!("{stem}.reason.txt")),
                                reason,
                            )?;
                        }
                    }
                }
                if once {
                    return Ok(());
                }
                thread::sleep(Duration::from_secs(poll_secs));
            }
        }
    }
}

struct Outcome {
    item: Option<ListedItem>,
    result: Result<zsettle_core::policy::Approved, zsettle_core::policy::PolicyError>,
}

/// Match a trade against the listings and run the policy checks.
fn process(
    wallet: &Wallet,
    db: &Db,
    w: &WalletArgs,
    l: &Listings,
    pczt: &pczt::Pczt,
) -> Result<Outcome> {
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
            result: Err(zsettle_core::policy::PolicyError::ItemNotSpent),
        });
    };
    let ivk = prepared_ivk(&payout_key(db, w)?);
    let policy = SellerPolicy {
        price_zat: item.price_zat,
        max_expiry_delta: item.max_expiry_delta,
        tip,
    };
    let _ = wallet;
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

fn item_note_str(r: &NoteRef) -> String {
    format!("{}:{}", r.txid, r.index)
}

fn network_name(n: zcash_protocol::consensus::Network) -> &'static str {
    match n {
        zcash_protocol::consensus::Network::MainNetwork => "main",
        zcash_protocol::consensus::Network::TestNetwork => "test",
    }
}
