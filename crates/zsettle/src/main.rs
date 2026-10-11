//! `zsettle`: buyer-side and anyone-side command line.

use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use orchard::keys::SpendAuthorizingKey;
use zcash_protocol::consensus::Network;
use zsettle_core::listing::Listings;
use zsettle_core::pczt as pczt_ops;
use zsettle_core::trade::{build_trade, TradeRequest};
use zsettle_core::verify::Consignment;
use zsettle_core::wallet::{
    account_ufvk, find_account_by_ufvk, heights, import_viewing_key, largest_spendable_note,
    parse_account, sync_blocking, NoteRef, Wallet,
};

#[derive(Parser)]
#[command(
    name = "zsettle",
    version,
    about = "Atomic item-for-ZEC settlement on Zcash"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show the items in a listings file.
    List {
        /// Listings file published by a seller.
        #[arg(long)]
        listings: PathBuf,
    },
    /// Sync a wallet with a lightwalletd server (no other tool needed).
    Sync {
        /// Wallet directory.
        #[arg(long)]
        wallet: PathBuf,
        /// lightwalletd server URL.
        #[arg(long, default_value = zsettle_core::broadcast::DEFAULT_SERVER)]
        server: String,
    },
    /// Import the item viewing key from a listings file into the buyer wallet (the consignment).
    ImportListing {
        /// Buyer wallet directory.
        #[arg(long)]
        wallet: PathBuf,
        /// Listings file published by a seller.
        #[arg(long)]
        listings: PathBuf,
        /// Name for the imported view-only account.
        #[arg(long, default_value = "item-view")]
        name: String,
        /// lightwalletd server URL (for the account birthday).
        #[arg(long, default_value = zsettle_core::broadcast::DEFAULT_SERVER)]
        server: String,
    },
    /// Build, prove and redact a trade; writes the full PCZT and the seller's copy.
    Buy {
        /// Buyer wallet directory (devtool layout).
        #[arg(long)]
        wallet: PathBuf,
        /// UUID of the buyer's spending account.
        #[arg(long)]
        buyer_account: String,
        /// Listings file; fills in the seller address, price and expiry for `--item`.
        #[arg(long)]
        listings: Option<PathBuf>,
        /// The item note, as <txid>:<output index>.
        #[arg(long)]
        item: NoteRef,
        /// UUID of the imported, view-only account that holds the item note (auto-detected from the listings).
        #[arg(long)]
        item_account: Option<String>,
        /// UUID of the account of this wallet that receives the item (default: the buyer account).
        /// Use a dedicated item account if you may resell the item later.
        #[arg(long)]
        receive_account: Option<String>,
        /// The buyer's note that pays, as <txid>:<output index> (default: the largest spendable note).
        #[arg(long)]
        buyer_note: Option<NoteRef>,
        /// Seller's unified address (taken from the listings when omitted).
        #[arg(long)]
        seller: Option<String>,
        /// Price in zatoshis (taken from the listings when omitted).
        #[arg(long)]
        price: Option<u64>,
        /// Blocks until the transaction expires (taken from the listings when omitted).
        #[arg(long)]
        expiry_delta: Option<u32>,
        /// Confirmations required on the inputs.
        #[arg(long, default_value_t = 3)]
        min_confirmations: u32,
        /// Memo for the payment output.
        #[arg(long, default_value = "")]
        memo: String,
        /// Build the trade even if the listing marks the item as under offer.
        #[arg(long)]
        force: bool,
        /// Where to write the full PCZT (keep private).
        #[arg(long)]
        out: PathBuf,
        /// Where to write the redacted copy for the seller.
        #[arg(long)]
        seller_copy: PathBuf,
    },
    /// Combine the seller's signed copy with the full PCZT, sign the payment action, extract the transaction.
    Finish {
        /// Buyer wallet directory.
        #[arg(long)]
        wallet: PathBuf,
        /// age identity file that decrypts the buyer wallet's mnemonic.
        #[arg(long)]
        identity: PathBuf,
        /// ZIP 32 account index of the buyer's spending account.
        #[arg(long, default_value_t = 0)]
        account_index: u32,
        /// The full PCZT written by `buy`.
        #[arg(long)]
        full: PathBuf,
        /// The seller's signed copy.
        #[arg(long)]
        signed: PathBuf,
        /// Where to write the final transaction (raw bytes); the final PCZT goes to `<out>.final.pczt`.
        #[arg(long)]
        out: PathBuf,
        /// Broadcast through the lightwalletd server after extraction.
        #[arg(long)]
        broadcast: bool,
        /// lightwalletd server URL.
        #[arg(long, default_value = zsettle_core::broadcast::DEFAULT_SERVER)]
        server: String,
    },
    /// Broadcast a raw transaction file through a lightwalletd server.
    Broadcast {
        /// Raw transaction bytes, as written by `finish`.
        #[arg(long)]
        tx: PathBuf,
        /// lightwalletd server URL.
        #[arg(long, default_value = zsettle_core::broadcast::DEFAULT_SERVER)]
        server: String,
    },
    /// Keyless provenance check of an item: trace it from its genesis note with the listings' viewing keys.
    ///
    /// Pass `--listings` once per holder the item has passed through; each file contributes
    /// one consignment and the trace follows the item across every resale it can decrypt.
    Verify {
        /// Listings file(s) providing item viewing keys and birthdays.
        #[arg(long, required = true)]
        listings: Vec<PathBuf>,
        /// Genesis note of the item, as <txid>:<output index>.
        #[arg(long)]
        item: NoteRef,
        /// lightwalletd server URL.
        #[arg(long, default_value = zsettle_core::broadcast::DEFAULT_SERVER)]
        server: String,
        /// Stop scanning at this height (default: chain tip).
        #[arg(long)]
        to_height: Option<u32>,
    },
}

fn parse_network(name: &str) -> Result<Network> {
    match name {
        "main" => Ok(Network::MainNetwork),
        "test" => Ok(Network::TestNetwork),
        other => Err(anyhow!("unknown network {other}")),
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::List { listings } => {
            let l = Listings::read(&listings)?;
            println!(
                "network: {}  payout: {}…{}",
                l.network,
                &l.payout_address[..12],
                &l.payout_address[l.payout_address.len() - 6..]
            );
            for i in &l.items {
                let state = match &i.pending {
                    Some(p) => format!("  under offer until block {}", p.expiry_height),
                    None => String::new(),
                };
                println!(
                    "{}  {:>10} zat  expiry<= {:>3} blocks  {}{state}",
                    i.note, i.price_zat, i.max_expiry_delta, i.name
                );
            }
            Ok(())
        }
        Cmd::Sync { wallet, server } => {
            let w = Wallet::open(&wallet)?;
            let mut db = w.db()?;
            let report = sync_blocking(&w, &mut db, &server)?;
            println!("{}", serde_json::to_string(&report)?);
            Ok(())
        }
        Cmd::ImportListing {
            wallet,
            listings,
            name,
            server,
        } => {
            let w = Wallet::open(&wallet)?;
            let l = Listings::read(&listings)?;
            let mut db = w.db()?;
            if let Some(existing) = find_account_by_ufvk(&db, w.network, &l.item_ufvk)? {
                println!(
                    "{}",
                    serde_json::json!({ "account": existing.expose_uuid().to_string(), "status": "already imported" })
                );
                return Ok(());
            }
            let id = import_viewing_key(
                &mut db,
                w.network,
                &name,
                &l.item_ufvk,
                l.item_birthday,
                &server,
            )?;
            println!(
                "{}",
                serde_json::json!({ "account": id.expose_uuid().to_string(), "birthday": l.item_birthday, "next": "sync the wallet" })
            );
            Ok(())
        }
        Cmd::Buy {
            wallet,
            buyer_account,
            listings,
            item,
            item_account,
            receive_account,
            buyer_note,
            seller,
            price,
            expiry_delta,
            min_confirmations,
            memo,
            force,
            out,
            seller_copy,
        } => {
            let w = Wallet::open(&wallet)?;
            let mut db = w.db()?;
            let l = listings.as_ref().map(|p| Listings::read(p)).transpose()?;
            let listed = l
                .as_ref()
                .and_then(|l| l.find(&format!("{}:{}", item.txid, item.index)).cloned());
            if let Some(p) = listed.as_ref().and_then(|i| i.pending.as_ref()) {
                if !force {
                    return Err(anyhow!(
                        "the listing marks this item as under offer until block {}; pass --force to build a trade anyway",
                        p.expiry_height
                    ));
                }
            }
            let seller_address = seller
                .or_else(|| l.as_ref().map(|l| l.payout_address.clone()))
                .ok_or_else(|| anyhow!("--seller or --listings is required"))?;
            let price_zat = price
                .or_else(|| listed.as_ref().map(|i| i.price_zat))
                .ok_or_else(|| anyhow!("--price is required (item not found in listings)"))?;
            let expiry = expiry_delta
                .or_else(|| listed.as_ref().map(|i| i.max_expiry_delta))
                .unwrap_or(zsettle_core::DEFAULT_MAX_EXPIRY_DELTA);
            let item_memo = match listed.as_ref().map(|i| i.name.as_str()) {
                Some(name) if !name.is_empty() => format!("zsettle item: {name}"),
                _ => String::new(),
            };
            let buyer_account = parse_account(&buyer_account)?;
            let item_account = match (item_account, &l) {
                (Some(s), _) => parse_account(&s)?,
                (None, Some(l)) => {
                    find_account_by_ufvk(&db, w.network, &l.item_ufvk)?.ok_or_else(|| {
                        anyhow!("item viewing key not imported yet; run import-listing")
                    })?
                }
                (None, None) => return Err(anyhow!("--item-account or --listings is required")),
            };
            let receive_account = receive_account.as_deref().map(parse_account).transpose()?;
            let buyer_note = match buyer_note {
                Some(n) => n,
                None => {
                    let (target, _) = heights(&db, min_confirmations)?;
                    largest_spendable_note(&db, buyer_account, target)?
                        .ok_or_else(|| anyhow!("no spendable note in the buyer account"))?
                }
            };
            let _ = account_ufvk(&db, buyer_account)?;
            let req = TradeRequest {
                buyer_account,
                item_account,
                item_note: item,
                buyer_note,
                seller_address,
                price_zat,
                expiry_delta: expiry,
                min_confirmations,
                memo,
                item_memo,
                receive_account,
            };
            let built = build_trade(&mut db, w.network, &req)?;
            fs::write(&out, pczt_ops::to_bytes(&built.pczt)?)
                .with_context(|| format!("writing {}", out.display()))?;
            let redacted =
                pczt_ops::redact_for_seller(built.pczt.clone(), built.item_action_index)?;
            fs::write(&seller_copy, pczt_ops::to_bytes(&redacted)?)
                .with_context(|| format!("writing {}", seller_copy.display()))?;
            println!(
                "{}",
                serde_json::json!({
                    "item": format!("{}:{}", item.txid, item.index),
                    "buyer_note": format!("{}:{}", buyer_note.txid, buyer_note.index),
                    "price_zat": price_zat,
                    "item_action_index": built.item_action_index,
                    "payment_action_index": built.payment_action_index,
                    "fee_zat": built.fee,
                    "change_zat": built.change,
                    "target_height": built.target_height,
                    "expiry_height": built.expiry_height,
                    "receive_account": receive_account.map(|a| a.expose_uuid().to_string()),
                    "full_pczt": out,
                    "seller_copy": seller_copy,
                })
            );
            Ok(())
        }
        Cmd::Finish {
            wallet,
            identity,
            account_index,
            full,
            signed,
            out,
            broadcast,
            server,
        } => {
            let w = Wallet::open(&wallet)?;
            let full_pczt = pczt_ops::from_bytes(&fs::read(&full)?)?;
            let signed_pczt = pczt_ops::from_bytes(&fs::read(&signed)?)?;
            let combined = pczt_ops::combine(full_pczt, signed_pczt)?;
            let seed = w.seed(&identity)?;
            let usk = w.spending_key(&seed, account_index)?;
            let ask = SpendAuthorizingKey::from(usk.orchard());
            let (pczt, signed_n) = pczt_ops::sign_matching_actions(combined, &ask)?;
            let final_pczt_path = out.with_extension("final.pczt");
            fs::write(&final_pczt_path, pczt_ops::to_bytes(&pczt)?)?;
            let tx = pczt_ops::extract(pczt)?;
            let bytes = pczt_ops::tx_bytes(&tx)?;
            fs::write(&out, &bytes)?;
            let mut sent = serde_json::Value::Null;
            if broadcast {
                let msg =
                    zsettle_core::broadcast::send_raw_transaction_blocking(&server, bytes.clone())?;
                sent = serde_json::json!({ "server": server, "response": msg });
            }
            println!(
                "{}",
                serde_json::json!({
                    "txid": tx.txid().to_string(),
                    "buyer_signatures": signed_n,
                    "size_bytes": bytes.len(),
                    "raw_tx": out,
                    "final_pczt": final_pczt_path,
                    "broadcast": sent,
                })
            );
            Ok(())
        }
        Cmd::Broadcast { tx, server } => {
            let bytes = fs::read(&tx).with_context(|| format!("reading {}", tx.display()))?;
            let msg = zsettle_core::broadcast::send_raw_transaction_blocking(&server, bytes)?;
            println!(
                "{}",
                serde_json::json!({ "server": server, "response": msg })
            );
            Ok(())
        }
        Cmd::Verify {
            listings,
            item,
            server,
            to_height,
        } => {
            let mut network: Option<Network> = None;
            let mut consignments = Vec::with_capacity(listings.len());
            for path in &listings {
                let l = Listings::read(path)?;
                let net = parse_network(&l.network)?;
                match network {
                    None => network = Some(net),
                    Some(n) if n != net => {
                        return Err(anyhow!("{} is for another network", path.display()))
                    }
                    Some(_) => {}
                }
                let ufvk = zcash_keys::keys::UnifiedFullViewingKey::decode(&net, &l.item_ufvk)
                    .map_err(|e| anyhow!("decoding item viewing key in {}: {e}", path.display()))?;
                let fvk = ufvk.orchard().cloned().ok_or_else(|| {
                    anyhow!("{} has no Orchard/Ironwood viewing key", path.display())
                })?;
                let label = path.display().to_string();
                consignments.push(Consignment {
                    label,
                    fvk,
                    birthday: l.item_birthday,
                });
            }
            let network =
                network.ok_or_else(|| anyhow!("at least one --listings file is required"))?;
            let report = zsettle_core::verify::trace_blocking(
                &server,
                network,
                &consignments,
                item,
                to_height,
            )?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
    }
}
