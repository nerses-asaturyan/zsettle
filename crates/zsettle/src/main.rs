//! `zsettle`: buyer-side and anyone-side command line.

use anyhow::Result;
use clap::{Parser, Subcommand};

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
    /// Wallet operations (init, sync, import a viewing key).
    Wallet {
        #[command(subcommand)]
        cmd: WalletCmd,
    },
    /// Mint a new item (genesis note + anchor + manifest).
    Mint,
    /// Show or publish listings.
    List,
    /// Build, prove and redact a trade for a listing; writes the envelope for the seller.
    Buy,
    /// Sign the payment action, extract and broadcast the settled trade.
    Finish,
    /// Keyless provenance check of an item against chain data.
    Verify,
}

#[derive(Subcommand)]
enum WalletCmd {
    /// Create a wallet database.
    Init,
    /// Sync the Ironwood pool from a lightwalletd endpoint.
    Sync,
    /// Import a unified full viewing key (e.g. an item account received by consignment).
    ImportFvk,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Wallet { cmd } => match cmd {
            WalletCmd::Init => not_yet("wallet init"),
            WalletCmd::Sync => not_yet("wallet sync"),
            WalletCmd::ImportFvk => not_yet("wallet import-fvk"),
        },
        Cmd::Mint => not_yet("mint"),
        Cmd::List => not_yet("list"),
        Cmd::Buy => not_yet("buy"),
        Cmd::Finish => not_yet("finish"),
        Cmd::Verify => not_yet("verify"),
    }
}

fn not_yet(what: &str) -> Result<()> {
    println!(
        "`{what}` is not implemented yet (fee schedule check: {} zat for 2 actions)",
        zsettle_core::zip317_fee(2)
    );
    Ok(())
}
