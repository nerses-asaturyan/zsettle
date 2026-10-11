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
use zsettle_core::broadcast::{check_consensus_at_tip_blocking, DEFAULT_SERVER};
use zsettle_core::listing::{ListedItem, Listings};
use zsettle_core::mint::{note_state, NoteState};
use zsettle_core::pczt as pczt_ops;
use zsettle_core::policy::{check, describe, prepared_ivk, Approved, PolicyError, SellerPolicy};
use zsettle_core::wallet::{
    account_birthday, account_orchard_fvk, create_account, heights, init_wallet, ironwood_note,
    largest_spendable_note, list_accounts, max_scanned_height, next_orchard_address, parse_account,
    recent_ironwood_anchors, sync_blocking, unspent_ironwood_nullifiers, Db, NoteRef, Wallet,
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
        #[arg(long)]
        identity: Option<PathBuf>,
        /// `main` or `test`.
        #[arg(long, default_value = "main")]
        network: String,
        /// lightwalletd server URL (for the birthday).
        #[arg(long, default_value = DEFAULT_SERVER)]
        server: String,
    },
    /// List the wallet's accounts: name, UUID, ZIP 32 index, birthday.
    Accounts {
        /// Wallet directory.
        #[arg(long)]
        wallet: PathBuf,
    },
    /// Print a fresh receiving address of an account (Orchard only). Each call gives a new
    /// one; reuse links payments.
    Address {
        /// Wallet directory.
        #[arg(long)]
        wallet: PathBuf,
        /// Account UUID.
        #[arg(long)]
        account: String,
    },
    /// Sync the wallet with a lightwalletd server (no other tool needed).
    Sync {
        /// Wallet directory.
        #[arg(long)]
        wallet: PathBuf,
        /// lightwalletd server URL.
        #[arg(long, default_value = DEFAULT_SERVER)]
        server: String,
    },
    /// Create a new item account in this wallet. One account per item keeps a consignment to
    /// that item alone: whoever holds its listing sees nothing of your other items.
    NewItemAccount {
        /// Wallet directory.
        #[arg(long)]
        wallet: PathBuf,
        /// age identity file that decrypts the wallet's mnemonic. Omit it for a
        /// passphrase-protected wallet: the passphrase is asked for, or taken from
        /// ZSETTLE_PASSPHRASE.
        #[arg(long)]
        identity: Option<PathBuf>,
        /// Account name.
        #[arg(long)]
        name: String,
        /// lightwalletd server URL (for the account birthday).
        #[arg(long, default_value = DEFAULT_SERVER)]
        server: String,
    },
    /// Mint an item: pay a small amount from the payout account to the item account, memo = name.
    Mint {
        #[command(flatten)]
        w: WalletArgs,
        /// age identity file that decrypts the wallet's mnemonic. Omit it for a
        /// passphrase-protected wallet: the passphrase is asked for, or taken from
        /// ZSETTLE_PASSPHRASE.
        #[arg(long)]
        identity: Option<PathBuf>,
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
        #[arg(long, default_value = DEFAULT_SERVER)]
        server: String,
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
        /// age identity file that decrypts the wallet's mnemonic. Omit it for a
        /// passphrase-protected wallet: the passphrase is asked for, or taken from
        /// ZSETTLE_PASSPHRASE.
        #[arg(long)]
        identity: Option<PathBuf>,
        /// Listings file to create or update.
        #[arg(long)]
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
        /// Price in zatoshis (more than 5,000: smaller payments are dust no wallet spends).
        #[arg(long)]
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
        #[arg(long)]
        identity: Option<PathBuf>,
        /// The listed item note, as <txid>:<output index>.
        #[arg(long)]
        item_note: NoteRef,
        /// ZIP 32 account index of the payout account (signs the fee-paying spend).
        #[arg(long, default_value_t = 0)]
        payout_account_index: u32,
        /// Listings file to drop the item from (signed again).
        #[arg(long)]
        listings: Option<PathBuf>,
        /// Confirmations required on the notes.
        #[arg(long, default_value_t = 3)]
        min_confirmations: u32,
        /// lightwalletd server URL.
        #[arg(long, default_value = DEFAULT_SERVER)]
        server: String,
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
        #[arg(long)]
        identity: Option<PathBuf>,
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
        /// lightwalletd server URL for the sync that precedes the check.
        #[arg(long, default_value = DEFAULT_SERVER)]
        server: String,
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
        #[arg(long)]
        identity: Option<PathBuf>,
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
        /// lightwalletd server URL for the built-in sync.
        #[arg(long, default_value = DEFAULT_SERVER)]
        server: String,
        /// Do not sync; something else keeps the wallet up to date.
        #[arg(long)]
        no_sync: bool,
        /// Shell command to run instead of the built-in sync.
        #[arg(long)]
        sync_cmd: Option<String>,
        /// Also take offers over HTTP at this address (for example 127.0.0.1:8900): `POST /offer`
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
            let created = init_wallet(
                &wallet,
                parse_network(&network)?,
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
            let mut db = wallet.db()?;
            let report = sync_blocking(&wallet, &mut db, &server)?;
            println!("{}", serde_json::to_string(&report)?);
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
            name,
            value,
            server,
        } => {
            let wallet = Wallet::open(&w.wallet)?;
            let mut db = wallet.db()?;
            check_consensus_at_tip_blocking(&server, wallet.network)?;
            let seed = wallet.seed(identity.as_deref())?;
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
                let payout_ua = UnifiedAddress::from_receivers(Some(payout), None, None)
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
                l.upsert(ListedItem {
                    note: item_note.to_string(),
                    name,
                    price_zat: price,
                    max_expiry_delta,
                });
            }
            if let Some(note) = remove {
                if !l.remove(&note.to_string()) {
                    return Err(anyhow!("{note} is not listed in {}", listings.display()));
                }
            }
            // Every entry is checked, the new one and the ones already in the file.
            for item in &l.items {
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
            if !no_sync {
                sync_blocking(&wallet, &mut db, &server)?;
            }
            let item_account = parse_account(&w.item_account)?;
            let payout_account = parse_account(&w.payout_account)?;
            let (target, _) = heights(&db, min_confirmations)?;
            let item_value = ironwood_note(&db, item_note, target)?.note.value().inner();
            let fee = zsettle_core::zip317_fee(2);
            // A payout note that covers the fee exactly or leaves change above dust.
            let mut exclude = Vec::new();
            let (payout_note, payout_value) = loop {
                let Some(n) = largest_spendable_note(&db, payout_account, target, &exclude)? else {
                    return Err(anyhow!(
                        "no payout note can pay the {fee} zat fee: it needs exactly {fee} zat or more than {} zat",
                        fee + ZIP317_MARGINAL_FEE
                    ));
                };
                let v = ironwood_note(&db, n, target)?.note.value().inner();
                if v == fee || v > fee + ZIP317_MARGINAL_FEE {
                    break (n, v);
                }
                exclude.push(n);
            };
            let mut outputs = vec![(item_account, item_value)];
            if payout_value > fee {
                outputs.push((payout_account, payout_value - fee));
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
            let outcome = process(&db, &w, wallet.network, &l, &pczt, &anchors)?;
            println!("{}", describe(&outcome.result));
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
            };
            if let Some(addr) = listen {
                let http = tiny_http::Server::http(&addr)
                    .map_err(|e| anyhow!("listening on {addr}: {e}"))?;
                println!(
                    "{}",
                    serde_json::json!({ "event": "listening", "address": addr })
                );
                let (inbox, outbox) = (inbox.clone(), outbox.clone());
                thread::spawn(move || serve_http(http, &inbox, &outbox));
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
            // Regular files only: a symlink to a FIFO or a device would stall the pass.
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .filter(|e| e.path().extension().is_some_and(|x| x == "pczt"))
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
            move_into(path, &self.inbox.join("done"))?;
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
        let dest = move_into(path, &self.inbox.join("rejected"))?;
        let mut why = dest.into_os_string();
        why.push(".reason.txt");
        fs::write(why, reason)?;
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
        pczt_ops::verify_proof(&pczt, self.vk)
            .map_err(|e| Refusal::Reject(format!("refused: {e:#}")))?;
        let expiry = *pczt.global().expiry_height();
        let outcome = match process(db, self.w, self.network, l, &pczt, anchors) {
            Ok(outcome) => outcome,
            // A failure of this pass rather than of the envelope: keep it while its
            // transaction can still be mined.
            Err(e) if expiry != 0 => return Err(Refusal::Defer(format!("error: {e:#}"))),
            Err(e) => return Err(Refusal::Reject(format!("error: {e:#}"))),
        };
        if outcome.item.is_none() && outcome.unavailable > 0 && expiry > outcome.tip {
            return Err(Refusal::Defer(format!(
                "spends no listed item this wallet can spend, and {} listed items are not spendable here yet (unscanned or too recent)",
                outcome.unavailable
            )));
        }
        let approved = match outcome.result {
            Ok(approved) => approved,
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
        };
        let item = outcome.item.ok_or_else(|| {
            Refusal::Reject("error: approved trade without a listed item".to_string())
        })?;
        let signed = pczt_ops::sign_ironwood_action(pczt, approved.item_action_index, self.ask)
            .map_err(|e| Refusal::Reject(format!("signing failed: {e:#}")))?;
        let bytes = pczt_ops::to_bytes(&signed)
            .map_err(|e| Refusal::Reject(format!("signing failed: {e:#}")))?;
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
    };
    let result = check(pczt, &nf, &seller_nfs, &ivk, &policy, anchors);
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
/// The HTTP face of the service: offers in, replies out. An envelope lands in the inbox under
/// its content tag, so the directory loop handles it exactly like a dropped file, and each
/// buyer can fetch only the reply to its own envelope.
fn serve_http(server: tiny_http::Server, inbox: &Path, outbox: &Path) {
    use std::io::Read;
    use tiny_http::{Header, Method, Response};
    for mut request in server.incoming_requests() {
        let url = request.url().to_string();
        let response = match (request.method(), url.as_str()) {
            (Method::Post, "/offer") => {
                let declared = request.body_length().unwrap_or(0) as u64;
                let pending = fs::read_dir(inbox)
                    .map(|d| {
                        d.filter_map(|e| e.ok())
                            .filter(|e| e.path().extension().is_some_and(|x| x == "pczt"))
                            .count()
                    })
                    .unwrap_or(0);
                let mut body = Vec::new();
                let read = if pending >= MAX_PENDING_OFFERS {
                    let _ = request.respond(
                        Response::from_string("too many offers waiting; try again later")
                            .with_status_code(503)
                            .with_header(server_header()),
                    );
                    continue;
                } else if declared > MAX_ENVELOPE_BYTES {
                    Err(())
                } else {
                    request
                        .as_reader()
                        .take(MAX_ENVELOPE_BYTES + 1)
                        .read_to_end(&mut body)
                        .map_err(|_| ())
                };
                match read {
                    Ok(_) if !body.is_empty() && body.len() as u64 <= MAX_ENVELOPE_BYTES => {
                        let tag = pczt_ops::envelope_tag(&body);
                        let path = inbox.join(format!("{tag}.pczt"));
                        let stored = if path.exists() {
                            Ok(())
                        } else {
                            write_atomic(&path, &body)
                        };
                        match stored {
                            Ok(()) => Response::from_string(
                                serde_json::json!({ "tag": tag, "reply": format!("/reply/{tag}") })
                                    .to_string(),
                            )
                            .with_status_code(200),
                            Err(e) => {
                                Response::from_string(format!("cannot store the offer: {e:#}"))
                                    .with_status_code(500)
                            }
                        }
                    }
                    _ => Response::from_string("the envelope must be between 1 byte and 1 MiB")
                        .with_status_code(413),
                }
            }
            (Method::Get, path) if path.starts_with("/reply/") => {
                let tag = &path["/reply/".len()..];
                if tag.len() != 32 || !tag.bytes().all(|b| b.is_ascii_hexdigit()) {
                    Response::from_string("bad tag").with_status_code(400)
                } else {
                    let signed = outbox.join(format!("{tag}.{tag}.signed.pczt"));
                    let reason = inbox
                        .join("rejected")
                        .join(format!("{tag}.pczt.reason.txt"));
                    if let Ok(bytes) = fs::read(&signed) {
                        Response::from_data(bytes)
                            .with_status_code(200)
                            .with_header(
                                Header::from_bytes("Content-Type", "application/octet-stream")
                                    .expect("header"),
                            )
                    } else if let Ok(why) = fs::read_to_string(&reason) {
                        Response::from_string(why).with_status_code(410)
                    } else {
                        Response::from_string("pending").with_status_code(202)
                    }
                }
            }
            _ => Response::from_string("zsettle seller: POST /offer, GET /reply/<tag>")
                .with_status_code(404),
        };
        let _ = request.respond(response.with_header(server_header()));
    }
}

/// A neutral `Server` header instead of the library's name and version.
fn server_header() -> tiny_http::Header {
    tiny_http::Header::from_bytes("Server", "zsettle").expect("header")
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
        dest = dir.join(format!("{name}.{n}"));
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
