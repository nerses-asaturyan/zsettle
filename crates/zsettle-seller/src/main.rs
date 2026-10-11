//! `zsettle-seller`: the seller's half. Watches an inbox for trade envelopes, runs the
//! policy checks, signs only the item action, and writes the signed envelope back.

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "zsettle-seller",
    version,
    about = "Seller service for zsettle trades"
)]
struct Cli {
    /// Directory watched for incoming `*.trade` envelopes.
    #[arg(long, default_value = "inbox")]
    inbox: PathBuf,
    /// Listing/policy file (price, max expiry delta, item id).
    #[arg(long, default_value = "listing.json")]
    policy: PathBuf,
    /// Validate and report, never sign.
    #[arg(long)]
    dry_run: bool,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    println!(
        "seller service not implemented yet: inbox={} policy={} dry_run={} max_expiry_delta={}",
        cli.inbox.display(),
        cli.policy.display(),
        cli.dry_run,
        zsettle_core::DEFAULT_MAX_EXPIRY_DELTA
    );
    Ok(())
}
