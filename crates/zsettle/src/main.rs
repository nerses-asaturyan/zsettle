//! `zsettle`: buyer-side and anyone-side command line.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use orchard::keys::SpendAuthorizingKey;
use zsettle_core::pczt as pczt_ops;
use zsettle_core::trade::{build_trade, TradeRequest};
use zsettle_core::wallet::{parse_account, NoteRef, Wallet};

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
    /// Build, prove and redact a trade for a listing; writes the full PCZT and the seller's copy.
    Buy {
        /// Buyer wallet directory (devtool layout).
        #[arg(long)]
        wallet: PathBuf,
        /// UUID of the buyer's spending account.
        #[arg(long)]
        buyer_account: String,
        /// UUID of the imported, view-only account that holds the item note.
        #[arg(long)]
        item_account: String,
        /// The item note, as <txid>:<output index>.
        #[arg(long)]
        item_note: NoteRef,
        /// The buyer's note that pays, as <txid>:<output index>.
        #[arg(long)]
        buyer_note: NoteRef,
        /// Seller's unified address.
        #[arg(long)]
        seller: String,
        /// Price in zatoshis.
        #[arg(long)]
        price: u64,
        /// Blocks until the transaction expires.
        #[arg(long, default_value_t = zsettle_core::DEFAULT_MAX_EXPIRY_DELTA)]
        expiry_delta: u32,
        /// Confirmations required on the inputs.
        #[arg(long, default_value_t = 3)]
        min_confirmations: u32,
        /// Memo for the payment output.
        #[arg(long, default_value = "")]
        memo: String,
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
        /// Where to write the final transaction (raw bytes) and `<out>.pczt`.
        #[arg(long)]
        out: PathBuf,
        /// Broadcast through this lightwalletd server after extraction.
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
    /// Keyless provenance check of an item against chain data.
    Verify,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Buy {
            wallet,
            buyer_account,
            item_account,
            item_note,
            buyer_note,
            seller,
            price,
            expiry_delta,
            min_confirmations,
            memo,
            out,
            seller_copy,
        } => {
            let w = Wallet::open(&wallet)?;
            let mut db = w.db()?;
            let req = TradeRequest {
                buyer_account: parse_account(&buyer_account)?,
                item_account: parse_account(&item_account)?,
                item_note,
                buyer_note,
                seller_address: seller,
                price_zat: price,
                expiry_delta,
                min_confirmations,
                memo,
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
                    "item_action_index": built.item_action_index,
                    "payment_action_index": built.payment_action_index,
                    "fee_zat": built.fee,
                    "change_zat": built.change,
                    "target_height": built.target_height,
                    "expiry_height": built.expiry_height,
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
            let final_pczt_path = out.with_extension("pczt");
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
        Cmd::Verify => {
            println!("`verify` is not implemented yet");
            Ok(())
        }
    }
}
