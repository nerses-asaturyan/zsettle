//! Minting items and listing them.
//!
//! An item is minted by paying a small shielded amount from the seller's payout account to
//! the item account, with a memo naming the item. The resulting note is the item's genesis
//! note; its commitment is the item's identity.

use std::{num::NonZeroUsize, str::FromStr};

use anyhow::{anyhow, Result};
use secrecy::SecretVec;
use zcash_address::ZcashAddress;
use zcash_client_backend::data_api::{
    wallet::{
        create_proposed_transactions,
        input_selection::{GreedyInputSelector, SpendPolicy},
        propose_transfer, ConfirmationsPolicy, SpendingKeys,
    },
    WalletRead,
};
use zcash_client_backend::fees::{
    standard::MultiOutputChangeStrategy, DustOutputPolicy, SplitPolicy, StandardFeeRule,
};
use zcash_client_backend::wallet::OvkPolicy;
use zcash_client_sqlite::{util::SystemClock, AccountUuid};
use zcash_keys::address::UnifiedAddress;
use zcash_proofs::prover::LocalTxProver;
use zcash_protocol::{
    consensus::Network,
    memo::{Memo, MemoBytes},
    value::Zatoshis,
    ShieldedPool, TxId,
};
use zip321::{Payment, TransactionRequest};

use crate::wallet::{next_orchard_address, Db, NoteRef, Wallet};

/// An unspent note of the item account.
#[derive(Clone, Debug)]
pub struct ItemNote {
    /// Where the note lives.
    pub note: NoteRef,
    /// Value in zatoshis.
    pub value_zat: u64,
    /// Height the note was mined at, if known.
    pub mined_height: Option<u32>,
}

/// SQL condition that a spend recorded in transaction `tx` still counts: the transaction is
/// mined, never expires, or is unexpired at `target` (an SQL parameter). A spend whose expiry
/// the wallet does not know, as happens for one seen only in a compact block, counts for the
/// default expiry delta after it was first observed, the way the wallet library itself bounds
/// it. Without that bound a spend undone by a reorg would hide the note for good.
fn live_spend(tx: &str, target: &str) -> String {
    format!(
        "({tx}.mined_height IS NOT NULL
          OR {tx}.expiry_height = 0
          OR {tx}.expiry_height >= {target}
          OR ({tx}.expiry_height IS NULL
              AND {tx}.min_observed_height + {delta} >= {target}))",
        delta = crate::DEFAULT_MAX_EXPIRY_DELTA
    )
}

/// Every unspent Ironwood note of an account, dust included, oldest first and unmined notes
/// last. The wallet's own note selection ignores notes at or below the ZIP 317 marginal fee,
/// which is exactly where item notes live, so this reads the wallet's tables directly.
pub fn unspent_notes(wallet: &Wallet, db: &Db, account: AccountUuid) -> Result<Vec<ItemNote>> {
    // The next block to be scanned: the height a spend must still be valid at.
    let target = crate::wallet::max_scanned_height(db)?.unwrap_or(0) + 1;
    let conn = open_read_only(wallet)?;
    let mut stmt = conn
        .prepare(&format!(
            "SELECT t.txid, n.action_index, n.value, t.mined_height
             FROM ironwood_received_notes n
             JOIN transactions t ON t.id_tx = n.transaction_id
             JOIN accounts a ON a.id = n.account_id
             WHERE a.uuid = ?1
               AND NOT EXISTS (
                 SELECT 1 FROM ironwood_received_note_spends s
                 JOIN transactions st ON st.id_tx = s.transaction_id
                 WHERE s.ironwood_received_note_id = n.id
                   AND {live})
             ORDER BY t.mined_height IS NULL, t.mined_height, n.action_index",
            live = live_spend("st", "?2")
        ))
        .map_err(|e| anyhow!("{e}"))?;
    let rows = stmt
        .query_map(
            rusqlite::params![account.expose_uuid().as_bytes().as_slice(), target],
            |row| {
                let txid: Vec<u8> = row.get(0)?;
                let index: u32 = row.get(1)?;
                let value: i64 = row.get(2)?;
                let mined: Option<u32> = row.get(3)?;
                Ok((txid, index, value, mined))
            },
        )
        .map_err(|e| anyhow!("{e}"))?;
    let mut out = Vec::new();
    for row in rows {
        let (txid, index, value, mined_height) = row.map_err(|e| anyhow!("{e}"))?;
        let txid: [u8; 32] = txid
            .try_into()
            .map_err(|_| anyhow!("bad txid in the wallet database"))?;
        out.push(ItemNote {
            note: NoteRef {
                txid: TxId::from_bytes(txid),
                index,
            },
            value_zat: u64::try_from(value).unwrap_or(0),
            mined_height,
        });
    }
    Ok(out)
}

/// Whether a note of this wallet is unspent, spent, or unknown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoteState {
    /// The wallet has never seen this note.
    Unknown,
    /// Received and not spent; `mined_height` is `None` while the creating transaction is unmined.
    Unspent {
        /// Height of the transaction that created the note.
        mined_height: Option<u32>,
    },
    /// Spent by the given transaction (mined, or created by this wallet and not yet expired).
    Spent {
        /// Transaction id of the spend.
        txid: String,
    },
}

fn open_read_only(wallet: &Wallet) -> Result<rusqlite::Connection> {
    rusqlite::Connection::open_with_flags(
        wallet.dir.join("data.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| anyhow!("opening wallet database: {e}"))
}

/// The wallet's knowledge of one Ironwood note, read from its tables. Unlike
/// [`crate::wallet::find_ironwood_note`], this distinguishes a note the wallet has recorded
/// a spend for from one it merely cannot spend right now (unmined, reorged, not yet
/// scanned).
pub fn note_state(wallet: &Wallet, db: &Db, note: NoteRef) -> Result<NoteState> {
    use rusqlite::OptionalExtension;
    let target = crate::wallet::max_scanned_height(db)?.unwrap_or(0) + 1;
    let conn = open_read_only(wallet)?;
    let row = conn
        .query_row(
            &format!(
                "SELECT t.mined_height, (
                SELECT st.txid FROM ironwood_received_note_spends s
                JOIN transactions st ON st.id_tx = s.transaction_id
                WHERE s.ironwood_received_note_id = n.id
                  AND {live}
                LIMIT 1)
             FROM ironwood_received_notes n
             JOIN transactions t ON t.id_tx = n.transaction_id
             WHERE t.txid = ?1 AND n.action_index = ?2",
                live = live_spend("st", "?3")
            ),
            rusqlite::params![note.txid.as_ref().to_vec(), note.index, target],
            |row| {
                let mined: Option<u32> = row.get(0)?;
                let spent: Option<Vec<u8>> = row.get(1)?;
                Ok((mined, spent))
            },
        )
        .optional()
        .map_err(|e| anyhow!("{e}"))?;
    Ok(match row {
        None => NoteState::Unknown,
        Some((mined_height, None)) => NoteState::Unspent { mined_height },
        Some((_, Some(spender))) => {
            let bytes: [u8; 32] = spender
                .try_into()
                .map_err(|_| anyhow!("bad txid in the wallet database"))?;
            NoteState::Spent {
                txid: TxId::from_bytes(bytes).to_string(),
            }
        }
    })
}

/// A fresh receiving address of the item account, as a unified address string with only
/// an Orchard receiver. Each mint gets its own diversifier.
pub fn item_address(db: &mut Db, network: Network, item_account: AccountUuid) -> Result<String> {
    let addr = next_orchard_address(db, item_account)?;
    let ua = UnifiedAddress::from_receivers(Some(addr), None, None, None, None)
        .ok_or_else(|| anyhow!("cannot build item address"))?;
    Ok(ua.encode(&network))
}

/// What to mint and which account pays for it.
#[derive(Clone, Debug)]
pub struct MintRequest {
    /// Account that pays (the seller's payout account).
    pub from_account: AccountUuid,
    /// ZIP 32 index of that account, to derive its spending key.
    pub from_account_index: u32,
    /// The item account that receives the genesis note.
    pub item_account: AccountUuid,
    /// Value of the item note in zatoshis.
    pub value_zat: u64,
    /// Item name, written into the memo.
    pub name: String,
}

/// A plain shielded payment from an account of this wallet.
#[derive(Clone, Debug)]
pub struct PayRequest {
    /// Account that pays.
    pub from_account: AccountUuid,
    /// ZIP 32 index of that account, to derive its spending key.
    pub from_account_index: u32,
    /// Recipient, as a unified address string.
    pub to: String,
    /// Amount in zatoshis.
    pub value_zat: u64,
    /// Optional text memo.
    pub memo: Option<String>,
}

/// Mint one item: pay the item account from the payout account with the name as the memo.
/// Returns the raw transaction bytes and its id; the caller broadcasts.
pub fn mint(
    wallet: &Wallet,
    db: &mut Db,
    seed: &SecretVec<u8>,
    req: &MintRequest,
) -> Result<(Vec<u8>, String)> {
    let MintRequest {
        from_account,
        from_account_index,
        item_account,
        value_zat,
        name,
    } = req.clone();
    let to = item_address(db, wallet.network, item_account)?;
    pay(
        wallet,
        db,
        seed,
        &PayRequest {
            from_account,
            from_account_index,
            to,
            value_zat,
            memo: Some(format!("zsettle item: {name}")),
        },
    )
}

/// Pay a unified address from an account of this wallet. Returns the raw transaction bytes
/// and its id; the caller broadcasts.
pub fn pay(
    wallet: &Wallet,
    db: &mut Db,
    seed: &SecretVec<u8>,
    req: &PayRequest,
) -> Result<(Vec<u8>, String)> {
    let PayRequest {
        from_account,
        from_account_index,
        to,
        value_zat,
        memo,
    } = req.clone();
    let memo = memo
        .map(|m| Memo::from_str(&m).map_err(|e| anyhow!("memo: {e}")))
        .transpose()?;
    let payment = Payment::new(
        ZcashAddress::from_str(&to).map_err(|e| anyhow!("address: {e}"))?,
        Some(Zatoshis::from_u64(value_zat).map_err(|_| anyhow!("value"))?),
        memo.as_ref().map(MemoBytes::from),
        None,
        None,
        vec![],
    )
    .map_err(|e| anyhow!("invalid payment: {e:?}"))?;
    let request =
        TransactionRequest::new(vec![payment]).map_err(|e| anyhow!("payment request: {e}"))?;

    let prover = LocalTxProver::bundled();
    let change_strategy = MultiOutputChangeStrategy::new(
        StandardFeeRule::Zip317,
        None,
        ShieldedPool::Ironwood,
        DustOutputPolicy::default(),
        SplitPolicy::with_min_output_value(
            NonZeroUsize::new(1).expect("nonzero"),
            Zatoshis::from_u64(10_000).expect("ok"),
        ),
    );
    let input_selector = GreedyInputSelector::new();
    let proposal =
        propose_transfer::<_, _, _, _, zcash_client_sqlite::wallet::commitment_tree::Error>(
            db,
            &wallet.network,
            from_account,
            &input_selector,
            &change_strategy,
            request,
            ConfirmationsPolicy::default(),
            &SpendPolicy::default(),
            None,
            None,
        )
        .map_err(|e| anyhow!("proposing: {e}"))?;

    let usk = wallet.spending_key(seed, from_account_index)?;
    let txids = create_proposed_transactions::<
        _,
        _,
        zcash_client_backend::data_api::wallet::input_selection::GreedyInputSelectorError,
        _,
        zcash_primitives::transaction::fees::zip317::FeeError,
        _,
    >(
        db,
        &wallet.network,
        &SystemClock,
        &mut rand::rng(),
        &prover,
        &prover,
        &SpendingKeys::from_unified_spending_key(usk),
        OvkPolicy::Sender,
        &proposal,
        None,
    )
    .map_err(|e| anyhow!("creating transaction: {e}"))?;
    let txid = *txids.first();
    let tx = db
        .get_transaction(txid)
        .map_err(|e| anyhow!("{e}"))?
        .ok_or_else(|| anyhow!("transaction {txid} not stored"))?;
    let mut bytes = Vec::new();
    tx.write(&mut bytes)?;
    Ok((bytes, txid.to_string()))
}
