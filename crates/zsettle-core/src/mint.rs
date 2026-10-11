//! Minting items and listing them.
//!
//! An item is minted by paying a small shielded amount from the seller's payout account to
//! the item account, with a memo naming the item. The resulting note is the item's genesis
//! note; its commitment is the item's identity.

use std::{num::NonZeroUsize, str::FromStr};

use anyhow::{anyhow, Result};
use orchard::keys::Scope;
use rand_core::OsRng;
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
use zcash_client_sqlite::AccountUuid;
use zcash_keys::address::UnifiedAddress;
use zcash_proofs::prover::LocalTxProver;
use zcash_protocol::{
    consensus::Network,
    memo::{Memo, MemoBytes},
    value::Zatoshis,
    ShieldedPool, TxId,
};
use zip321::{Payment, TransactionRequest};

use crate::wallet::{account_ufvk, Db, NoteRef, Wallet};

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

/// Every unspent Ironwood note of an account, dust included, oldest first and unmined notes
/// last. The wallet's own note selection ignores notes at or below the ZIP 317 marginal fee,
/// which is exactly where item notes live, so this reads the wallet's tables directly.
pub fn unspent_notes(wallet: &Wallet, db: &Db, account: AccountUuid) -> Result<Vec<ItemNote>> {
    let tip = crate::wallet::max_scanned_height(db)?.unwrap_or(0);
    let conn = rusqlite::Connection::open_with_flags(
        wallet.dir.join("data.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| anyhow!("opening wallet database: {e}"))?;
    let mut stmt = conn
        .prepare(
            "SELECT t.txid, n.action_index, n.value, t.mined_height
             FROM ironwood_received_notes n
             JOIN transactions t ON t.id_tx = n.transaction_id
             JOIN accounts a ON a.id = n.account_id
             WHERE a.uuid = ?1
               AND NOT EXISTS (
                 SELECT 1 FROM ironwood_received_note_spends s
                 JOIN transactions st ON st.id_tx = s.transaction_id
                 WHERE s.ironwood_received_note_id = n.id
                   AND (st.mined_height IS NOT NULL
                        OR st.expiry_height IS NULL
                        OR st.expiry_height = 0
                        OR st.expiry_height >= ?2))
             ORDER BY t.mined_height IS NULL, t.mined_height, n.action_index",
        )
        .map_err(|e| anyhow!("{e}"))?;
    let rows = stmt
        .query_map(
            rusqlite::params![account.expose_uuid().as_bytes().as_slice(), tip],
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

/// The item account's receiving address, as a unified address string.
pub fn item_address(db: &Db, network: Network, item_account: AccountUuid) -> Result<String> {
    let fvk = account_ufvk(db, item_account)?
        .orchard()
        .cloned()
        .ok_or_else(|| anyhow!("item account has no Orchard/Ironwood key"))?;
    let ua =
        UnifiedAddress::from_receivers(Some(fvk.address_at(0u32, Scope::External)), None, None)
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
    let name = name.as_str();
    let to = item_address(db, wallet.network, item_account)?;
    let memo =
        Memo::from_str(&format!("zsettle item: {name}")).map_err(|e| anyhow!("memo: {e}"))?;
    let payment = Payment::new(
        ZcashAddress::from_str(&to).map_err(|e| anyhow!("item address: {e}"))?,
        Some(Zatoshis::from_u64(value_zat).map_err(|_| anyhow!("value"))?),
        Some(MemoBytes::from(&memo)),
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
    let _ = OsRng;
    Ok((bytes, txid.to_string()))
}
