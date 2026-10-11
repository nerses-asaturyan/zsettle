//! `zsettle`: buyer-side and anyone-side command line.

use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use orchard::keys::SpendAuthorizingKey;
use zcash_protocol::consensus::Network;
use zsettle_core::broadcast::{check_consensus_blocking, pick_server, server_info_blocking};
use zsettle_core::listing::Listings;
use zsettle_core::pczt as pczt_ops;
use zsettle_core::trade::{build_trade, TradeRequest};
use zsettle_core::verify::Consignment;
use zsettle_core::wallet::{
    account_ufvk, find_account_by_ufvk, heights, import_viewing_key, largest_spendable_note,
    parse_account, reserve_note, reserved_notes, sync_blocking, NoteRef, Wallet,
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
    /// Show the items in a listings file and whether its signature checks out.
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
    /// Pay a unified address from an account of this wallet (for example to fund a second wallet).
    Pay {
        /// Wallet directory.
        #[arg(long)]
        wallet: PathBuf,
        /// UUID of the paying account.
        #[arg(long)]
        account: String,
        /// ZIP 32 account index of the paying account.
        #[arg(long, default_value_t = 0)]
        account_index: u32,
        /// age identity file that decrypts the wallet's mnemonic.
        #[arg(long)]
        identity: PathBuf,
        /// Recipient unified address.
        #[arg(long)]
        to: String,
        /// Amount in zatoshis.
        #[arg(long)]
        amount: u64,
        /// Optional memo.
        #[arg(long)]
        memo: Option<String>,
        /// lightwalletd server URL (default: the zec.rocks endpoint of the wallet's network).
        #[arg(long)]
        server: Option<String>,
    },
    /// Import the item viewing key from a listings file into the buyer wallet (the consignment).
    ImportListing {
        /// Buyer wallet directory.
        #[arg(long)]
        wallet: PathBuf,
        /// Listings file published by a seller (must be signed).
        #[arg(long)]
        listings: PathBuf,
        /// Name for the imported view-only account.
        #[arg(long, default_value = "item-view")]
        name: String,
        /// lightwalletd server URL (default: the zec.rocks endpoint of the wallet's network).
        #[arg(long)]
        server: Option<String>,
    },
    /// Build, prove and redact a trade; writes the full PCZT and the seller's copy.
    Buy {
        /// Buyer wallet directory (devtool layout).
        #[arg(long)]
        wallet: PathBuf,
        /// UUID of the buyer's spending account.
        #[arg(long)]
        buyer_account: String,
        /// Listings file (must be signed); fills in the seller address, price and expiry for `--item`.
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
        /// The buyer's note that pays, as <txid>:<output index> (default: the largest spendable
        /// note that is not reserved by an unsettled trade).
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
        /// lightwalletd server URL (default: the zec.rocks endpoint of the wallet's network).
        #[arg(long)]
        server: Option<String>,
        /// Skip the consensus-branch check against the server.
        #[arg(long)]
        offline: bool,
        /// Where to write the full PCZT. It holds your viewing key and witnesses: keep it private.
        #[arg(long)]
        out: PathBuf,
        /// Where to write the redacted copy for the seller (drop it in the seller's inbox).
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
        /// lightwalletd server URL (default: the zec.rocks endpoint of the wallet's network).
        #[arg(long)]
        server: Option<String>,
    },
    /// Broadcast a raw transaction file through a lightwalletd server.
    Broadcast {
        /// Raw transaction bytes, as written by `finish`.
        #[arg(long)]
        tx: PathBuf,
        /// Network the transaction is for (`main` or `test`).
        #[arg(long, default_value = "main")]
        network: String,
        /// lightwalletd server URL (default: the zec.rocks endpoint of the wallet's network).
        #[arg(long)]
        server: Option<String>,
    },
    /// Ask the light server where a transaction is: mined (at which height), in the mempool, or unknown.
    Tx {
        /// Transaction id (display form).
        #[arg(long)]
        txid: String,
        /// Network the transaction is for (`main` or `test`).
        #[arg(long, default_value = "main")]
        network: String,
        /// lightwalletd server URL (default: the zec.rocks endpoint of the network).
        #[arg(long)]
        server: Option<String>,
    },
    /// Keyless provenance check of an item: trace it from its genesis note with the listings' viewing keys.
    ///
    /// Pass `--listings` once per holder the item has passed through; each file contributes
    /// one consignment and the trace follows the item across every resale it can decrypt.
    Verify {
        /// Listings file(s) providing item viewing keys and birthdays (must be signed).
        #[arg(long, required = true)]
        listings: Vec<PathBuf>,
        /// Genesis note of the item, as <txid>:<output index>.
        #[arg(long)]
        item: NoteRef,
        /// lightwalletd server URL (default: the zec.rocks endpoint of the wallet's network).
        #[arg(long)]
        server: Option<String>,
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

/// Refuse to send when the server is on a consensus branch this build does not sign for.
fn check_server_before_sending(server: &str, network: Network) -> Result<()> {
    let info = server_info_blocking(server)?;
    check_consensus_blocking(server, network, info.block_height.saturating_add(1))?;
    Ok(())
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
        Cmd::List { listings } => {
            let l = Listings::read(&listings)?;
            let signed = match l.verify_signature() {
                Ok(()) => "signed by the item account's key".to_string(),
                Err(e) => format!("NOT TRUSTWORTHY: {e}"),
            };
            println!(
                "network: {}  payout: {}…{}  {signed}",
                l.network,
                &l.payout_address[..12],
                &l.payout_address[l.payout_address.len() - 6..]
            );
            for i in &l.items {
                println!(
                    "{}  {:>10} zat  expiry<= {:>3} blocks  {}",
                    i.note, i.price_zat, i.max_expiry_delta, i.name
                );
            }
            Ok(())
        }
        Cmd::Sync { wallet, server } => {
            let w = Wallet::open(&wallet)?;
            let server = pick_server(server, w.network);
            let mut db = w.db()?;
            let report = sync_blocking(&w, &mut db, &server)?;
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
        Cmd::Pay {
            wallet,
            account,
            account_index,
            identity,
            to,
            amount,
            memo,
            server,
        } => {
            let w = Wallet::open(&wallet)?;
            let server = pick_server(server, w.network);
            let mut db = w.db()?;
            check_server_before_sending(&server, w.network)?;
            let seed = w.seed(&identity)?;
            let req = zsettle_core::mint::PayRequest {
                from_account: parse_account(&account)?,
                from_account_index: account_index,
                to,
                value_zat: amount,
                memo,
            };
            let (bytes, txid) = zsettle_core::mint::pay(&w, &mut db, &seed, &req)?;
            let response = zsettle_core::broadcast::send_raw_transaction_blocking(&server, bytes)?;
            println!(
                "{}",
                serde_json::json!({ "txid": txid, "amount_zat": amount, "broadcast": response })
            );
            Ok(())
        }
        Cmd::ImportListing {
            wallet,
            listings,
            name,
            server,
        } => {
            let w = Wallet::open(&wallet)?;
            let server = pick_server(server, w.network);
            let l = Listings::read_verified(&listings)?;
            if l.network()? != w.network {
                return Err(anyhow!("the listings file is for another network"));
            }
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
            server,
            offline,
            out,
            seller_copy,
        } => {
            let w = Wallet::open(&wallet)?;
            let server = pick_server(server, w.network);
            let mut db = w.db()?;
            let l = listings
                .as_ref()
                .map(|p| Listings::read_verified(p))
                .transpose()?;
            if let Some(l) = &l {
                if l.network()? != w.network {
                    return Err(anyhow!("the listings file is for another network"));
                }
            }
            let listed = l.as_ref().and_then(|l| l.find(&item.to_string()).cloned());
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
            let (target, _) = heights(&db, min_confirmations)?;
            let target_height = u32::from(zcash_protocol::consensus::BlockHeight::from(target));
            if !offline {
                check_consensus_blocking(&server, w.network, target_height)?;
            }
            let reserved = reserved_notes(&w, target_height)?;
            let buyer_note = match buyer_note {
                Some(n) => n,
                None => largest_spendable_note(&db, buyer_account, target, &reserved)?
                    .ok_or_else(|| {
                        anyhow!("no spendable note in the buyer account that is not reserved by an unsettled trade")
                    })?,
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
            let envelope = pczt_ops::to_bytes(&redacted)?;
            fs::write(&seller_copy, &envelope)
                .with_context(|| format!("writing {}", seller_copy.display()))?;
            reserve_note(&w, buyer_note, built.expiry_height)?;
            let stem = seller_copy
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("trade");
            let expect_signed = format!("{stem}.{}.signed.pczt", pczt_ops::envelope_tag(&envelope));
            println!(
                "{}",
                serde_json::json!({
                    "item": item.to_string(),
                    "buyer_note": buyer_note.to_string(),
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
                    "expect_signed": expect_signed,
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
            let server = pick_server(server, w.network);
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
                check_server_before_sending(&server, w.network)?;
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
        Cmd::Broadcast {
            tx,
            network,
            server,
        } => {
            let bytes = fs::read(&tx).with_context(|| format!("reading {}", tx.display()))?;
            let network = parse_network(&network)?;
            let server = pick_server(server, network);
            check_server_before_sending(&server, network)?;
            let msg = zsettle_core::broadcast::send_raw_transaction_blocking(&server, bytes)?;
            println!(
                "{}",
                serde_json::json!({ "server": server, "response": msg })
            );
            Ok(())
        }
        Cmd::Tx {
            txid,
            network,
            server,
        } => {
            let network = parse_network(&network)?;
            let server = pick_server(server, network);
            let id = zsettle_core::wallet::parse_txid(&txid)?;
            let status = zsettle_core::broadcast::transaction_status_blocking(&server, &id)?;
            println!(
                "{}",
                serde_json::json!({ "txid": id.to_string(), "status": status, "server": server })
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
                let l = Listings::read_verified(path)?;
                let net = l.network()?;
                match network {
                    None => network = Some(net),
                    Some(n) if n != net => {
                        return Err(anyhow!("{} is for another network", path.display()))
                    }
                    Some(_) => {}
                }
                consignments.push(Consignment {
                    label: path.display().to_string(),
                    fvk: l.item_fvk()?,
                    birthday: l.item_birthday,
                });
            }
            let network =
                network.ok_or_else(|| anyhow!("at least one --listings file is required"))?;
            let server = pick_server(server, network);
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
