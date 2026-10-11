//! `zsettle-seller`: the seller's half. Checks a trade envelope against policy and signs
//! only the item action.

use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use orchard::keys::SpendAuthorizingKey;
use zsettle_core::pczt as pczt_ops;
use zsettle_core::policy::{check, describe, prepared_ivk, SellerPolicy};
use zsettle_core::wallet::{account_ufvk, heights, ironwood_note, parse_account, NoteRef, Wallet};

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

#[derive(Subcommand)]
enum Cmd {
    /// Check one trade PCZT against the listing and sign the item action.
    Sign {
        /// Seller wallet directory (devtool layout).
        #[arg(long)]
        wallet: PathBuf,
        /// age identity file that decrypts the seller wallet's mnemonic.
        #[arg(long)]
        identity: PathBuf,
        /// ZIP 32 account index of the item account (used to derive its signing key).
        #[arg(long, default_value_t = 1)]
        item_account_index: u32,
        /// UUID of the item account in this wallet.
        #[arg(long)]
        item_account: String,
        /// UUID of the account that receives the payment.
        #[arg(long)]
        payout_account: String,
        /// The listed item note, as <txid>:<output index>.
        #[arg(long)]
        item_note: NoteRef,
        /// Listed price in zatoshis.
        #[arg(long)]
        price: u64,
        /// Maximum blocks between the current tip and the transaction expiry.
        #[arg(long, default_value_t = zsettle_core::DEFAULT_MAX_EXPIRY_DELTA)]
        max_expiry_delta: u32,
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
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Sign {
            wallet,
            identity,
            item_account_index,
            item_account,
            payout_account,
            item_note,
            price,
            max_expiry_delta,
            input,
            out,
            dry_run,
        } => {
            let w = Wallet::open(&wallet)?;
            let db = w.db()?;
            let (target, _anchor) = heights(&db, 1)?;
            let tip: u32 =
                u32::from(zcash_protocol::consensus::BlockHeight::from(target)).saturating_sub(1);
            let item = ironwood_note(&db, item_note, target)?;
            let item_fvk = account_ufvk(&db, parse_account(&item_account)?)?
                .orchard()
                .cloned()
                .ok_or_else(|| anyhow!("item account has no Orchard/Ironwood key"))?;
            if item_fvk.scope_for_address(&item.note.recipient()).is_none() {
                return Err(anyhow!("the listed note is not owned by --item-account"));
            }
            let nf = item.note.nullifier(&item_fvk);
            let payout_fvk = account_ufvk(&db, parse_account(&payout_account)?)?
                .orchard()
                .cloned()
                .ok_or_else(|| anyhow!("payout account has no Orchard/Ironwood key"))?;
            let ivk = prepared_ivk(&payout_fvk);
            let pczt = pczt_ops::from_bytes(
                &fs::read(&input).with_context(|| format!("reading {}", input.display()))?,
            )?;
            let policy = SellerPolicy {
                price_zat: price,
                max_expiry_delta,
                tip,
            };
            let result = check(&pczt, &nf, &ivk, &policy);
            println!("{}", describe(&result));
            let approved = result.map_err(zsettle_core::policy::into_anyhow)?;
            if dry_run {
                return Ok(());
            }
            let seed = w.seed(&identity)?;
            let usk = w.spending_key(&seed, item_account_index)?;
            let ask = SpendAuthorizingKey::from(usk.orchard());
            let signed = pczt_ops::sign_ironwood_action(pczt, approved.item_action_index, &ask)?;
            fs::write(&out, pczt_ops::to_bytes(&signed)?)
                .with_context(|| format!("writing {}", out.display()))?;
            println!(
                "{}",
                serde_json::json!({
                    "signed_action": approved.item_action_index,
                    "paid_zat": approved.paid_zat,
                    "expiry_height": approved.expiry_height,
                    "out": out,
                })
            );
            Ok(())
        }
    }
}
