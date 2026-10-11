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
    locking::LockFilter,
    wallet::{
        create_proposed_transactions,
        input_selection::{GreedyInputSelector, SpendPolicy},
        propose_transfer, ConfirmationsPolicy, SpendingKeys, TargetHeight,
    },
    InputSource, MaxSpendMode, TargetValue, WalletRead,
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
    ShieldedPool,
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

/// Unspent Ironwood notes of an account, largest first.
pub fn unspent_notes(
    db: &Db,
    account: AccountUuid,
    target_height: TargetHeight,
) -> Result<Vec<ItemNote>> {
    let notes = db
        .select_spendable_notes(
            account,
            TargetValue::AllFunds(MaxSpendMode::MaxSpendable),
            &[ShieldedPool::Ironwood],
            target_height,
            ConfirmationsPolicy::default(),
            &[],
            LockFilter::Unfiltered,
        )
        .map_err(|e| anyhow!("{e}"))?;
    let mut out: Vec<ItemNote> = notes
        .ironwood()
        .iter()
        .map(|n| ItemNote {
            note: NoteRef {
                txid: *n.txid(),
                index: u32::from(n.output_index()),
            },
            value_zat: n.note().value().inner(),
            mined_height: n.mined_height().map(u32::from),
        })
        .collect();
    out.sort_by(|a, b| b.value_zat.cmp(&a.value_zat));
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
