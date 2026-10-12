//! `zsettle-seller`: the seller's half. Publishes signed listings, checks incoming trade
//! envelopes against policy, verifies their proofs, and signs only the item action.

use std::{
    cell::RefCell,
    collections::HashSet,
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
use zsettle_core::broadcast::{check_consensus_at_tip_blocking, pick_server};
use zsettle_core::listing::{ListedItem, Listings};
use zsettle_core::mint::{note_state, NoteState};
use zsettle_core::pczt as pczt_ops;
use zsettle_core::policy::{
    check, check_public, describe, prepared_ivk, Approved, PolicyError, SellerPolicy,
};
use zsettle_core::wallet::{
    account_birthday, account_orchard_fvk, account_unspent_nullifiers, create_account, heights,
    init_wallet, ironwood_note, largest_spendable_note, list_accounts, max_scanned_height,
    next_orchard_address, parse_account, recent_ironwood_anchors, sync_blocking, Db, NoteRef,
    Wallet,
};
use zsettle_core::DEFAULT_MAX_EXPIRY_DELTA;
use zsettle_core::ZIP317_MARGINAL_FEE;
use zsettle_core::{MAX_ENVELOPE_BYTES, MIN_PRICE_ZAT};

/// How many recent blocks' tree roots count as acceptable anchors (the wallet keeps that
/// many checkpoints).
const ANCHOR_DEPTH: u32 = 100;
/// How many blocks a buyer may be ahead of this wallet before a too-long expiry is final.
const LAG_SLACK: u32 = 5;
/// Longest pause between passes after repeated failures.
const MAX_BACKOFF_SECS: u64 = 300;
/// Inbox files modified more recently than this are left for the next pass: they may still be
/// in the middle of being written.
const SETTLE_SECS: u64 = 2;
/// Offers waiting in the inbox before the HTTP endpoint answers 503: bounds the disk a
/// flood can fill to this many envelopes of at most 1 MiB.
const MAX_PENDING_OFFERS: usize = 1_000;
/// Reason files kept under `rejected/`; beyond this, refusals are only logged.
const MAX_REASON_FILES: usize = 10_000;
/// Actions a trade can have: the item action, the payment, perhaps the buyer's change.
const MAX_TRADE_ACTIONS: usize = 3;
/// Connections the HTTP endpoint serves at once.
const MAX_HTTP_CONNECTIONS: usize = 32;
/// Envelopes one pass may leave waiting for a later pass; beyond this they are refused, so a
/// flood of unproven envelopes cannot fill the inbox up to its cap.
const MAX_DEFERRED: usize = 100;
/// Blocks a deferred envelope may wait for the chain view to catch up with it. A buyer can be
/// ahead of this wallet's view by at most `LAG_SLACK` blocks (beyond that the expiry check
/// refuses for good), and a listed note becomes spendable a few blocks after its mint, so every
/// honest reason to defer resolves well within this; a longer wait is given up.
const DEFER_MAX_BLOCKS: u32 = 10;
/// Content tags whose proof was verified, kept in memory; cleared when this many.
const MAX_VERIFIED_TAGS: usize = 10_000;
/// Filed envelopes and signed replies carry buyers' note nullifiers: deleted after this long.
const MAX_DONE_AGE_SECS: u64 = 24 * 60 * 60;
/// Request head the HTTP endpoint reads at most.
const MAX_HTTP_HEAD: usize = 8 * 1024;
/// Read and write timeout of one HTTP connection.
const HTTP_TIMEOUT_SECS: u64 = 10;

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
    /// Seller wallet directory.
    #[arg(long, env = "ZSETTLE_WALLET")]
    wallet: PathBuf,
    /// UUID of the item account in this wallet.
    #[arg(long, env = "ZSETTLE_ITEM_ACCOUNT")]
    item_account: String,
    /// ZIP 32 account index of the item account (used to derive its signing key).
    #[arg(long, env = "ZSETTLE_ITEM_ACCOUNT_INDEX", default_value_t = 1)]
    item_account_index: u32,
    /// UUID of the account that receives payments.
    #[arg(long, env = "ZSETTLE_PAYOUT_ACCOUNT")]
    payout_account: String,
}

#[derive(Subcommand)]
// Command structs are built once and matched once; their sizes do not matter.
#[allow(clippy::large_enum_variant)]
enum Cmd {
    /// Create a seller wallet: a new mnemonic encrypted to an age identity, the database, and
    /// the accounts `payout` (index 0, pays for mints and receives payments) and `item`
    /// (index 1, holds the items). Prints the account UUIDs and an address to fund.
    Init {
        /// Directory to create.
        #[arg(long)]
        wallet: PathBuf,
        /// age identity file to encrypt the mnemonic to, generated when it does not exist;
        /// keep it, it is the only way to decrypt the mnemonic. Omit it to protect the
        /// mnemonic with a passphrase instead (asked for twice, or taken from
        /// ZSETTLE_PASSPHRASE).
        #[arg(long, env = "ZSETTLE_IDENTITY")]
        identity: Option<PathBuf>,
        /// `main` or `test`.
        #[arg(long, default_value = "main")]
        network: String,
        /// lightwalletd server URL (defaults to the network's).
        #[arg(long)]
        server: Option<String>,
    },
    /// List the wallet's accounts: name, UUID, ZIP 32 index, birthday.
    Accounts {
        /// Wallet directory.
        #[arg(long, env = "ZSETTLE_WALLET")]
        wallet: PathBuf,
    },
    /// Print a fresh receiving address of an account (Orchard only). Each call gives a new
    /// one; reuse links payments.
    Address {
        /// Wallet directory.
        #[arg(long, env = "ZSETTLE_WALLET")]
        wallet: PathBuf,
        /// Account UUID.
        #[arg(long)]
        account: String,
    },
    /// Send ZEC from the payout account to any address: cash out, or fund a counterparty.
    Pay {
        /// Wallet directory.
        #[arg(long, env = "ZSETTLE_WALLET")]
        wallet: PathBuf,
        /// UUID of the paying account.
        #[arg(long, env = "ZSETTLE_PAYOUT_ACCOUNT")]
        account: String,
        /// ZIP 32 account index of the paying account.
        #[arg(long, default_value_t = 0)]
        account_index: u32,
        /// age identity file that decrypts the wallet's mnemonic. Omit it for a
        /// passphrase-protected wallet: the passphrase is asked for, or taken from
        /// ZSETTLE_PASSPHRASE.
        #[arg(long, env = "ZSETTLE_IDENTITY")]
        identity: Option<PathBuf>,
        /// Recipient unified address.
        #[arg(long)]
        to: String,
        /// Amount, in zatoshis or ZEC (`0.001`, `0.001 ZEC`).
        #[arg(long, value_parser = zsettle_core::parse_zat)]
        amount: u64,
        /// Optional memo.
        #[arg(long)]
        memo: Option<String>,
        /// lightwalletd server URL (defaults to the network's).
        #[arg(long)]
        server: Option<String>,
    },
    /// Sync the wallet with a lightwalletd server (no other tool needed).
    Sync {
        /// Wallet directory.
        #[arg(long, env = "ZSETTLE_WALLET")]
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
        identity: Option<PathBuf>,
    },
    /// Create a new item account in this wallet. One account per item keeps a consignment to
    /// that item alone: whoever holds its listing sees nothing of your other items.
    NewItemAccount {
        /// Wallet directory.
        #[arg(long, env = "ZSETTLE_WALLET")]
        wallet: PathBuf,
        /// age identity file that decrypts the wallet's mnemonic. Omit it for a
        /// passphrase-protected wallet: the passphrase is asked for, or taken from
        /// ZSETTLE_PASSPHRASE.
        #[arg(long, env = "ZSETTLE_IDENTITY")]
        identity: Option<PathBuf>,
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
        /// age identity file that decrypts the wallet's mnemonic. Omit it for a
        /// passphrase-protected wallet: the passphrase is asked for, or taken from
        /// ZSETTLE_PASSPHRASE.
        #[arg(long, env = "ZSETTLE_IDENTITY")]
        identity: Option<PathBuf>,
        /// ZIP 32 account index of the payout account.
        #[arg(long, default_value_t = 0)]
        payout_account_index: u32,
        /// Item name (goes into the memo). Repeat it to mint several items in one
        /// transaction, each at its own fresh address.
        #[arg(long = "name", required = true)]
        names: Vec<String>,
        /// Item note value for each, in zatoshis or ZEC.
        #[arg(long, value_parser = zsettle_core::parse_zat, default_value_t = zsettle_core::item::MIN_ITEM_VALUE)]
        value: u64,
        /// lightwalletd server URL (default: the zec.rocks endpoint of the wallet's network).
        #[arg(long)]
        server: Option<String>,
    },
    /// List the item account's unspent notes (the references to publish).
    Items {
        /// Wallet directory.
        #[arg(long, env = "ZSETTLE_WALLET")]
        wallet: PathBuf,
        /// UUID of the item account.
        #[arg(long, env = "ZSETTLE_ITEM_ACCOUNT")]
        item_account: String,
    },
    /// Create or update the signed public listings file for this wallet's item account.
    Publish {
        #[command(flatten)]
        w: WalletArgs,
        /// age identity file that decrypts the wallet's mnemonic. Omit it for a
        /// passphrase-protected wallet: the passphrase is asked for, or taken from
        /// ZSETTLE_PASSPHRASE.
        #[arg(long, env = "ZSETTLE_IDENTITY")]
        identity: Option<PathBuf>,
        /// Listings file to create or update.
        #[arg(long, env = "ZSETTLE_LISTINGS")]
        listings: PathBuf,
        /// Item note to list, as <txid>:<output index>. Without it, the existing file is
        /// checked and signed again.
        #[arg(long)]
        item_note: Option<NoteRef>,
        /// Sign a file whose signature does not verify (after a format change, or a manual
        /// edit you do intend). Every entry is still checked.
        #[arg(long)]
        resign_unverified: bool,
        /// Take this item out of the file (delist without spending; a signed offer stays
        /// valid until its expiry, `cancel` ends it for good).
        #[arg(long)]
        remove: Option<NoteRef>,
        /// Display name.
        #[arg(long, default_value = "")]
        name: String,
        /// A short description for buyers (one line, at most 280 characters).
        #[arg(long)]
        description: Option<String>,
        /// One emoji shown as the item's picture.
        #[arg(long)]
        icon: Option<String>,
        /// Price, in zatoshis or ZEC (`0.001`, `0.001 ZEC`); more than 5,000 zatoshis, since a
        /// smaller payment is dust no wallet spends.
        #[arg(long, value_parser = zsettle_core::parse_zat)]
        price: Option<u64>,
        /// Maximum blocks between the chain tip and the transaction expiry.
        #[arg(long, default_value_t = zsettle_core::DEFAULT_MAX_EXPIRY_DELTA)]
        max_expiry_delta: u32,
    },
    /// Take an item off the market for good: spend its note to a fresh address of the item
    /// account, with one payout-account note paying the fee, and drop it from the listings
    /// file. Once mined, `items` shows the new note, which can be listed again later.
    Cancel {
        #[command(flatten)]
        w: WalletArgs,
        /// age identity file that decrypts the wallet's mnemonic. Omit it for a
        /// passphrase-protected wallet: the passphrase is asked for, or taken from
        /// ZSETTLE_PASSPHRASE.
        #[arg(long, env = "ZSETTLE_IDENTITY")]
        identity: Option<PathBuf>,
        /// The listed item note, as <txid>:<output index>.
        #[arg(long)]
        item_note: NoteRef,
        /// ZIP 32 account index of the payout account (signs the fee-paying spend).
        #[arg(long, default_value_t = 0)]
        payout_account_index: u32,
        /// Listings file to drop the item from (signed again).
        #[arg(long, env = "ZSETTLE_LISTINGS")]
        listings: Option<PathBuf>,
        /// Confirmations required on the notes.
        #[arg(long, default_value_t = 3)]
        min_confirmations: u32,
        /// lightwalletd server URL (defaults to the network's).
        #[arg(long)]
        server: Option<String>,
        /// Skip the sync first (the wallet must be fresh).
        #[arg(long)]
        no_sync: bool,
    },
    /// Check one trade PCZT against the listings, verify its proof, and sign the item action.
    Sign {
        #[command(flatten)]
        w: WalletArgs,
        /// age identity file that decrypts the wallet's mnemonic. Omit it for a
        /// passphrase-protected wallet: the passphrase is asked for, or taken from
        /// ZSETTLE_PASSPHRASE.
        #[arg(long, env = "ZSETTLE_IDENTITY")]
        identity: Option<PathBuf>,
        /// Listings file.
        #[arg(long, env = "ZSETTLE_LISTINGS")]
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
        /// age identity file that decrypts the wallet's mnemonic. Omit it for a
        /// passphrase-protected wallet: the passphrase is asked for, or taken from
        /// ZSETTLE_PASSPHRASE.
        #[arg(long, env = "ZSETTLE_IDENTITY")]
        identity: Option<PathBuf>,
        /// Listings file (kept in sync with the chain: sold items removed).
        #[arg(long, env = "ZSETTLE_LISTINGS")]
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
        /// Also serve over HTTP at this address (for example 127.0.0.1:8900): `GET /listings`
        /// returns the signed listings file; `POST /offer`
        /// with the envelope as the body answers `{"tag": …}`; `GET /reply/<tag>` returns the
        /// signed copy once it exists (202 while pending, 410 with the reason if refused). Put
        /// a TLS reverse proxy or a Tor hidden service in front of it for the internet.
        #[arg(long)]
        listen: Option<String>,
        /// Run one pass and exit; a failed pass exits with an error.
        #[arg(long)]
        once: bool,
    },
}

fn main() -> Result<()> {
    // Quiet by default; `ZSETTLE_LOG=1` shows the wallet library's progress lines.
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
        Cmd::Init {
            wallet,
            identity,
            network,
            server,
        } => {
            let network = parse_network(&network)?;
            let server = pick_server(server, network);
            let created = init_wallet(
                &wallet,
                network,
                &server,
                identity.as_deref(),
                &["payout", "item"],
            )?;
            let w = Wallet::open(&wallet)?;
            let mut db = w.db()?;
            let fund = parse_account(&created.accounts[0].account)?;
            let address = zsettle_core::mint::item_address(&mut db, w.network, fund)?;
            println!(
                "{}",
                serde_json::json!({
                    "wallet": created.dir, "network": created.network, "birthday": created.birthday,
                    "identity": identity, "protection": created.protection, "identity_created": created.identity_created,
                    "accounts": created.accounts, "fund": address,
                    "next": "send ZEC to `fund`, then `zsettle-seller sync`; `--payout-account` is the first UUID, `--item-account` the second (index 1)",
                })
            );
            let mut vars = vec![
                ("ZSETTLE_WALLET", wallet.display().to_string()),
                (
                    "ZSETTLE_PAYOUT_ACCOUNT",
                    created.accounts[0].account.to_string(),
                ),
                (
                    "ZSETTLE_ITEM_ACCOUNT",
                    created.accounts[1].account.to_string(),
                ),
                ("ZSETTLE_ITEM_ACCOUNT_INDEX", "1".to_string()),
                (
                    "ZSETTLE_LISTINGS",
                    wallet.join("listings.json").display().to_string(),
                ),
            ];
            if let Some(i) = &identity {
                vars.push(("ZSETTLE_IDENTITY", i.display().to_string()));
            }
            let (env_path, env) = zsettle_core::wallet::write_env_files(&wallet, &vars)?;
            eprintln!(
                "# later commands need no --wallet, --identity, --item-account, --payout-account or --listings after:\nsource {}\n{env}# PowerShell: . {}",
                env_path.display(),
                wallet.join("env.ps1").display()
            );
            Ok(())
        }
        Cmd::Accounts { wallet } => {
            let w = Wallet::open(&wallet)?;
            let db = w.db()?;
            println!("{}", serde_json::to_string(&list_accounts(&db)?)?);
            Ok(())
        }
        Cmd::Address { wallet, account } => {
            let w = Wallet::open(&wallet)?;
            let mut db = w.db()?;
            let address =
                zsettle_core::mint::item_address(&mut db, w.network, parse_account(&account)?)?;
            println!(
                "{}",
                serde_json::json!({ "account": account, "address": address })
            );
            Ok(())
        }
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
            w.upgrade(identity.as_deref())?;
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
            let seed = wallet.seed(identity.as_deref())?;
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
            names,
            value,
            server,
        } => {
            let wallet = Wallet::open(&w.wallet)?;
            let server = pick_server(server, wallet.network);
            let mut db = wallet.db()?;
            check_consensus_at_tip_blocking(&server, wallet.network)?;
            let seed = wallet.seed(identity.as_deref())?;
            let req = zsettle_core::mint::MintRequest {
                from_account: parse_account(&w.payout_account)?,
                from_account_index: payout_account_index,
                item_account: parse_account(&w.item_account)?,
                value_zat: value,
                names: names.clone(),
            };
            let (bytes, txid, indices) = zsettle_core::mint::mint(&wallet, &mut db, &seed, &req)
                .map_err(|e| {
                    let text = format!("{e:#}");
                    if text.contains("Insufficient balance") {
                        anyhow!("{text}: the payout account has no spendable note for this; a note received from another wallet is spendable after 10 confirmations, the wallet's own change after 3")
                    } else {
                        e
                    }
                })?;
            let response = zsettle_core::broadcast::send_raw_transaction_blocking(&server, bytes)?;
            println!(
                "{}",
                serde_json::json!({
                    "txid": txid, "names": names, "value_zat": value, "broadcast": response,
                    "notes": indices.iter().map(|i| i.map(|i| format!("{txid}:{i}"))).collect::<Vec<_>>(),
                    "next": "once mined and synced: publish --item-note <note> for each",
                })
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
            let wallet = Wallet::open(&wallet)?;
            let mut db = wallet.db()?;
            let server = pick_server(server, wallet.network);
            check_consensus_at_tip_blocking(&server, wallet.network)?;
            let seed = wallet.seed(identity.as_deref())?;
            let req = zsettle_core::mint::PayRequest {
                from_account: parse_account(&account)?,
                from_account_index: account_index,
                to,
                value_zat: amount,
                memo,
            };
            let (bytes, txid) = zsettle_core::mint::pay(&wallet, &mut db, &seed, &req)?;
            let response = zsettle_core::broadcast::send_raw_transaction_blocking(&server, bytes)?;
            println!(
                "{}",
                serde_json::json!({ "txid": txid, "amount_zat": amount, "broadcast": response })
            );
            Ok(())
        }
        Cmd::Items {
            wallet,
            item_account,
        } => {
            let wallet = Wallet::open(&wallet)?;
            let db = wallet.db()?;
            let notes =
                zsettle_core::mint::unspent_notes(&wallet, &db, parse_account(&item_account)?)?;
            for n in &notes {
                println!(
                    "{}  {:>8} zat ({})  mined {}",
                    n.note,
                    n.value_zat,
                    zec(n.value_zat),
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
            description,
            icon,
            price,
            max_expiry_delta,
            resign_unverified,
            remove,
        } => {
            let wallet = Wallet::open(&w.wallet)?;
            let mut db = wallet.db()?;
            let item_account = parse_account(&w.item_account)?;
            let item_fvk = account_orchard_fvk(&db, item_account)?;
            let ask = item_ask(
                &wallet,
                &db,
                identity.as_deref(),
                item_account,
                w.item_account_index,
            )?;
            let mut l = if listings.exists() {
                let l = Listings::read(&listings)?;
                if l.item_fvk()? != item_fvk {
                    return Err(anyhow!(
                        "{} belongs to another item account",
                        listings.display()
                    ));
                }
                if resign_unverified {
                    eprintln!(
                        "warning: signing {} without verifying its signature",
                        listings.display()
                    );
                } else {
                    l.verify_signature()
                        .with_context(|| format!("{} is not trustworthy; pass --resign-unverified to sign it again if you edited it on purpose", listings.display()))?;
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
            let (target, _) = heights(&db, 1)?;
            if let Some(item_note) = item_note {
                let price = price.ok_or_else(|| anyhow!("--price is required with --item-note"))?;
                // A new entry must name a note this wallet can spend.
                let note = ironwood_note(&db, item_note, target)?;
                if item_fvk.scope_for_address(&note.note.recipient()).is_none() {
                    return Err(anyhow!("note {item_note} is not owned by the item account"));
                }
                let description = description
                    .map(|d| d.trim().to_string())
                    .filter(|d| !d.is_empty());
                let icon = icon.map(|i| i.trim().to_string()).filter(|i| !i.is_empty());
                l.upsert(ListedItem {
                    note: item_note.to_string(),
                    name,
                    price_zat: price,
                    max_expiry_delta,
                    description,
                    icon,
                });
            }
            if let Some(note) = remove {
                if !l.remove(&note.to_string()) {
                    return Err(anyhow!("{note} is not listed in {}", listings.display()));
                }
            }
            // Every entry is checked, the new one and the ones already in the file.
            for item in &l.items {
                item.check_details()
                    .with_context(|| format!("item {}", item.note))?;
                if item.price_zat < MIN_PRICE_ZAT {
                    return Err(anyhow!(
                        "{}: price must be at least {MIN_PRICE_ZAT} zat: a payment at or below the ZIP 317 marginal fee is dust that no wallet spends",
                        item.note
                    ));
                }
                if item.max_expiry_delta != DEFAULT_MAX_EXPIRY_DELTA {
                    eprintln!(
                        "warning: {}: an expiry limit other than {DEFAULT_MAX_EXPIRY_DELTA} blocks makes every trade of this item recognisable on chain",
                        item.note
                    );
                }
                if let Ok(r) = item.note_ref() {
                    if let Ok(n) = ironwood_note(&db, r, target) {
                        if item_fvk.scope_for_address(&n.note.recipient()).is_none() {
                            return Err(anyhow!(
                                "note {} is not owned by the item account",
                                item.note
                            ));
                        }
                    }
                }
            }
            if w.item_account_index == 0 {
                eprintln!(
                    "warning: account index 0 is a wallet's paying account; publishing its viewing key exposes every payment it ever receives. Keep items in an account of their own (new-item-account)"
                );
            }
            let held = zsettle_core::mint::unspent_notes(&wallet, &db, item_account)?.len();
            if held > 1 {
                eprintln!(
                    "warning: the item account holds {held} notes; whoever has this file sees all of them and every sale. One account per item (new-item-account) keeps consignments apart"
                );
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
        Cmd::Cancel {
            w,
            identity,
            item_note,
            payout_account_index,
            listings,
            min_confirmations,
            server,
            no_sync,
        } => {
            let wallet = Wallet::open(&w.wallet)?;
            let mut db = wallet.db()?;
            let server = pick_server(server, wallet.network);
            if !no_sync {
                sync_blocking(&wallet, &mut db, &server)?;
            }
            let item_account = parse_account(&w.item_account)?;
            let payout_account = parse_account(&w.payout_account)?;
            let (target, _) = heights(&db, min_confirmations)?;
            let item_value = ironwood_note(&db, item_note, target)?.note.value().inner();
            let fee = zsettle_core::zip317_fee(2);
            // The smallest payout note that covers the fee exactly or leaves change above dust.
            let mut exclude = Vec::new();
            let mut fitting = Vec::new();
            while let Some(n) = largest_spendable_note(&db, payout_account, target, &exclude)? {
                exclude.push(n);
                let v = ironwood_note(&db, n, target)?.note.value().inner();
                if v == fee || v > fee + ZIP317_MARGINAL_FEE {
                    fitting.push((v, n));
                }
            }
            let Some(&(payout_value, payout_note)) = fitting.iter().min_by_key(|(v, _)| *v) else {
                return Err(anyhow!(
                    "no payout note can pay the {fee} zat fee: it needs exactly {fee} zat or more than {} zat",
                    fee + ZIP317_MARGINAL_FEE
                ));
            };
            let mut outputs = vec![zsettle_core::trade::Output {
                account: item_account,
                value: item_value,
                internal: false,
            }];
            if payout_value > fee {
                outputs.push(zsettle_core::trade::Output {
                    account: payout_account,
                    value: payout_value - fee,
                    internal: true,
                });
            }
            let seed = wallet.seed(identity.as_deref())?;
            let usk_item = wallet.spending_key(&seed, w.item_account_index)?;
            if FullViewingKey::from(usk_item.orchard()) != account_orchard_fvk(&db, item_account)? {
                return Err(anyhow!(
                    "account index {} does not derive the item account",
                    w.item_account_index
                ));
            }
            let usk_payout = wallet.spending_key(&seed, payout_account_index)?;
            if FullViewingKey::from(usk_payout.orchard())
                != account_orchard_fvk(&db, payout_account)?
            {
                return Err(anyhow!(
                    "account index {payout_account_index} does not derive the payout account"
                ));
            }
            check_consensus_at_tip_blocking(&server, wallet.network)?;
            let built = zsettle_core::trade::build_transfer(
                &mut db,
                wallet.network,
                &[(item_account, item_note), (payout_account, payout_note)],
                &outputs,
                min_confirmations,
                true,
            )?;
            let item_ask = SpendAuthorizingKey::from(usk_item.orchard());
            let (pczt, n1) = pczt_ops::sign_matching_actions(built.pczt, &item_ask)?;
            let (pczt, n2) = pczt_ops::sign_matching_actions(
                pczt,
                &SpendAuthorizingKey::from(usk_payout.orchard()),
            )?;
            if n1 != 1 || n2 != 1 {
                return Err(anyhow!(
                    "expected one signature per account, got {n1} and {n2}"
                ));
            }
            let tx = pczt_ops::extract(pczt)?;
            let bytes = pczt_ops::tx_bytes(&tx)?;
            zsettle_core::broadcast::send_raw_transaction_blocking(&server, bytes)?;
            let mut delisted = false;
            if let Some(path) = listings {
                let mut l = Listings::read_verified(&path)?;
                if l.remove(&item_note.to_string()) {
                    l.sign(&item_ask, max_scanned_height(&db)?.unwrap_or(0))?;
                    l.write(&path)?;
                    delisted = true;
                }
            }
            println!(
                "{}",
                serde_json::json!({
                    "cancelled": item_note.to_string(), "txid": tx.txid().to_string(), "fee": built.fee,
                    "fee_paid_by": payout_note.to_string(), "delisted": delisted, "expiry_height": built.expiry_height,
                    "next": "once mined, `items` shows the item's new note; `publish --item-note` lists it again",
                })
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
            eprintln!("proof verified");
            let outcome = process(&db, &w, wallet.network, &l, &pczt, &anchors, true)?;
            eprintln!("{}", describe(&outcome.result));
            if outcome.item.is_none() && outcome.unavailable > 0 {
                eprintln!(
                    "note: {} listed items are not spendable in this wallet right now (sold, unscanned, or too recent); the trade may be for one of them",
                    outcome.unavailable
                );
            }
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
                identity.as_deref(),
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
            listen,
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
                    identity.as_deref(),
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
                network: wallet.network,
                ask: &ask,
                vk: &vk,
                listings: &listings,
                inbox: &inbox,
                outbox: &outbox,
                server: &server,
                no_sync,
                sync_cmd: sync_cmd.as_deref(),
                verified: RefCell::new(HashSet::new()),
                deferred: std::cell::Cell::new(0),
                deferred_since: RefCell::new(std::collections::HashMap::new()),
            };
            if let Some(addr) = listen {
                let listener = std::net::TcpListener::bind(&addr)
                    .with_context(|| format!("listening on {addr}"))?;
                println!(
                    "{}",
                    serde_json::json!({ "event": "listening", "address": addr })
                );
                let (inbox, outbox, listings) = (inbox.clone(), outbox.clone(), listings.clone());
                thread::spawn(move || serve_http(listener, inbox, outbox, listings));
            }
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
    network: zcash_protocol::consensus::Network,
    ask: &'a SpendAuthorizingKey,
    vk: &'a VerifyingKey,
    listings: &'a Path,
    inbox: &'a Path,
    outbox: &'a Path,
    server: &'a str,
    no_sync: bool,
    sync_cmd: Option<&'a str>,
    /// Content tags whose proof this process has verified: a deferred envelope is not
    /// verified again.
    verified: RefCell<HashSet<String>>,
    /// Envelopes deferred in the current pass.
    deferred: std::cell::Cell<usize>,
    /// Chain tip at which each waiting envelope (by file name) was first deferred.
    deferred_since: RefCell<std::collections::HashMap<String, u32>>,
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
        // Filed envelopes and signed replies are kept only as long as a buyer or a restart
        // could need them.
        let now = SystemTime::now();
        for dir in [
            self.inbox.join("done"),
            self.outbox.to_path_buf(),
            self.inbox.join("rejected"),
        ] {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let old = entry
                    .path()
                    .extension()
                    .is_some_and(|x| x == "pczt" || x == "txt")
                    && entry
                        .metadata()
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| now.duration_since(t).ok())
                        .is_some_and(|age| age > Duration::from_secs(MAX_DONE_AGE_SECS));
                if old {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        self.deferred.set(0);
        let anchors = recent_ironwood_anchors(&mut db, ANCHOR_DEPTH)?;
        let mut l = Listings::read_verified(self.listings)?;
        if reconcile(&wallet, &db, &mut l)? {
            l.sign(self.ask, max_scanned_height(&db)?.unwrap_or(0))?;
            l.write(self.listings)?;
        }
        let paths = self.envelopes()?;
        {
            let names: HashSet<String> = paths
                .iter()
                .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string))
                .collect();
            self.deferred_since
                .borrow_mut()
                .retain(|name, _| names.contains(name));
        }
        for path in paths {
            if let Err(e) = self.handle(&db, &l, &anchors, &path) {
                println!(
                    "{}",
                    serde_json::json!({ "event": "error", "file": path, "error": format!("{e:#}") })
                );
            }
        }
        Ok(())
    }

    /// Envelopes waiting in the inbox, in arrival order (then by name), skipping files still
    /// being written.
    fn envelopes(&self) -> Result<Vec<PathBuf>> {
        let now = SystemTime::now();
        let mut paths: Vec<PathBuf> = fs::read_dir(self.inbox)
            .with_context(|| format!("reading {}", self.inbox.display()))?
            .filter_map(|e| e.ok())
            // Regular files only: a symlink to a FIFO or a device would stall the pass.
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .filter(|e| e.path().extension().is_some_and(|x| x == "pczt"))
            .filter(|e| settled(e, now))
            .map(|e| e.path())
            .collect();
        paths.sort_by_cached_key(|p| (fs::metadata(p).and_then(|m| m.modified()).ok(), p.clone()));
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
            return self.reject(path, format!("too large: {size} bytes"));
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
            let filed = self
                .inbox
                .join("done")
                .join(path.file_name().unwrap_or_default());
            if filed.exists() {
                // The same bytes are already filed: a re-post adds nothing.
                fs::remove_file(path).with_context(|| format!("removing {}", path.display()))?;
            } else {
                move_into(path, &self.inbox.join("done"))?;
            }
            return Ok(());
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
            Err(Refusal::Reject(reason)) => self.reject(path, reason),
            Err(Refusal::Defer(reason)) => {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("envelope")
                    .to_string();
                let tip = last_tip(db);
                // A tip of zero means the wallet view was unreadable: judge the wait later.
                if tip != 0 {
                    let since = *self.deferred_since.borrow_mut().entry(name).or_insert(tip);
                    if tip > since.saturating_add(DEFER_MAX_BLOCKS) {
                        return self.reject(
                            path,
                            format!("refused: waited {DEFER_MAX_BLOCKS} blocks for the chain view to catch up with this offer, and it did not; build the offer again"),
                        );
                    }
                }
                if self.deferred.get() >= MAX_DEFERRED {
                    return self.reject(
                        path,
                        "refused: too many offers are waiting for a later pass; send it again later"
                            .into(),
                    );
                }
                self.deferred.set(self.deferred.get() + 1);
                println!(
                    "{}",
                    serde_json::json!({ "event": "deferred", "file": path, "reason": reason })
                );
                Ok(())
            }
        }
    }

    fn reject(&self, path: &Path, reason: String) -> Result<()> {
        println!(
            "{}",
            serde_json::json!({ "event": "rejected", "file": path, "reason": reason })
        );
        // The buyer keeps the original; only the reason is kept here, and never more than
        // MAX_REASON_FILES of them.
        let rejected = self.inbox.join("rejected");
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("envelope")
            .to_string();
        let kept = fs::read_dir(&rejected).map(|d| d.count()).unwrap_or(0);
        if kept < MAX_REASON_FILES {
            // The latest judgement of these bytes is the one served.
            let why = rejected.join(format!("{name}.reason.txt"));
            fs::write(&why, &reason).with_context(|| format!("writing {}", why.display()))?;
        }
        fs::remove_file(path).with_context(|| format!("removing {}", path.display()))?;
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
    ) -> std::result::Result<(ListedItem, Approved, Vec<u8>), Refusal> {
        let pczt = pczt_ops::from_bytes(bytes)
            .map_err(|e| Refusal::Reject(format!("unreadable: {e:#}")))?;
        // Cheap checks first. The proof is verified last and once per envelope, so an offer
        // that fails anything else, or waits, costs this service nothing.
        let actions = pczt_ops::action_count(&pczt)
            .map_err(|e| Refusal::Reject(format!("unreadable: {e:#}")))?;
        if actions > MAX_TRADE_ACTIONS {
            return Err(Refusal::Reject(format!(
                "refused: {actions} actions; a trade has two or three"
            )));
        }
        let expiry = *pczt.global().expiry_height();
        // Stage one: public data only (branch, expiry, anchor, the listed item's spend). A
        // refusal here tells the sender nothing the listings file does not already give.
        // No envelope waits longer than the longest listed expiry allows: an envelope that
        // can never settle must not sit in the inbox forever.
        let horizon = l
            .items
            .iter()
            .map(|i| i.max_expiry_delta)
            .max()
            .unwrap_or(DEFAULT_MAX_EXPIRY_DELTA)
            .saturating_add(1 + LAG_SLACK);
        let tip_now = last_tip(db);
        let outcome = match process(db, self.w, self.network, l, &pczt, anchors, false) {
            Ok(outcome) => outcome,
            // A failure of this pass rather than of the envelope: keep it while its
            // transaction can still be mined. The detail stays in this log.
            Err(e)
                if expiry != 0
                    && (tip_now == 0
                        || (expiry > tip_now && expiry <= tip_now.saturating_add(horizon))) =>
            {
                eprintln!("could not judge an envelope this pass: {e:#}");
                return Err(Refusal::Defer(
                    "error: this pass could not judge the envelope; it will be judged again".into(),
                ));
            }
            Err(e) => {
                eprintln!("could not judge an envelope: {e:#}");
                return Err(Refusal::Reject(
                    "error: this envelope could not be judged".into(),
                ));
            }
        };
        if outcome.item.is_none()
            && outcome.unavailable > 0
            && expiry > outcome.tip
            && expiry <= outcome.tip.saturating_add(horizon)
        {
            return Err(Refusal::Defer(format!(
                "spends no listed item this wallet can spend, and {} listed items are not spendable here yet (unscanned or too recent)",
                outcome.unavailable
            )));
        }
        match outcome.result {
            Ok(_) => {}
            // The buyer may be a block or two ahead of this wallet: a root it has not scanned
            // yet, or an expiry measured from a later tip. Judge again next pass.
            Err(PolicyError::UnknownAnchor) if expiry > outcome.tip => {
                return Err(Refusal::Defer(
                    "anchor not known to this wallet yet (buyer ahead of it?)".into(),
                ))
            }
            Err(PolicyError::ExpiryTooLong {
                expiry: e,
                tip,
                max_delta,
            }) if e <= tip.saturating_add(max_delta).saturating_add(1 + LAG_SLACK) => {
                return Err(Refusal::Defer(format!(
                    "expiry {e} is {} blocks past the limit; the buyer may be ahead of this wallet",
                    e - tip - max_delta - 1
                )))
            }
            Err(e) => return Err(Refusal::Reject(format!("refused: {e}"))),
        }
        let item = outcome.item.ok_or_else(|| {
            Refusal::Reject("error: approved trade without a listed item".to_string())
        })?;
        // Stage two: the proof, once per envelope.
        let tag = pczt_ops::envelope_tag(bytes);
        if !self.verified.borrow().contains(&tag) {
            pczt_ops::verify_proof(&pczt, self.vk)
                .map_err(|e| Refusal::Reject(format!("refused: {e:#}")))?;
            let mut verified = self.verified.borrow_mut();
            if verified.len() >= MAX_VERIFIED_TAGS {
                verified.clear();
            }
            verified.insert(tag);
        }
        // Stage three: the checks that use the seller's own keys. With a valid proof the
        // sender knows every note in the bundle, so these cannot be answered about anyone
        // else's notes.
        let approved = match process(db, self.w, self.network, l, &pczt, anchors, true) {
            Ok(full) => full
                .result
                .map_err(|e| Refusal::Reject(format!("refused: {e}")))?,
            Err(e) => {
                eprintln!("could not judge an envelope: {e:#}");
                return Err(Refusal::Reject(
                    "error: this envelope could not be judged".into(),
                ));
            }
        };
        let signed = pczt_ops::sign_ironwood_action(pczt, approved.item_action_index, self.ask)
            .map_err(|e| Refusal::Reject(format!("signing failed: {e:#}")))?;
        let bytes = pczt_ops::to_bytes(&signed)
            .map_err(|e| Refusal::Reject(format!("signing failed: {e:#}")))?;
        Ok((item, approved, bytes))
    }
}

/// The last height this wallet scanned, or zero.
/// Zatoshis as ZEC, for people.
fn zec(zat: u64) -> String {
    format!("{} ZEC", zat as f64 / 1e8)
}

fn last_tip(db: &Db) -> u32 {
    max_scanned_height(db).ok().flatten().unwrap_or(0)
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
                    serde_json::json!({ "event": "sold", "item": item.note, "name": item.name, "price_zat": item.price_zat, "txid": txid })
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
    /// Listed items this wallet cannot spend right now (unscanned, too recent, sold). A trade
    /// that spends none of the others may be for one of these.
    unavailable: usize,
    /// Chain tip the checks were made against.
    tip: u32,
}

/// Why an envelope was not signed in this pass.
enum Refusal {
    /// Final: the envelope moves to `rejected/`.
    Reject(String),
    /// Try again next pass: the wallet may not have caught up with the chain yet.
    Defer(String),
}

/// Match a trade against the listings and run the policy checks.
fn process(
    db: &Db,
    w: &WalletArgs,
    network: zcash_protocol::consensus::Network,
    l: &Listings,
    pczt: &pczt::Pczt,
    anchors: &[orchard::Anchor],
    with_secrets: bool,
) -> Result<Outcome> {
    let (target, _) = heights(db, 1)?;
    let tip: u32 =
        u32::from(zcash_protocol::consensus::BlockHeight::from(target)).saturating_sub(1);
    let item_fvk = account_orchard_fvk(db, parse_account(&w.item_account)?)?;
    let item_account = parse_account(&w.item_account)?;
    // Only the listed account's own notes: a listings holder can derive these, so the
    // decision reveals nothing about the rest of the wallet.
    let seller_nfs = account_unspent_nullifiers(db, item_account)?;
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
        return Ok(Outcome {
            item: None,
            result: Err(PolicyError::ItemNotSpent),
            unavailable,
            tip,
        });
    };
    let ivk = prepared_ivk(&payout_key(db, w)?);
    let policy = SellerPolicy {
        price_zat: item.price_zat,
        max_expiry_delta: item.max_expiry_delta,
        tip,
        branch_id: u32::from(BranchId::for_height(&network, BlockHeight::from(tip + 1))),
        payout: listed_payout(l, network)?,
    };
    let result = if with_secrets {
        check(pczt, &nf, &seller_nfs, &ivk, &policy, anchors)
    } else {
        check_public(pczt, &nf, &policy, anchors)
    };
    Ok(Outcome {
        item: Some(item),
        result,
        unavailable,
        tip,
    })
}

/// The item account's spend authorizing key, derived from the encrypted seed, checked
/// against the account's viewing key so a wrong index fails here and not at the buyer.
fn item_ask(
    wallet: &Wallet,
    db: &Db,
    identity: Option<&Path>,
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
/// The Orchard receiver of the listed payout address.
fn listed_payout(
    l: &Listings,
    network: zcash_protocol::consensus::Network,
) -> Result<orchard::Address> {
    match Address::decode(&network, &l.payout_address) {
        Some(Address::Unified(ua)) => ua.orchard().copied(),
        _ => None,
    }
    .ok_or_else(|| {
        anyhow!("the listings payout address is not a unified address with an Orchard receiver")
    })
}

fn check_payout(
    l: &Listings,
    payout_fvk: &FullViewingKey,
    network: zcash_protocol::consensus::Network,
) -> Result<()> {
    let addr = listed_payout(l, network)?;
    if payout_fvk.scope_for_address(&addr) != Some(orchard::keys::Scope::External) {
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
        // A child must not inherit the wallet passphrase.
        .env_remove("ZSETTLE_PASSPHRASE")
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
        // A modification time in the future (clock changes, odd copies) must not keep the
        // file waiting forever.
        Ok(modified) => match now.duration_since(modified) {
            Ok(age) => age >= Duration::from_secs(SETTLE_SECS),
            Err(_) => true,
        },
        Err(_) => true,
    }
}

/// Write a file through a temporary name so readers never see a partial file.
/// The HTTP face of the service: offers in, replies out, with limits of its own: a request
/// head of at most 8 KiB read with a timeout, bodies refused above 1 MiB before they are
/// read, a bounded number of connections, one request per connection. An envelope lands in
/// the inbox under its content tag, so the directory loop handles it like a dropped file,
/// and each buyer can fetch only the reply to its own envelope.
fn serve_http(listener: std::net::TcpListener, inbox: PathBuf, outbox: PathBuf, listings: PathBuf) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    let live = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        if live.load(Ordering::Relaxed) >= MAX_HTTP_CONNECTIONS {
            let _ = respond(&mut stream, 503, "text/plain", b"busy; try again later");
            continue;
        }
        live.fetch_add(1, Ordering::Relaxed);
        let (live, inbox, outbox, listings) = (
            Arc::clone(&live),
            inbox.clone(),
            outbox.clone(),
            listings.clone(),
        );
        thread::spawn(move || {
            handle_http(stream, &inbox, &outbox, &listings);
            live.fetch_sub(1, Ordering::Relaxed);
        });
    }
}

/// One HTTP request: `GET /listings`, `POST /offer` or `GET /reply/<tag>`.
fn handle_http(mut stream: std::net::TcpStream, inbox: &Path, outbox: &Path, listings: &Path) {
    use std::io::Read;
    let timeout = Some(Duration::from_secs(HTTP_TIMEOUT_SECS));
    if stream.set_read_timeout(timeout).is_err() || stream.set_write_timeout(timeout).is_err() {
        return;
    }
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    let body_start = loop {
        let n = match stream.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        head.extend_from_slice(&buf[..n]);
        if let Some(at) = find(&head, b"\r\n\r\n") {
            break at + 4;
        }
        if head.len() > MAX_HTTP_HEAD {
            let _ = respond(&mut stream, 431, "text/plain", b"request head too large");
            return;
        }
    };
    let (head_bytes, rest) = head.split_at(body_start);
    let head_text = String::from_utf8_lossy(head_bytes);
    let mut lines = head_text.lines();
    let mut request_line = lines.next().unwrap_or("").split_whitespace();
    let method = request_line.next().unwrap_or("");
    let path = request_line.next().unwrap_or("");
    let mut content_length: u64 = 0;
    let mut expects_continue = false;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(u64::MAX);
            } else if name.trim().eq_ignore_ascii_case("expect")
                && value.trim().eq_ignore_ascii_case("100-continue")
            {
                expects_continue = true;
            }
        }
    }
    match (method, path) {
        ("POST", "/offer") => {
            if content_length == 0 || content_length > MAX_ENVELOPE_BYTES {
                let _ = respond(
                    &mut stream,
                    413,
                    "text/plain",
                    b"the envelope must be between 1 byte and 1 MiB, with a Content-Length",
                );
                return;
            }
            let pending = fs::read_dir(inbox)
                .map(|d| {
                    d.filter_map(|e| e.ok())
                        .filter(|e| e.path().extension().is_some_and(|x| x == "pczt"))
                        .count()
                })
                .unwrap_or(0);
            if pending >= MAX_PENDING_OFFERS {
                let _ = respond(
                    &mut stream,
                    503,
                    "text/plain",
                    b"too many offers waiting; try again later",
                );
                return;
            }
            if expects_continue {
                use std::io::Write;
                if stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").is_err() {
                    return;
                }
            }
            let mut body = rest.to_vec();
            body.truncate(content_length as usize);
            while (body.len() as u64) < content_length {
                let want = ((content_length - body.len() as u64) as usize).min(buf.len());
                match stream.read(&mut buf[..want]) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => body.extend_from_slice(&buf[..n]),
                }
            }
            let tag = pczt_ops::envelope_tag(&body);
            let file = inbox.join(format!("{tag}.pczt"));
            let stored = if file.exists() {
                Ok(())
            } else {
                // A refusal of an earlier copy of these bytes must not answer for this one.
                let _ = fs::remove_file(
                    inbox
                        .join("rejected")
                        .join(format!("{tag}.pczt.reason.txt")),
                );
                write_atomic(&file, &body)
            };
            match stored {
                Ok(()) => {
                    let answer =
                        serde_json::json!({ "tag": tag, "reply": format!("/reply/{tag}") });
                    let _ = respond(
                        &mut stream,
                        200,
                        "application/json",
                        answer.to_string().as_bytes(),
                    );
                }
                Err(e) => {
                    eprintln!("cannot store an offer: {e:#}");
                    let _ = respond(
                        &mut stream,
                        500,
                        "text/plain",
                        b"cannot store the offer now; send it again later",
                    );
                }
            }
        }
        ("GET", "/listings") => match fs::read(listings) {
            Ok(bytes) => {
                let _ = respond(&mut stream, 200, "application/json", &bytes);
            }
            Err(_) => {
                let _ = respond(&mut stream, 404, "text/plain", b"no listings file");
            }
        },
        ("GET", p) if p.starts_with("/reply/") => {
            let tag = &p["/reply/".len()..];
            if tag.len() != 32
                || !tag
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                let _ = respond(&mut stream, 400, "text/plain", b"bad tag");
                return;
            }
            let signed = outbox.join(format!("{tag}.{tag}.signed.pczt"));
            let reason = inbox
                .join("rejected")
                .join(format!("{tag}.pczt.reason.txt"));
            if let Ok(bytes) = fs::read(&signed) {
                let _ = respond(&mut stream, 200, "application/octet-stream", &bytes);
            } else if inbox.join(format!("{tag}.pczt")).exists()
                || inbox.join("done").join(format!("{tag}.pczt")).exists()
            {
                // An envelope under judgement wins over a stale reason from an earlier copy.
                let _ = respond(&mut stream, 202, "text/plain", b"pending");
            } else if let Ok(why) = fs::read_to_string(&reason) {
                let _ = respond(&mut stream, 410, "text/plain", why.as_bytes());
            } else {
                let _ = respond(
                    &mut stream,
                    410,
                    "text/plain",
                    b"no longer available: refused with its reason not kept, expired from the service's files, or never received",
                );
            }
        }
        _ => {
            let _ = respond(&mut stream, 404, "text/plain", b"not found");
        }
    }
}

/// Write one HTTP/1.1 response and close.
fn respond(
    stream: &mut std::net::TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    use std::io::Write;
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        404 => "Not Found",
        410 => "Gone",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
}

/// Move a file into `dir` without ever replacing a file already there: a later file with
/// the same name gets a numbered suffix.
fn move_into(path: &Path, dir: &Path) -> Result<PathBuf> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow!("{} has no usable file name", path.display()))?;
    let mut dest = dir.join(name);
    let mut n = 1u32;
    while dest.exists() {
        // Keep the extension, so the daily cleanup recognises the file.
        dest = dir.join(match name.rsplit_once('.') {
            Some((stem, ext)) => format!("{stem}.{n}.{ext}"),
            None => format!("{name}.{n}"),
        });
        n += 1;
    }
    fs::rename(path, &dest)
        .with_context(|| format!("moving {} to {}", path.display(), dir.display()))?;
    Ok(dest)
}

fn parse_network(name: &str) -> Result<zcash_protocol::consensus::Network> {
    match name {
        "main" => Ok(zcash_protocol::consensus::Network::MainNetwork),
        "test" => Ok(zcash_protocol::consensus::Network::TestNetwork),
        other => Err(anyhow!("unknown network {other:?}: use main or test")),
    }
}

fn network_name(n: zcash_protocol::consensus::Network) -> &'static str {
    match n {
        zcash_protocol::consensus::Network::MainNetwork => "main",
        zcash_protocol::consensus::Network::TestNetwork => "test",
    }
}
