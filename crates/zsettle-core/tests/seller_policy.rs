//! Seller policy against synthetic trades: the checks must approve a correct trade and
//! refuse underpayment, long expiry, a missing item spend and an extra seller spend.

mod common;

use common::{fixture, Fixture};
use orchard::keys::Scope;
use pczt::roles::{creator::Creator, io_finalizer::IoFinalizer};
use pczt::Pczt;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use zcash_primitives::transaction::builder::{BuildConfig, Builder, BundlePadding};
use zcash_primitives::transaction::fees::zip317::{FeeError, FeeRule};
use zcash_protocol::consensus::{BlockHeight, BranchId, Network};
use zcash_protocol::memo::MemoBytes;
use zcash_protocol::value::Zatoshis;
use zsettle_core::policy::{check, prepared_ivk, PolicyError, SellerPolicy};

const TARGET: u32 = 3_514_200;

/// Build an unsigned, unproven trade PCZT. `price` is what the buyer actually pays.
fn trade(f: &Fixture, price: u64, expiry_height: u32, spend_extra: bool) -> Pczt {
    trade_paying(f, &[price], expiry_height, spend_extra)
}

/// Same, with the price split over several payment outputs.
fn trade_paying(f: &Fixture, payments: &[u64], expiry_height: u32, spend_extra: bool) -> Pczt {
    let price: u64 = payments.iter().sum();
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
    for p in payments {
        b.add_ironwood_output::<FeeError>(
            None,
            seller_addr,
            Zatoshis::from_u64(*p).unwrap(),
            MemoBytes::empty(),
        )
        .unwrap();
    }
    let spends: u64 = if spend_extra { 3 } else { 2 };
    let outputs: u64 = 2 + payments.len() as u64;
    let fee = zsettle_core::zip317_fee(spends.max(outputs));
    let change = inputs - item_out - price - fee;
    b.add_ironwood_output::<FeeError>(
        None,
        f.fvk_buyer.address_at(0u32, Scope::Internal),
        Zatoshis::from_u64(change).unwrap(),
        MemoBytes::empty(),
    )
    .unwrap();
    let result = b
        .build_for_pczt(ChaCha20Rng::seed_from_u64(5), &FeeRule::standard())
        .unwrap();
    let pczt = Creator::build_from_parts(result.pczt_parts).unwrap();
    IoFinalizer::new(pczt).finalize_io().unwrap()
}

fn policy(price: u64) -> SellerPolicy {
    SellerPolicy {
        price_zat: price,
        max_expiry_delta: 10,
        tip: TARGET - 1,
        branch_id: u32::from(BranchId::for_height(
            &Network::MainNetwork,
            BlockHeight::from(TARGET),
        )),
    }
}

#[test]
fn approves_a_correct_trade() {
    let f = fixture();
    let pczt = trade(&f, 6_000, TARGET + 10, false);
    let nf = f.item_note.nullifier(&f.fvk_item);
    let extra_nf = f.extra_seller_note.nullifier(&f.fvk_item);
    let ivk = prepared_ivk(&f.fvk_seller);
    let ok = check(
        &pczt,
        &nf,
        &[nf, extra_nf],
        &ivk,
        &policy(6_000),
        &[f.anchor],
    )
    .expect("approved");
    assert_eq!(ok.paid_zat, 6_000);
    assert_eq!(ok.expiry_height, TARGET + 10);
}

#[test]
fn refuses_underpayment() {
    let f = fixture();
    let pczt = trade(&f, 5_999, TARGET + 10, false);
    let nf = f.item_note.nullifier(&f.fvk_item);
    let ivk = prepared_ivk(&f.fvk_seller);
    assert_eq!(
        check(&pczt, &nf, &[nf], &ivk, &policy(6_000), &[f.anchor]),
        Err(PolicyError::Underpaid {
            got: 5_999,
            want: 6_000
        })
    );
}

#[test]
fn refuses_long_expiry() {
    let f = fixture();
    let pczt = trade(&f, 6_000, TARGET + 40, false);
    let nf = f.item_note.nullifier(&f.fvk_item);
    let ivk = prepared_ivk(&f.fvk_seller);
    assert!(matches!(
        check(&pczt, &nf, &[nf], &ivk, &policy(6_000), &[f.anchor]),
        Err(PolicyError::ExpiryTooLong { .. })
    ));
}

#[test]
fn refuses_when_the_item_is_not_spent() {
    let f = fixture();
    let pczt = trade(&f, 6_000, TARGET + 10, false);
    let other_nf = f.extra_seller_note.nullifier(&f.fvk_item);
    let ivk = prepared_ivk(&f.fvk_seller);
    assert_eq!(
        check(
            &pczt,
            &other_nf,
            &[other_nf],
            &ivk,
            &policy(6_000),
            &[f.anchor]
        ),
        Err(PolicyError::ItemNotSpent)
    );
}

#[test]
fn refuses_an_extra_seller_spend() {
    let f = fixture();
    let pczt = trade(&f, 6_000, TARGET + 10, true);
    let nf = f.item_note.nullifier(&f.fvk_item);
    let extra_nf = f.extra_seller_note.nullifier(&f.fvk_item);
    let ivk = prepared_ivk(&f.fvk_seller);
    assert!(matches!(
        check(
            &pczt,
            &nf,
            &[nf, extra_nf],
            &ivk,
            &policy(6_000),
            &[f.anchor]
        ),
        Err(PolicyError::ExtraSellerSpend(_))
    ));
}

#[test]
fn payment_to_someone_else_does_not_count() {
    let f = fixture();
    let pczt = trade(&f, 6_000, TARGET + 10, false);
    let nf = f.item_note.nullifier(&f.fvk_item);
    // The buyer's own key sees the item and change outputs, not a payment to the seller.
    let ivk = prepared_ivk(&f.fvk_buyer);
    assert!(matches!(
        check(&pczt, &nf, &[nf], &ivk, &policy(6_000), &[f.anchor]),
        Err(PolicyError::Underpaid { .. })
    ));
}

#[test]
fn refuses_a_transaction_that_never_expires() {
    let f = fixture();
    let pczt = trade(&f, 6_000, 0, false);
    let nf = f.item_note.nullifier(&f.fvk_item);
    let ivk = prepared_ivk(&f.fvk_seller);
    assert_eq!(
        check(&pczt, &nf, &[nf], &ivk, &policy(6_000), &[f.anchor]),
        Err(PolicyError::NoExpiry)
    );
}

#[test]
fn refuses_an_expiry_at_or_below_the_tip() {
    let f = fixture();
    let nf = f.item_note.nullifier(&f.fvk_item);
    let ivk = prepared_ivk(&f.fvk_seller);
    for expiry in [TARGET - 5, TARGET - 1] {
        let pczt = trade(&f, 6_000, expiry, false);
        assert!(matches!(
            check(&pczt, &nf, &[nf], &ivk, &policy(6_000), &[f.anchor]),
            Err(PolicyError::Expired { .. })
        ));
    }
    // One block above the tip is the earliest acceptable expiry.
    let pczt = trade(&f, 6_000, TARGET, false);
    assert!(check(&pczt, &nf, &[nf], &ivk, &policy(6_000), &[f.anchor]).is_ok());
}

#[test]
fn refuses_an_anchor_the_wallet_does_not_know() {
    let f = fixture();
    let pczt = trade(&f, 6_000, TARGET + 10, false);
    let nf = f.item_note.nullifier(&f.fvk_item);
    let ivk = prepared_ivk(&f.fvk_seller);
    assert_eq!(
        check(&pczt, &nf, &[nf], &ivk, &policy(6_000), &[]),
        Err(PolicyError::UnknownAnchor)
    );
    let other = orchard::Anchor::from_bytes([7u8; 32]).unwrap();
    assert_eq!(
        check(&pczt, &nf, &[nf], &ivk, &policy(6_000), &[other]),
        Err(PolicyError::UnknownAnchor)
    );
}

#[test]
fn a_price_paid_in_dust_does_not_count() {
    let f = fixture();
    let pczt = trade_paying(&f, &[2_500, 2_501], TARGET + 10, false);
    let nf = f.item_note.nullifier(&f.fvk_item);
    let ivk = prepared_ivk(&f.fvk_seller);
    assert_eq!(
        check(&pczt, &nf, &[nf], &ivk, &policy(5_001), &[f.anchor]),
        Err(PolicyError::Underpaid {
            got: 0,
            want: 5_001
        })
    );
    // One real payment plus dust: only the real one counts.
    let pczt = trade_paying(&f, &[5_001, 4_000], TARGET + 10, false);
    let ok = check(&pczt, &nf, &[nf], &ivk, &policy(5_001), &[f.anchor]).expect("approved");
    assert_eq!(ok.paid_zat, 5_001);
}

#[test]
fn refuses_a_transaction_for_another_branch() {
    let f = fixture();
    let pczt = trade(&f, 6_000, TARGET + 10, false);
    let nf = f.item_note.nullifier(&f.fvk_item);
    let ivk = prepared_ivk(&f.fvk_seller);
    let mut other = policy(6_000);
    other.branch_id ^= 1;
    assert!(matches!(
        check(&pczt, &nf, &[nf], &ivk, &other, &[f.anchor]),
        Err(PolicyError::WrongBranch { .. })
    ));
}

/// The policy refuses an item action whose spend randomizer is zero: `rk` would then equal
/// the seller's bare `ak`. The builder never emits one, so the PCZT is edited after the
/// fact: the randomizer's 32 bytes are located in the serialized form and zeroed.
#[test]
fn refuses_a_zero_randomizer() {
    use pasta_curves::group::ff::PrimeField;
    use pczt::roles::verifier::Verifier;
    let f = fixture();
    let pczt = trade(&f, 6_000, TARGET + 10, false);
    let nf = f.item_note.nullifier(&f.fvk_item);
    let ivk = prepared_ivk(&f.fvk_seller);
    let found = std::cell::RefCell::new(None);
    Verifier::new(pczt.clone())
        .with_ironwood::<anyhow::Error, _>(|bundle| {
            for (i, action) in bundle.actions().iter().enumerate() {
                if action.spend().nullifier() == &nf {
                    *found.borrow_mut() =
                        Some((i, action.spend().alpha().expect("alpha set").to_repr()));
                }
            }
            Ok(())
        })
        .unwrap();
    let (index, alpha) = found.into_inner().expect("the item action");
    let mut bytes = zsettle_core::pczt::to_bytes(&pczt).unwrap();
    let hits: Vec<usize> = bytes
        .windows(32)
        .enumerate()
        .filter(|(_, w)| *w == &alpha[..])
        .map(|(at, _)| at)
        .collect();
    assert_eq!(hits.len(), 1, "the randomizer appears exactly once");
    bytes[hits[0]..hits[0] + 32].fill(0);
    let edited = zsettle_core::pczt::from_bytes(&bytes).unwrap();
    assert_eq!(
        check(&edited, &nf, &[nf], &ivk, &policy(6_000), &[f.anchor]),
        Err(PolicyError::ZeroRandomizer(index))
    );
}
