//! `zsettle`: buyer-side and anyone-side command line.

use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use orchard::keys::SpendAuthorizingKey;
use zcash_protocol::consensus::Network;
use zsettle_core::broadcast::{
    check_consensus_at_tip_blocking, check_consensus_blocking, pick_server,
};
use zsettle_core::listing::Listings;
use zsettle_core::mint::{note_state, NoteState};
use zsettle_core::pczt as pczt_ops;
use zsettle_core::trade::{build_self_spend, build_trade, TradeRequest};
use zsettle_core::verify::Consignment;
use zsettle_core::wallet::{
    account_orchard_fvk, account_ufvk, find_account_by_ufvk, heights, import_viewing_key,
    init_wallet, largest_spendable_note, list_accounts, parse_account, release_note, reservations,
    reserve_note, reserved_notes, sync_blocking, NoteRef, Wallet,
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
// Command structs are built once and matched once; their sizes do not matter.
#[allow(clippy::large_enum_variant)]
enum Cmd {
    /// Show the items in a listings file and whether its signature checks out.
    List {
        /// Listings file published by a seller.
        #[arg(long)]
        listings: PathBuf,
    },
    /// Create a wallet: a new mnemonic encrypted to an age identity, the database, and the
    /// accounts `buyer` (index 0, pays and receives payments) and `item` (index 1, holds
    /// items for resale). Prints the account UUIDs and an address to fund.
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
        /// lightwalletd server URL (defaults to the network's).
        #[arg(long)]
        server: Option<String>,
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
        identity: Option<PathBuf>,
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
        identity: Option<PathBuf>,
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
        /// Account that receives the item. Default: the wallet's `item` account, as `init`
        /// makes it, so that items stay apart from payments; the paying account if there is
        /// none.
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
        /// Where to write the redacted copy for the seller: drop it in the seller's inbox, or
        /// let `--seller-url` post it. Default: `<out>.seller.pczt`.
        #[arg(long)]
        seller_copy: Option<PathBuf>,
        /// Seller service URL (http://host:port). The redacted copy is also posted to
        /// `<url>/offer`, and the reply URL to pass to `finish --signed` is printed.
        #[arg(long)]
        seller_url: Option<String>,
    },
    /// Combine the seller's signed copy with the full PCZT, sign the payment action, extract the transaction.
    Finish {
        /// Buyer wallet directory.
        #[arg(long)]
        wallet: PathBuf,
        /// age identity file that decrypts the wallet's mnemonic. Omit it for a
        /// passphrase-protected wallet: the passphrase is asked for, or taken from
        /// ZSETTLE_PASSPHRASE.
        #[arg(long)]
        identity: Option<PathBuf>,
        /// ZIP 32 account index of the buyer's spending account.
        #[arg(long, default_value_t = 0)]
        account_index: u32,
        /// The full PCZT written by `buy`.
        #[arg(long)]
        full: PathBuf,
        /// The seller's signed copy: a file, or the reply URL `buy` printed, which is polled
        /// until the seller has answered.
        #[arg(long)]
        signed: String,
        /// How long to wait for a reply URL, in seconds.
        #[arg(long, default_value_t = 300)]
        wait: u64,
        /// Where to write the final transaction (raw bytes); the final PCZT goes to `<out>.pczt`.
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
    /// Provenance check of an item with viewing keys only: trace it from its genesis note with the listings' keys.
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
        /// Do not ask the server where the genesis transaction is (that reveals which item you
        /// look at): scan from the listings' birthdays instead, and skip the item name.
        #[arg(long)]
        private: bool,
    },
    /// Retire the paying notes of offers that did not settle: each one is moved to a fresh
    /// address of its account in a plain two-action transaction, so the nullifier the seller
    /// learned from the envelope can never point at a later payment of yours.
    Rotate {
        /// Buyer wallet directory.
        #[arg(long)]
        wallet: PathBuf,
        /// UUID of the paying account the reservations belong to.
        #[arg(long)]
        account: String,
        /// ZIP 32 account index of that account.
        #[arg(long, default_value_t = 0)]
        account_index: u32,
        /// age identity file that decrypts the wallet's mnemonic. Omit it for a
        /// passphrase-protected wallet: the passphrase is asked for, or taken from
        /// ZSETTLE_PASSPHRASE.
        #[arg(long)]
        identity: Option<PathBuf>,
        /// Confirmations required on the note.
        #[arg(long, default_value_t = 3)]
        min_confirmations: u32,
        /// lightwalletd server URL (defaults to the wallet's network).
        #[arg(long)]
        server: Option<String>,
        /// Report what would be done without building or sending anything.
        #[arg(long)]
        dry_run: bool,
        /// Skip the sync before deciding (the wallet must be fresh).
        #[arg(long)]
        no_sync: bool,
    },
    /// Merge every spendable note of an account into one note at a fresh address, so that one
    /// note can pay for a trade. Notes reserved for open offers are left alone.
    Consolidate {
        /// Wallet directory.
        #[arg(long)]
        wallet: PathBuf,
        /// UUID of the account.
        #[arg(long)]
        account: String,
        /// ZIP 32 account index of that account.
        #[arg(long, default_value_t = 0)]
        account_index: u32,
        /// age identity file that decrypts the wallet's mnemonic. Omit it for a
        /// passphrase-protected wallet: the passphrase is asked for, or taken from
        /// ZSETTLE_PASSPHRASE.
        #[arg(long)]
        identity: Option<PathBuf>,
        /// Confirmations required on the notes.
        #[arg(long, default_value_t = 3)]
        min_confirmations: u32,
        /// lightwalletd server URL (defaults to the network's).
        #[arg(long)]
        server: Option<String>,
        /// Report the plan without building or sending anything.
        #[arg(long)]
        dry_run: bool,
        /// Skip the sync first (the wallet must be fresh).
        #[arg(long)]
        no_sync: bool,
    },
    /// List the unspent Ironwood notes of an account of a wallet, dust included.
    Notes {
        /// Wallet directory.
        #[arg(long)]
        wallet: PathBuf,
        /// Account UUID.
        #[arg(long)]
        account: String,
    },
}

/// A listings file older than this many blocks earns a warning at `buy`.
const STALE_LISTING_BLOCKS: u32 = 1_000;

fn shorten(s: &str) -> String {
    if s.chars().count() > 20 {
        let head: String = s.chars().take(12).collect();
        let tail: String = s
            .chars()
            .rev()
            .take(6)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("{head}…{tail}")
    } else {
        s.to_string()
    }
}

fn parse_network(name: &str) -> Result<Network> {
    match name {
        "main" => Ok(Network::MainNetwork),
        "test" => Ok(Network::TestNetwork),
        other => Err(anyhow!("unknown network {other}")),
    }
}

/// Refuse to send when the server is on a consensus branch this build does not sign for.
/// An HTTP client that follows no redirects: an offer must reach the service itself.
fn http_agent() -> ureq::Agent {
    ureq::builder()
        .redirects(0)
        .timeout(std::time::Duration::from_secs(30))
        .build()
}

/// Post the redacted copy to the seller service; returns the URL its reply will appear at.
fn post_offer(seller_url: &str, envelope: &[u8]) -> Result<String> {
    let base = seller_url.trim_end_matches('/');
    let resp = http_agent()
        .post(&format!("{base}/offer"))
        .set("Content-Type", "application/octet-stream")
        .send_bytes(envelope)
        .map_err(|e| anyhow!("posting the offer to {base}: {e}"))?;
    if resp.status() != 200 {
        return Err(anyhow!(
            "the seller answered {} to the offer; redirects are not followed, use the service's final URL",
            resp.status()
        ));
    }
    let text = resp.into_string().context("the seller's answer")?;
    let answer: serde_json::Value = serde_json::from_str(&text).context("the seller's answer")?;
    let tag = answer["tag"]
        .as_str()
        .ok_or_else(|| anyhow!("the seller's answer names no tag: {text}"))?;
    if tag != pczt_ops::envelope_tag(envelope) {
        return Err(anyhow!(
            "the seller tagged the offer differently; it did not receive it as sent"
        ));
    }
    Ok(format!("{base}/reply/{tag}"))
}

/// Poll the seller's reply URL until the signed copy is there, the offer was refused, or
/// the wait runs out.
fn fetch_reply(url: &str, wait_secs: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(wait_secs);
    loop {
        match http_agent().get(url).call() {
            Ok(resp) if resp.status() == 200 => {
                let mut bytes = Vec::new();
                resp.into_reader()
                    .take(zsettle_core::MAX_ENVELOPE_BYTES + 1)
                    .read_to_end(&mut bytes)?;
                return Ok(bytes);
            }
            Ok(resp) if resp.status() == 202 => {}
            Ok(resp) => return Err(anyhow!("unexpected answer {} from {url}", resp.status())),
            Err(ureq::Error::Status(410, resp)) => {
                return Err(anyhow!(
                    "the seller refused the offer: {}",
                    resp.into_string().unwrap_or_default()
                ))
            }
            Err(e) => return Err(anyhow!("fetching {url}: {e}")),
        }
        if std::time::Instant::now() >= deadline {
            return Err(anyhow!(
                "no answer from the seller within {wait_secs} s; run finish again later with the same --signed URL"
            ));
        }
        eprintln!("waiting for the seller's signature...");
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
}

/// Zatoshis as ZEC, for people.
fn zec(zat: u64) -> String {
    format!("{} ZEC", zat as f64 / 1e8)
}

fn check_server_before_sending(server: &str, network: Network) -> Result<()> {
    check_consensus_at_tip_blocking(server, network)?;
    Ok(())
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
        Cmd::List { listings } => {
            let l = Listings::read(&listings)?;
            let signed = match l.verify_signature() {
                Ok(()) => "signed by the item account's key".to_string(),
                Err(e) => format!("NOT TRUSTWORTHY: {e}"),
            };
            println!(
                "network: {}  payout: {}  key: {}  issued at: {}  {signed}",
                l.network,
                shorten(&l.payout_address),
                l.key_fingerprint(),
                l.issued_at
            );
            for i in &l.items {
                println!(
                    "{}  {:>10} zat ({}) expiry<= {:>3} blocks  {}",
                    i.note,
                    i.price_zat,
                    zec(i.price_zat),
                    i.max_expiry_delta,
                    i.name
                );
            }
            Ok(())
        }
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
                &["buyer", "item"],
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
                    "next": "send ZEC to `fund`, then `zsettle sync`; `--buyer-account` is the first UUID, `--receive-account` the second",
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
            let w = Wallet::open(&wallet)?;
            let server = pick_server(server, w.network);
            let mut db = w.db()?;
            let report = sync_blocking(&w, &mut db, &server)?;
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
            let seed = w.seed(identity.as_deref())?;
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
            seller_url,
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
            // Never longer than the ecosystem default: a shorter limit from the listing is
            // honoured, a longer one would mark the trade on chain.
            let expiry = expiry_delta.unwrap_or(
                listed
                    .as_ref()
                    .map(|i| {
                        i.max_expiry_delta
                            .min(zsettle_core::DEFAULT_MAX_EXPIRY_DELTA)
                    })
                    .unwrap_or(zsettle_core::DEFAULT_MAX_EXPIRY_DELTA),
            );
            if expiry == 0 || expiry > zsettle_core::DEFAULT_MAX_EXPIRY_DELTA {
                return Err(anyhow!(
                    "--expiry-delta must be between 1 and {} blocks: a longer expiry would mark the trade on chain, and the seller refuses it anyway",
                    zsettle_core::DEFAULT_MAX_EXPIRY_DELTA
                ));
            }
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
            let receive_account = match receive_account.as_deref().map(parse_account).transpose()? {
                Some(account) => Some(account),
                // The wallet's own item account, as `init` makes it, keeps items apart from
                // payments; the paying account only when there is none.
                None => {
                    let item = list_accounts(&db)?
                        .into_iter()
                        .find(|a| a.name == "item" && a.index.is_some())
                        .map(|a| parse_account(&a.account))
                        .transpose()?
                        .filter(|a| *a != buyer_account);
                    match item {
                        Some(account) => {
                            eprintln!(
                                "note: the item will be received by the wallet's item account {}; pass --receive-account to choose another",
                                account.expose_uuid()
                            );
                            Some(account)
                        }
                        None => {
                            eprintln!(
                                "note: the item will be received by the paying account; pass --receive-account <item account> if you may resell it, so that publishing that account's key later reveals nothing about your payments"
                            );
                            None
                        }
                    }
                }
            };
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
                        anyhow!("no spendable note in the buyer account: notes received from others need 10 confirmations and change 3, and notes reserved by an unsettled trade are skipped")
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
            if let Some(l) = &l {
                if l.issued_at > 0
                    && target_height > l.issued_at.saturating_add(STALE_LISTING_BLOCKS)
                {
                    eprintln!(
                        "warning: the listings file was signed {} blocks ago; check with the seller that the price is current",
                        target_height - l.issued_at
                    );
                }
            }
            eprintln!(
                "note: the seller learns the nullifier of the paying note; if this offer does not settle, spend that note to yourself before using it again"
            );
            let built = build_trade(&mut db, w.network, &req)?;
            fs::write(&out, pczt_ops::to_bytes(&built.pczt)?)
                .with_context(|| format!("writing {}", out.display()))?;
            let redacted =
                pczt_ops::redact_for_seller(built.pczt.clone(), built.item_action_index)?;
            let envelope = pczt_ops::to_bytes(&redacted)?;
            let seller_copy = seller_copy.unwrap_or_else(|| {
                let mut p = out.clone().into_os_string();
                p.push(".seller.pczt");
                PathBuf::from(p)
            });
            fs::write(&seller_copy, &envelope)
                .with_context(|| format!("writing {}", seller_copy.display()))?;
            reserve_note(&w, buyer_note, built.expiry_height)?;
            let stem = seller_copy
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("trade");
            let expect_signed = format!("{stem}.{}.signed.pczt", pczt_ops::envelope_tag(&envelope));
            let reply_url = match &seller_url {
                Some(url) => match post_offer(url, &envelope) {
                    Ok(reply) => Some(reply),
                    Err(e) => {
                        eprintln!(
                            "warning: the offer is written to {} but was not posted: {e:#}; drop the file in the seller's inbox, or post it again",
                            seller_copy.display()
                        );
                        None
                    }
                },
                None => None,
            };
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
                    "reply_url": reply_url,
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
            wait,
        } => {
            let w = Wallet::open(&wallet)?;
            let server = pick_server(server, w.network);
            let full_pczt = pczt_ops::from_bytes(&fs::read(&full)?)?;
            let signed_bytes = if signed.starts_with("http://") || signed.starts_with("https://") {
                fetch_reply(&signed, wait)?
            } else {
                fs::read(&signed).with_context(|| format!("reading {signed}"))?
            };
            let signed_pczt = pczt_ops::from_bytes(&signed_bytes)?;
            let combined = pczt_ops::combine(full_pczt, signed_pczt)?;
            let seed = w.seed(identity.as_deref())?;
            let usk = w.spending_key(&seed, account_index)?;
            let ask = SpendAuthorizingKey::from(usk.orchard());
            let (pczt, signed_n) = pczt_ops::sign_matching_actions(combined, &ask)?;
            if signed_n == 0 {
                return Err(anyhow!(
                    "the key derived from --account-index {account_index} signs none of the actions; pass the index of the account that paid"
                ));
            }
            let final_pczt_path = PathBuf::from(format!("{}.pczt", out.display()));
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
        Cmd::Rotate {
            wallet,
            account,
            account_index,
            identity,
            min_confirmations,
            server,
            dry_run,
            no_sync,
        } => {
            let w = Wallet::open(&wallet)?;
            let mut db = w.db()?;
            let server = pick_server(server, w.network);
            if !no_sync {
                sync_blocking(&w, &mut db, &server)?;
            }
            let account = parse_account(&account)?;
            let (target, _) = heights(&db, 1)?;
            let tip =
                u32::from(zcash_protocol::consensus::BlockHeight::from(target)).saturating_sub(1);
            let ask = if dry_run {
                None
            } else {
                let seed = w.seed(identity.as_deref())?;
                let usk = w.spending_key(&seed, account_index)?;
                if orchard::keys::FullViewingKey::from(usk.orchard())
                    != account_orchard_fvk(&db, account)?
                {
                    return Err(anyhow!(
                        "account index {account_index} does not derive account {}",
                        account.expose_uuid()
                    ));
                }
                Some(SpendAuthorizingKey::from(usk.orchard()))
            };
            let mut rotated = 0usize;
            for (note, expiry) in reservations(&w)? {
                let state = note_state(&w, &db, note)?;
                let action = match state {
                    NoteState::Spent { .. } => {
                        release_note(&w, note)?;
                        "released: already spent"
                    }
                    NoteState::Unknown => "kept: not in this wallet",
                    // Mineable while the next block (tip + 1) is at or below the expiry.
                    NoteState::Unspent { .. } if expiry > tip => {
                        "kept: its offer can still be mined"
                    }
                    NoteState::Unspent { .. } => match &ask {
                        None => "would rotate",
                        Some(ask) => {
                            check_server_before_sending(&server, w.network)?;
                            let built = build_self_spend(
                                &mut db,
                                w.network,
                                account,
                                note,
                                min_confirmations,
                            )?;
                            let (pczt, signed_n) =
                                pczt_ops::sign_matching_actions(built.pczt, ask)?;
                            if signed_n == 0 {
                                return Err(anyhow!("the key signs no action of the rotation"));
                            }
                            let tx = pczt_ops::extract(pczt)?;
                            let bytes = pczt_ops::tx_bytes(&tx)?;
                            zsettle_core::broadcast::send_raw_transaction_blocking(&server, bytes)?;
                            reserve_note(&w, note, built.expiry_height)?;
                            rotated += 1;
                            println!(
                                "{}",
                                serde_json::json!({ "note": note.to_string(), "rotated_in": tx.txid().to_string(), "value_out": built.value_out, "fee": built.fee })
                            );
                            continue;
                        }
                    },
                };
                println!(
                    "{}",
                    serde_json::json!({ "note": note.to_string(), "offer_expiry": expiry, "action": action })
                );
            }
            println!(
                "{}",
                serde_json::json!({ "tip": tip, "rotated": rotated, "dry_run": dry_run })
            );
            Ok(())
        }
        Cmd::Consolidate {
            wallet,
            account,
            account_index,
            identity,
            min_confirmations,
            server,
            dry_run,
            no_sync,
        } => {
            let w = Wallet::open(&wallet)?;
            let mut db = w.db()?;
            let server = pick_server(server, w.network);
            if !no_sync {
                sync_blocking(&w, &mut db, &server)?;
            }
            let account = parse_account(&account)?;
            let (target, _) = heights(&db, min_confirmations)?;
            // Every spendable note that is not reserved for an open offer, largest first.
            let next_block = u32::from(zcash_protocol::consensus::BlockHeight::from(target));
            let reserved = reserved_notes(&w, next_block)?;
            let mut seen = reserved.clone();
            let mut notes = Vec::new();
            while let Some(n) = largest_spendable_note(&db, account, target, &seen)? {
                seen.push(n);
                notes.push(n);
            }
            // Item notes (memo "zsettle item: …") in this account are kept, whatever their value.
            let memos: std::collections::HashMap<String, Option<String>> =
                zsettle_core::mint::unspent_notes(&w, &db, account)?
                    .into_iter()
                    .map(|n| (n.note.to_string(), n.memo))
                    .collect();
            notes.retain(|n| {
                !memos
                    .get(&n.to_string())
                    .and_then(|m| m.as_deref())
                    .is_some_and(|m| m.starts_with("zsettle item"))
            });
            let mut total = 0u64;
            for n in &notes {
                total += zsettle_core::wallet::ironwood_note(&db, *n, target)?
                    .note
                    .value()
                    .inner();
            }
            let fee = zsettle_core::zip317_fee(notes.len() as u64);
            if notes.len() < 2 {
                println!(
                    "{}",
                    serde_json::json!({ "notes": notes.len(), "reserved": reserved.len(), "action": "nothing to merge" })
                );
                return Ok(());
            }
            if total <= fee + zsettle_core::ZIP317_MARGINAL_FEE {
                return Err(anyhow!(
                    "the {} notes are worth {total} zat together, too little to merge for a {fee} zat fee",
                    notes.len()
                ));
            }
            if dry_run {
                println!(
                    "{}",
                    serde_json::json!({ "notes": notes.iter().map(|n| n.to_string()).collect::<Vec<_>>(), "value_in": total, "fee": fee, "value_out": total - fee, "dry_run": true })
                );
                return Ok(());
            }
            let seed = w.seed(identity.as_deref())?;
            let usk = w.spending_key(&seed, account_index)?;
            if orchard::keys::FullViewingKey::from(usk.orchard())
                != account_orchard_fvk(&db, account)?
            {
                return Err(anyhow!(
                    "account index {account_index} does not derive account {}",
                    account.expose_uuid()
                ));
            }
            let ask = SpendAuthorizingKey::from(usk.orchard());
            check_server_before_sending(&server, w.network)?;
            let inputs: Vec<_> = notes.iter().map(|n| (account, *n)).collect();
            let built = zsettle_core::trade::build_transfer(
                &mut db,
                w.network,
                &inputs,
                &[zsettle_core::trade::Output {
                    account,
                    value: total - fee,
                    internal: true,
                }],
                min_confirmations,
                false,
            )?;
            let (pczt, signed_n) = pczt_ops::sign_matching_actions(built.pczt, &ask)?;
            if signed_n != notes.len() {
                return Err(anyhow!(
                    "the key signed {signed_n} of {} spends",
                    notes.len()
                ));
            }
            let tx = pczt_ops::extract(pczt)?;
            let bytes = pczt_ops::tx_bytes(&tx)?;
            zsettle_core::broadcast::send_raw_transaction_blocking(&server, bytes)?;
            for n in &notes {
                reserve_note(&w, *n, built.expiry_height)?;
            }
            println!(
                "{}",
                serde_json::json!({ "merged": notes.len(), "value_out": built.value_out, "fee": built.fee, "txid": tx.txid().to_string(), "expiry_height": built.expiry_height })
            );
            Ok(())
        }
        Cmd::Notes { wallet, account } => {
            let w = Wallet::open(&wallet)?;
            let db = w.db()?;
            let notes = zsettle_core::mint::unspent_notes(&w, &db, parse_account(&account)?)?;
            for n in &notes {
                println!(
                    "{}  {:>10} zat  mined {}",
                    n.note,
                    n.value_zat,
                    n.mined_height
                        .map(|h| h.to_string())
                        .unwrap_or_else(|| "pending".into())
                );
            }
            if notes.is_empty() {
                println!("(no unspent notes)");
            }
            Ok(())
        }
        Cmd::Verify {
            listings,
            item,
            server,
            to_height,
            private,
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
                private,
            )?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
    }
}
