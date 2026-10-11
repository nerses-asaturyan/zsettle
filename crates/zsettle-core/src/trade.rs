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
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use zcash_client_backend::data_api::WalletCommitmentTrees;
use zcash_client_sqlite::AccountUuid;
use zcash_keys::address::Address;
use zcash_primitives::transaction::builder::{BuildConfig, Builder, BundlePadding};
use zcash_primitives::transaction::fees::zip317::{FeeError, FeeRule};
use zcash_protocol::consensus::{BlockHeight, Network};
use zcash_protocol::memo::MemoBytes;
use zcash_protocol::value::Zatoshis;

use crate::wallet::{account_ufvk, heights, ironwood_note, Db, NoteRef};
use crate::{zip317_fee, DEFAULT_MAX_EXPIRY_DELTA};

/// A public listing: what is for sale and at what price. Participants stay shielded.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Listing {
    /// The item being sold.
    pub item: crate::item::ItemId,
    /// Price in zatoshis, paid to the seller's shielded address.
    pub price_zat: u64,
    /// Maximum blocks the seller's signature may stay valid.
    pub max_expiry_delta: u32,
}

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
}

impl TradeRequest {
    /// Policy defaults for the optional fields.
    pub fn defaults() -> (u32, u32) {
        (DEFAULT_MAX_EXPIRY_DELTA, 3)
    }
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

    // Fee and change. Two spends; outputs are item, payment and (maybe) change.
    let fee_no_change = zip317_fee(2);
    let (fee, change) = if payer_value == req.price_zat + fee_no_change {
        (fee_no_change, 0)
    } else {
        let fee = zip317_fee(3);
        let needed = req.price_zat + fee;
        if payer_value < needed {
            return Err(anyhow!(
                "buyer note {payer_value} zat cannot cover price {} + fee {fee}",
                req.price_zat
            ));
        }
        (fee, payer_value - needed)
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
    let buyer_item_addr = fvk_buyer.address_at(0u32, Scope::External);
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

    // The bundle.
    let target: BlockHeight = target_height.into();
    let expiry = target + req.expiry_delta;
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
        .add_ironwood_spend::<FeeError>(fvk_item.clone(), item.note, path_item)
        .map_err(|e| anyhow!("adding item spend: {e:?}"))?;
    builder
        .add_ironwood_spend::<FeeError>(fvk_buyer.clone(), payer.note, path_payer)
        .map_err(|e| anyhow!("adding buyer spend: {e:?}"))?;
    builder
        .add_ironwood_output::<FeeError>(
            Some(fvk_item.to_ovk(Scope::External)),
            buyer_item_addr,
            Zatoshis::from_u64(item_value).map_err(|_| anyhow!("item value"))?,
            MemoBytes::empty(),
        )
        .map_err(|e| anyhow!("adding item output: {e:?}"))?;
    builder
        .add_ironwood_output::<FeeError>(
            Some(fvk_buyer.to_ovk(Scope::External)),
            seller_addr,
            Zatoshis::from_u64(req.price_zat).map_err(|_| anyhow!("price"))?,
            memo_bytes(&req.memo)?,
        )
        .map_err(|e| anyhow!("adding payment output: {e:?}"))?;
    if change > 0 {
        builder
            .add_ironwood_output::<FeeError>(
                Some(fvk_buyer.to_ovk(Scope::Internal)),
                buyer_change_addr,
                Zatoshis::from_u64(change).map_err(|_| anyhow!("change"))?,
                MemoBytes::empty(),
            )
            .map_err(|e| anyhow!("adding change output: {e:?}"))?;
    }

    let result = builder
        .build_for_pczt(OsRng, &FeeRule::standard())
        .map_err(|e| anyhow!("building transaction: {e:?}"))?;
    let item_action_index = result
        .ironwood_meta
        .spend_action_index(0)
        .ok_or_else(|| anyhow!("no action for the item spend"))?;
    let payment_action_index = result
        .ironwood_meta
        .spend_action_index(1)
        .ok_or_else(|| anyhow!("no action for the buyer spend"))?;

    let pczt = Creator::build_from_parts(result.pczt_parts)
        .ok_or_else(|| anyhow!("PCZT creation failed"))?;
    let pczt = IoFinalizer::new(pczt)
        .finalize_io()
        .map_err(|e| anyhow!("IO finalizer: {e:?}"))?;

    // Prove with viewing keys only.
    let pk = ProvingKey::build(OrchardCircuitVersion::PostNu6_3);
    let pczt = Prover::new(pczt)
        .create_ironwood_proof(&pk)
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

fn memo_bytes(s: &str) -> Result<MemoBytes> {
    if s.is_empty() {
        return Ok(MemoBytes::empty());
    }
    MemoBytes::from_bytes(s.as_bytes()).context("memo too long (512 bytes max)")
}
