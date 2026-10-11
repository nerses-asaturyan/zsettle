//! Two-party bundle construction.
//!
//! Builds one Ironwood transaction from two spends under two different full viewing keys,
//! both witnessed at the same anchor:
//!
//! - action A spends the seller's item note (seller's item account, viewing key only) to a
//!   new item note owned by the buyer;
//! - action B spends the buyer's ZEC note to the seller at the listed price, with change
//!   back to the buyer when needed.
//!
//! The result is a proven PCZT (ZIP 374) that still needs two signatures.

use anyhow::{anyhow, Context, Result};
use orchard::circuit::{OrchardCircuitVersion, ProvingKey};
use orchard::keys::Scope;
use pczt::roles::{creator::Creator, io_finalizer::IoFinalizer, prover::Prover};
use pczt::Pczt;
use zcash_client_backend::data_api::WalletCommitmentTrees;
use zcash_client_sqlite::AccountUuid;
use zcash_keys::address::Address;
use zcash_primitives::transaction::builder::{BuildConfig, Builder, BundlePadding, PcztResult};
use zcash_primitives::transaction::fees::zip317::{FeeError, FeeRule};
use zcash_protocol::consensus::{BlockHeight, Network};
use zcash_protocol::memo::MemoBytes;
use zcash_protocol::value::Zatoshis;

use crate::wallet::{
    account_orchard_fvk, account_ufvk, heights, ironwood_note, next_orchard_address, Db, NoteRef,
};
use crate::{zip317_fee, DEFAULT_MAX_EXPIRY_DELTA, ZIP317_MARGINAL_FEE};

/// Everything the buyer needs to assemble a trade.
#[derive(Clone, Debug)]
pub struct TradeRequest {
    /// The buyer's own (spending) account.
    pub buyer_account: AccountUuid,
    /// The imported, view-only account holding the item note (received by consignment).
    pub item_account: AccountUuid,
    /// The item note to buy.
    pub item_note: NoteRef,
    /// The buyer's note that pays for it (must cover price + fee).
    pub buyer_note: NoteRef,
    /// Seller's unified address receiving the payment.
    pub seller_address: String,
    /// Price in zatoshis.
    pub price_zat: u64,
    /// Blocks after the target height at which the transaction expires.
    pub expiry_delta: u32,
    /// Confirmations required for the inputs / anchor.
    pub min_confirmations: u32,
    /// Memo attached to the payment output.
    pub memo: String,
    /// Memo attached to the new item note (for example the item's name).
    pub item_memo: String,
    /// Account of the buyer's wallet that receives the item; defaults to `buyer_account`.
    /// A dedicated item account lets the buyer resell later without exposing other funds.
    pub receive_account: Option<AccountUuid>,
}

/// A proven, unsigned trade.
pub struct BuiltTrade {
    /// The full PCZT (buyer keeps this; it contains the buyer's keys and witnesses).
    pub pczt: Pczt,
    /// Index of the action that spends the item note (the seller signs this one).
    pub item_action_index: usize,
    /// Index of the action that spends the buyer's note (the buyer signs this one).
    pub payment_action_index: usize,
    /// Fee paid, in zatoshis.
    pub fee: u64,
    /// Change returned to the buyer, in zatoshis.
    pub change: u64,
    /// Height the transaction targets.
    pub target_height: u32,
    /// Height at which the transaction expires.
    pub expiry_height: u32,
}

/// Build and prove the two-party trade from the buyer's wallet database.
pub fn build_trade(db: &mut Db, network: Network, req: &TradeRequest) -> Result<BuiltTrade> {
    let (target_height, anchor_height) = heights(db, req.min_confirmations)?;

    // Keys: seller's item account (viewing only) and the buyer's own account.
    let fvk_item = account_ufvk(db, req.item_account)?
        .orchard()
        .cloned()
        .ok_or_else(|| anyhow!("item account has no Orchard/Ironwood viewing key"))?;
    let fvk_buyer = account_ufvk(db, req.buyer_account)?
        .orchard()
        .cloned()
        .ok_or_else(|| anyhow!("buyer account has no Orchard/Ironwood key"))?;

    // Notes, and proof that each belongs to the account it is claimed for.
    let item = ironwood_note(db, req.item_note, target_height)?;
    if fvk_item.scope_for_address(&item.note.recipient()).is_none() {
        return Err(anyhow!("item note is not owned by --item-account"));
    }
    let payer = ironwood_note(db, req.buyer_note, target_height)?;
    if fvk_buyer
        .scope_for_address(&payer.note.recipient())
        .is_none()
    {
        return Err(anyhow!("buyer note is not owned by --buyer-account"));
    }
    let item_value = item.note.value().inner();
    let payer_value = payer.note.value().inner();
    if item_value < crate::item::MIN_ITEM_VALUE {
        return Err(anyhow!(
            "item note value {item_value} is below the minimum {}",
            crate::item::MIN_ITEM_VALUE
        ));
    }

    // Fee and change. Two spends; outputs are item, payment and (maybe) change. Fees are
    // always the conventional ZIP 317 amount, so the transaction looks like any other.
    let fee_no_change = zip317_fee(2);
    let fee_with_change = zip317_fee(3);
    let exact = req.price_zat + fee_no_change;
    let (fee, change) = if payer_value == exact {
        (fee_no_change, 0)
    } else {
        let needed = req.price_zat + fee_with_change;
        if payer_value < needed {
            return Err(anyhow!(
                "buyer note {payer_value} zat cannot cover price {} plus the {fee_with_change} zat fee of a \
                 three-action transaction; pick a note (--buyer-note) worth at least {needed} zat, or exactly \
                 {exact} zat for a two-action transaction",
                req.price_zat
            ));
        }
        let change = payer_value - needed;
        if change <= crate::ZIP317_MARGINAL_FEE {
            return Err(anyhow!(
                "the change of {change} zat would be dust that no wallet spends; pick a note (--buyer-note) \
                 that leaves more than {} zat of change, or exactly {exact} zat for no change",
                crate::ZIP317_MARGINAL_FEE
            ));
        }
        (fee_with_change, change)
    };

    // Recipient addresses.
    let seller_addr = match Address::decode(&network, &req.seller_address) {
        Some(Address::Unified(ua)) => ua
            .orchard()
            .copied()
            .ok_or_else(|| anyhow!("seller address has no Orchard/Ironwood receiver"))?,
        Some(_) => {
            return Err(anyhow!(
                "seller address must be a unified address with an Orchard receiver"
            ))
        }
        None => return Err(anyhow!("cannot decode seller address for this network")),
    };
    // The item goes to a fresh diversified address of the receiving account: nothing on
    // chain links two purchases, or a purchase and the account's default address.
    let buyer_item_addr =
        next_orchard_address(db, req.receive_account.unwrap_or(req.buyer_account))?;
    let buyer_change_addr = fvk_buyer.address_at(0u32, Scope::Internal);

    // Anchor and witnesses at one checkpoint.
    let item_pos = item.position;
    let payer_pos = payer.position;
    let (anchor, path_item, path_payer) = db
        .with_ironwood_tree_mut::<_, _, anyhow::Error>(|tree| {
            let root = tree
                .root_at_checkpoint_id(&anchor_height)
                .map_err(|e| anyhow!("{e:?}"))?
                .ok_or_else(|| anyhow!("no Ironwood checkpoint at height {anchor_height}"))?;
            let w_item = tree
                .witness_at_checkpoint_id_caching(item_pos, &anchor_height)
                .map_err(|e| anyhow!("{e:?}"))?
                .ok_or_else(|| anyhow!("no witness for the item note at height {anchor_height}"))?;
            let w_payer = tree
                .witness_at_checkpoint_id_caching(payer_pos, &anchor_height)
                .map_err(|e| anyhow!("{e:?}"))?
                .ok_or_else(|| {
                    anyhow!("no witness for the buyer note at height {anchor_height}")
                })?;
            Ok((
                orchard::Anchor::from(root),
                orchard::tree::MerklePath::from(w_item),
                orchard::tree::MerklePath::from(w_payer),
            ))
        })?
        .ok_or_else(|| anyhow!("wallet has no Ironwood commitment tree"))?;

    // The bundle. The Orchard builder shuffles spends and outputs independently, so the
    // item spend and the new item note would share an action only by chance. The protocol
    // needs them in one action (the new note's rho is then the spent note's nullifier, which
    // is what binds the lineage), so the bundle is rebuilt with fresh randomness until the
    // pairing holds. Each attempt is cheap: proving happens once, afterwards.
    let target: BlockHeight = target_height.into();
    let expiry = target + req.expiry_delta;
    let item_value_zat = Zatoshis::from_u64(item_value).map_err(|_| anyhow!("item value"))?;
    let price = Zatoshis::from_u64(req.price_zat).map_err(|_| anyhow!("price"))?;
    let change_zat = Zatoshis::from_u64(change).map_err(|_| anyhow!("change"))?;
    let item_memo = memo_bytes(&req.item_memo)?;
    let payment_memo = memo_bytes(&req.memo)?;
    let make_builder = || -> Result<Builder<Network, ()>> {
        let mut builder = Builder::new(
            network,
            target,
            BuildConfig::Standard {
                sapling_anchor: None,
                orchard_anchor: None,
                ironwood_anchor: Some(anchor),
                orchard_padding: BundlePadding::DEFAULT,
                ironwood_padding: BundlePadding::DEFAULT,
            },
        )
        .with_expiry_height(expiry);
        builder
            .add_ironwood_spend::<FeeError>(fvk_item.clone(), item.note, path_item.clone())
            .map_err(|e| anyhow!("adding item spend: {e:?}"))?;
        builder
            .add_ironwood_spend::<FeeError>(fvk_buyer.clone(), payer.note, path_payer.clone())
            .map_err(|e| anyhow!("adding buyer spend: {e:?}"))?;
        builder
            .add_ironwood_output::<FeeError>(
                Some(fvk_buyer.to_ovk(Scope::External)),
                buyer_item_addr,
                item_value_zat,
                item_memo.clone(),
            )
            .map_err(|e| anyhow!("adding item output: {e:?}"))?;
        builder
            .add_ironwood_output::<FeeError>(
                Some(fvk_buyer.to_ovk(Scope::External)),
                seller_addr,
                price,
                payment_memo.clone(),
            )
            .map_err(|e| anyhow!("adding payment output: {e:?}"))?;
        if change > 0 {
            builder
                .add_ironwood_output::<FeeError>(
                    Some(fvk_buyer.to_ovk(Scope::Internal)),
                    buyer_change_addr,
                    change_zat,
                    MemoBytes::empty(),
                )
                .map_err(|e| anyhow!("adding change output: {e:?}"))?;
        }
        Ok(builder)
    };

    let (result, item_action_index, payment_action_index) = build_paired(make_builder)?;

    let pczt = Creator::build_from_parts(result.pczt_parts)
        .ok_or_else(|| anyhow!("PCZT creation failed"))?;
    let pczt = IoFinalizer::new(pczt)
        .finalize_io(rand::rng())
        .map_err(|e| anyhow!("IO finalizer: {e:?}"))?;

    // Prove with viewing keys only.
    let pk = ProvingKey::build(OrchardCircuitVersion::PostNu6_3);
    let pczt = Prover::new(pczt)
        .create_ironwood_proof(rand::rng(), &pk)
        .map_err(|e| anyhow!("prover: {e:?}"))?
        .finish();

    Ok(BuiltTrade {
        pczt,
        item_action_index,
        payment_action_index,
        fee,
        change,
        target_height: target.into(),
        expiry_height: expiry.into(),
    })
}

/// A proven transfer within one wallet (a rotation, a consolidation, a cancelled item).
pub struct SelfSpend {
    /// The full PCZT, ready for the account's signature.
    pub pczt: Pczt,
    /// Total value of the new notes.
    pub value_out: u64,
    /// Fee paid.
    pub fee: u64,
    /// Height at which the transaction expires.
    pub expiry_height: u32,
}

/// One spend of a transfer: the account that owns the note, and the note.
pub type Input = (AccountUuid, NoteRef);
/// One output of a transfer: the account that receives it, at a fresh address and under its
/// own outgoing key, and the value.
pub type Output = (AccountUuid, u64);

/// A proven transfer between accounts of one wallet. Every input is spent, every output goes
/// to a fresh address of its account, and the inputs must exceed the outputs by exactly the
/// ZIP 317 fee for the resulting number of actions. Outputs may be small: an item note is.
pub fn build_transfer(
    db: &mut Db,
    network: Network,
    inputs: &[Input],
    outputs: &[Output],
    min_confirmations: u32,
) -> Result<SelfSpend> {
    if inputs.is_empty() || outputs.is_empty() {
        return Err(anyhow!(
            "a transfer needs at least one input and one output"
        ));
    }
    let (target_height, anchor_height) = heights(db, min_confirmations)?;
    let fee = zip317_fee(inputs.len().max(outputs.len()) as u64);
    let mut spends = Vec::with_capacity(inputs.len());
    let mut total_in = 0u64;
    for (account, note) in inputs {
        let fvk = account_orchard_fvk(db, *account)?;
        let n = ironwood_note(db, *note, target_height)?;
        if fvk.scope_for_address(&n.note.recipient()).is_none() {
            return Err(anyhow!(
                "note {note} is not owned by account {}",
                account.expose_uuid()
            ));
        }
        total_in += n.note.value().inner();
        spends.push((fvk, n));
    }
    let total_out: u64 = outputs.iter().map(|(_, v)| v).sum();
    if total_in != total_out.saturating_add(fee) {
        return Err(anyhow!(
            "inputs ({total_in} zat) must equal outputs ({total_out} zat) plus the fee ({fee} zat)"
        ));
    }
    let mut outs = Vec::with_capacity(outputs.len());
    for (account, value) in outputs {
        let fvk = account_orchard_fvk(db, *account)?;
        let to = next_orchard_address(db, *account)?;
        outs.push((fvk.to_ovk(Scope::External), to, *value));
    }
    let (anchor, paths) = db
        .with_ironwood_tree_mut::<_, _, anyhow::Error>(|tree| {
            let root = tree
                .root_at_checkpoint_id(&anchor_height)
                .map_err(|e| anyhow!("{e:?}"))?
                .ok_or_else(|| anyhow!("no Ironwood checkpoint at height {anchor_height}"))?;
            let mut paths = Vec::with_capacity(spends.len());
            for (_, n) in &spends {
                let w = tree
                    .witness_at_checkpoint_id_caching(n.position, &anchor_height)
                    .map_err(|e| anyhow!("{e:?}"))?
                    .ok_or_else(|| anyhow!("no witness for a note at height {anchor_height}"))?;
                paths.push(orchard::tree::MerklePath::from(w));
            }
            Ok((orchard::Anchor::from(root), paths))
        })?
        .ok_or_else(|| anyhow!("wallet has no Ironwood commitment tree"))?;
    let target: BlockHeight = target_height.into();
    let expiry = target + DEFAULT_MAX_EXPIRY_DELTA;
    let mut builder = Builder::new(
        network,
        target,
        BuildConfig::Standard {
            sapling_anchor: None,
            orchard_anchor: None,
            ironwood_anchor: Some(anchor),
            orchard_padding: BundlePadding::DEFAULT,
            ironwood_padding: BundlePadding::DEFAULT,
        },
    )
    .with_expiry_height(expiry);
    for ((fvk, n), path) in spends.into_iter().zip(paths) {
        builder
            .add_ironwood_spend::<FeeError>(fvk, n.note, path)
            .map_err(|e| anyhow!("adding spend: {e:?}"))?;
    }
    for (ovk, to, value) in outs {
        builder
            .add_ironwood_output::<FeeError>(
                Some(ovk),
                to,
                Zatoshis::from_u64(value).map_err(|_| anyhow!("value"))?,
                MemoBytes::empty(),
            )
            .map_err(|e| anyhow!("adding output: {e:?}"))?;
    }
    let result = builder
        .build_for_pczt(rand::rng(), &FeeRule::standard())
        .map_err(|e| anyhow!("building transaction: {e:?}"))?;
    let pczt = Creator::build_from_parts(result.pczt_parts)
        .ok_or_else(|| anyhow!("PCZT creation failed"))?;
    let pczt = IoFinalizer::new(pczt)
        .finalize_io(rand::rng())
        .map_err(|e| anyhow!("IO finalizer: {e:?}"))?;
    let pk = ProvingKey::build(OrchardCircuitVersion::PostNu6_3);
    let pczt = Prover::new(pczt)
        .create_ironwood_proof(rand::rng(), &pk)
        .map_err(|e| anyhow!("prover: {e:?}"))?
        .finish();
    Ok(SelfSpend {
        pczt,
        value_out: total_out,
        fee,
        expiry_height: expiry.into(),
    })
}

/// Retire a note by moving its value to a fresh address of its own account. Used after an
/// offer did not settle: the seller holds the note's nullifier from the envelope, and once
/// the note is gone that nullifier can only ever point at this transfer.
pub fn build_self_spend(
    db: &mut Db,
    network: Network,
    account: AccountUuid,
    note: NoteRef,
    min_confirmations: u32,
) -> Result<SelfSpend> {
    let (target_height, _) = heights(db, min_confirmations)?;
    let value = ironwood_note(db, note, target_height)?.note.value().inner();
    let fee = zip317_fee(2);
    if value <= fee + ZIP317_MARGINAL_FEE {
        return Err(anyhow!(
            "note {note} is worth {value} zat, too little to move for a {fee} zat fee"
        ));
    }
    build_transfer(
        db,
        network,
        &[(account, note)],
        &[(account, value - fee)],
        min_confirmations,
    )
}

/// Build the bundle until the item spend (spend 0) and the new item note (output 0) share an
/// action. With cross-address outputs enabled the builder pairs them at random, so each
/// attempt succeeds with probability 1/n for n actions; `MAX_PAIRING_ATTEMPTS` makes failure
/// astronomically unlikely. Returns the build result and the item and payment action indices.
pub fn build_paired(
    mut make_builder: impl FnMut() -> Result<Builder<Network, ()>>,
) -> Result<(PcztResult<Network>, usize, usize)> {
    for _ in 0..MAX_PAIRING_ATTEMPTS {
        let result = make_builder()?
            .build_for_pczt(rand::rng(), &FeeRule::standard())
            .map_err(|e| anyhow!("building transaction: {e:?}"))?;
        let meta = &result.ironwood_meta;
        let item_spend = meta
            .spend_action_index(0)
            .ok_or_else(|| anyhow!("no action for the item spend"))?;
        let item_output = meta
            .output_action_index(0)
            .ok_or_else(|| anyhow!("no action for the item output"))?;
        let payment = meta
            .spend_action_index(1)
            .ok_or_else(|| anyhow!("no action for the buyer spend"))?;
        if item_spend == item_output {
            return Ok((result, item_spend, payment));
        }
    }
    Err(anyhow!(
        "could not pair the item spend with the item output in {MAX_PAIRING_ATTEMPTS} attempts"
    ))
}

/// Upper bound on bundle rebuilds while looking for a paired item action.
pub const MAX_PAIRING_ATTEMPTS: usize = 128;

fn memo_bytes(s: &str) -> Result<MemoBytes> {
    if s.is_empty() {
        return Ok(MemoBytes::empty());
    }
    MemoBytes::from_bytes(s.as_bytes()).context("memo too long (512 bytes max)")
}
