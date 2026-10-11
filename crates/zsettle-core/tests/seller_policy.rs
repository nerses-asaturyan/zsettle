//! Seller policy against synthetic trades: the checks must approve a correct trade and
//! refuse underpayment, long expiry, a missing item spend and an extra seller spend.

mod common;

use common::{fixture, Fixture};
use orchard::keys::Scope;
use pczt::roles::{creator::Creator, io_finalizer::IoFinalizer};
use pczt::Pczt;
use zcash_primitives::transaction::builder::{BuildConfig, Builder, BundlePadding};
use zcash_primitives::transaction::fees::zip317::{FeeError, FeeRule};
use zcash_protocol::consensus::{BlockHeight, Network};
use zcash_protocol::memo::MemoBytes;
use zcash_protocol::value::Zatoshis;
use zsettle_core::policy::{check, prepared_ivk, PolicyError, SellerPolicy};

const TARGET: u32 = 3_514_200;

/// Build an unsigned, unproven trade PCZT. `price` is what the buyer actually pays.
fn trade(f: &Fixture, price: u64, expiry_height: u32, spend_extra: bool) -> Pczt {
    let target = BlockHeight::from(TARGET);
    let mut b = Builder::new(
        Network::MainNetwork,
        target,
        BuildConfig::Standard {
            sapling_anchor: None,
            orchard_anchor: None,
            ironwood_anchor: Some(f.anchor),
            orchard_padding: BundlePadding::DEFAULT,
            ironwood_padding: BundlePadding::DEFAULT,
        },
    )
    .with_expiry_height(BlockHeight::from(expiry_height));
    b.add_ironwood_spend::<FeeError>(f.fvk_item.clone(), f.item_note, f.paths[0].clone())
        .unwrap();
    let mut inputs = 1_000 + 100_000;
    if spend_extra {
        b.add_ironwood_spend::<FeeError>(
            f.fvk_item.clone(),
            f.extra_seller_note,
            f.paths[1].clone(),
        )
        .unwrap();
        inputs += 1_000;
    }
    b.add_ironwood_spend::<FeeError>(f.fvk_buyer.clone(), f.buyer_note, f.paths[2].clone())
        .unwrap();
    let seller_addr = f.fvk_seller.address_at(0u32, Scope::External);
    let buyer_addr = f.fvk_buyer.address_at(0u32, Scope::External);
    let item_out = if spend_extra { 2_000 } else { 1_000 };
    b.add_ironwood_output::<FeeError>(
        None,
        buyer_addr,
        Zatoshis::from_u64(item_out).unwrap(),
        MemoBytes::empty(),
    )
    .unwrap();
    b.add_ironwood_output::<FeeError>(
        None,
        seller_addr,
        Zatoshis::from_u64(price).unwrap(),
        MemoBytes::empty(),
    )
    .unwrap();
    let fee = zsettle_core::zip317_fee(3);
    let change = inputs - item_out - price - fee;
    b.add_ironwood_output::<FeeError>(
        None,
        f.fvk_buyer.address_at(0u32, Scope::Internal),
        Zatoshis::from_u64(change).unwrap(),
        MemoBytes::empty(),
    )
    .unwrap();
    let result = b.build_for_pczt(rand::rng(), &FeeRule::standard()).unwrap();
    let pczt = Creator::build_from_parts(result.pczt_parts).unwrap();
    IoFinalizer::new(pczt).finalize_io(rand::rng()).unwrap()
}

fn policy(price: u64) -> SellerPolicy {
    SellerPolicy {
        price_zat: price,
        max_expiry_delta: 10,
        tip: TARGET - 1,
    }
}

#[test]
fn approves_a_correct_trade() {
    let f = fixture();
    let pczt = trade(&f, 5_000, TARGET + 10, false);
    let nf = f.item_note.nullifier(&f.fvk_item);
    let extra_nf = f.extra_seller_note.nullifier(&f.fvk_item);
    let ivk = prepared_ivk(&f.fvk_seller);
    let ok = check(
        &pczt,
        &nf,
        &[nf, extra_nf],
        &ivk,
        &policy(5_000),
        &[f.anchor],
    )
    .expect("approved");
    assert_eq!(ok.paid_zat, 5_000);
    assert_eq!(ok.expiry_height, TARGET + 10);
}

#[test]
fn refuses_underpayment() {
    let f = fixture();
    let pczt = trade(&f, 4_999, TARGET + 10, false);
    let nf = f.item_note.nullifier(&f.fvk_item);
    let ivk = prepared_ivk(&f.fvk_seller);
    assert_eq!(
        check(&pczt, &nf, &[nf], &ivk, &policy(5_000), &[f.anchor]),
        Err(PolicyError::Underpaid {
            got: 4_999,
            want: 5_000
        })
    );
}

#[test]
fn refuses_long_expiry() {
    let f = fixture();
    let pczt = trade(&f, 5_000, TARGET + 40, false);
    let nf = f.item_note.nullifier(&f.fvk_item);
    let ivk = prepared_ivk(&f.fvk_seller);
    assert!(matches!(
        check(&pczt, &nf, &[nf], &ivk, &policy(5_000), &[f.anchor]),
        Err(PolicyError::ExpiryTooLong { .. })
    ));
}

#[test]
fn refuses_when_the_item_is_not_spent() {
    let f = fixture();
    let pczt = trade(&f, 5_000, TARGET + 10, false);
    let other_nf = f.extra_seller_note.nullifier(&f.fvk_item);
    let ivk = prepared_ivk(&f.fvk_seller);
    assert_eq!(
        check(
            &pczt,
            &other_nf,
            &[other_nf],
            &ivk,
            &policy(5_000),
            &[f.anchor]
        ),
        Err(PolicyError::ItemNotSpent)
    );
}

#[test]
fn refuses_an_extra_seller_spend() {
    let f = fixture();
    let pczt = trade(&f, 5_000, TARGET + 10, true);
    let nf = f.item_note.nullifier(&f.fvk_item);
    let extra_nf = f.extra_seller_note.nullifier(&f.fvk_item);
    let ivk = prepared_ivk(&f.fvk_seller);
    assert!(matches!(
        check(
            &pczt,
            &nf,
            &[nf, extra_nf],
            &ivk,
            &policy(5_000),
            &[f.anchor]
        ),
        Err(PolicyError::ExtraSellerSpend(_))
    ));
}

#[test]
fn payment_to_someone_else_does_not_count() {
    let f = fixture();
    let pczt = trade(&f, 5_000, TARGET + 10, false);
    let nf = f.item_note.nullifier(&f.fvk_item);
    // The buyer's own key sees the item and change outputs, not a payment to the seller.
    let ivk = prepared_ivk(&f.fvk_buyer);
    assert!(matches!(
        check(&pczt, &nf, &[nf], &ivk, &policy(5_000), &[f.anchor]),
        Err(PolicyError::Underpaid { .. })
    ));
}

#[test]
fn refuses_a_transaction_that_never_expires() {
    let f = fixture();
    let pczt = trade(&f, 5_000, 0, false);
    let nf = f.item_note.nullifier(&f.fvk_item);
    let ivk = prepared_ivk(&f.fvk_seller);
    assert_eq!(
        check(&pczt, &nf, &[nf], &ivk, &policy(5_000), &[f.anchor]),
        Err(PolicyError::NoExpiry)
    );
}

#[test]
fn refuses_an_expiry_at_or_below_the_tip() {
    let f = fixture();
    let nf = f.item_note.nullifier(&f.fvk_item);
    let ivk = prepared_ivk(&f.fvk_seller);
    for expiry in [TARGET - 5, TARGET - 1] {
        let pczt = trade(&f, 5_000, expiry, false);
        assert!(matches!(
            check(&pczt, &nf, &[nf], &ivk, &policy(5_000), &[f.anchor]),
            Err(PolicyError::Expired { .. })
        ));
    }
    // One block above the tip is the earliest acceptable expiry.
    let pczt = trade(&f, 5_000, TARGET, false);
    assert!(check(&pczt, &nf, &[nf], &ivk, &policy(5_000), &[f.anchor]).is_ok());
}

#[test]
fn refuses_an_anchor_the_wallet_does_not_know() {
    let f = fixture();
    let pczt = trade(&f, 5_000, TARGET + 10, false);
    let nf = f.item_note.nullifier(&f.fvk_item);
    let ivk = prepared_ivk(&f.fvk_seller);
    assert_eq!(
        check(&pczt, &nf, &[nf], &ivk, &policy(5_000), &[]),
        Err(PolicyError::UnknownAnchor)
    );
    let other = orchard::Anchor::from_bytes([7u8; 32]).unwrap();
    assert_eq!(
        check(&pczt, &nf, &[nf], &ivk, &policy(5_000), &[other]),
        Err(PolicyError::UnknownAnchor)
    );
}
