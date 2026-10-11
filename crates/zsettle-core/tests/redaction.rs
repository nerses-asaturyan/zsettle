//! What the seller's copy of a trade contains, and that it still carries a proof the seller
//! can check. The buyer's keys, witnesses, note data, randomizers and value trapdoors must be
//! gone; the item action keeps the seller's own note data and its randomizer.

mod common;

use common::{fixture, Fixture};
use orchard::circuit::{OrchardCircuitVersion, ProvingKey};
use orchard::keys::Scope;
use pczt::roles::{
    creator::Creator, io_finalizer::IoFinalizer, prover::Prover, verifier::Verifier,
};
use pczt::Pczt;
use zcash_primitives::transaction::builder::{BuildConfig, Builder, BundlePadding};
use zcash_primitives::transaction::fees::zip317::FeeError;
use zcash_protocol::consensus::{BlockHeight, Network};
use zcash_protocol::memo::MemoBytes;
use zcash_protocol::value::Zatoshis;
use zsettle_core::pczt::{redact_for_seller, verify_proof, verifying_key};
use zsettle_core::trade::build_paired;

fn make_builder(f: &Fixture) -> anyhow::Result<Builder<Network, ()>> {
    let target = BlockHeight::from(3_514_200);
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
    .with_expiry_height(target + 40);
    b.add_ironwood_spend::<FeeError>(f.fvk_item.clone(), f.item_note, f.paths[0].clone())?;
    b.add_ironwood_spend::<FeeError>(f.fvk_buyer.clone(), f.buyer_note, f.paths[2].clone())?;
    b.add_ironwood_output::<FeeError>(
        Some(f.fvk_buyer.to_ovk(Scope::External)),
        f.fvk_buyer.address_at(5u32, Scope::External),
        Zatoshis::from_u64(1_000).unwrap(),
        MemoBytes::empty(),
    )?;
    b.add_ironwood_output::<FeeError>(
        Some(f.fvk_buyer.to_ovk(Scope::External)),
        f.fvk_seller.address_at(0u32, Scope::External),
        Zatoshis::from_u64(5_000).unwrap(),
        MemoBytes::empty(),
    )?;
    b.add_ironwood_output::<FeeError>(
        Some(f.fvk_buyer.to_ovk(Scope::Internal)),
        f.fvk_buyer.address_at(0u32, Scope::Internal),
        Zatoshis::from_u64(100_000 - 5_000 - zsettle_core::zip317_fee(3)).unwrap(),
        MemoBytes::empty(),
    )?;
    Ok(b)
}

/// An unproven trade PCZT and the index of its item action.
fn unproven(f: &Fixture) -> (Pczt, usize) {
    let (result, item_idx, _) = build_paired(|| make_builder(f)).unwrap();
    let pczt = Creator::build_from_parts(result.pczt_parts).unwrap();
    (IoFinalizer::new(pczt).finalize_io().unwrap(), item_idx)
}

#[test]
fn seller_copy_keeps_only_what_the_seller_needs() {
    let f = fixture();
    let (pczt, item_idx) = unproven(&f);
    let redacted = redact_for_seller(pczt, item_idx).unwrap();
    Verifier::new(redacted)
        .with_ironwood::<anyhow::Error, _>(|bundle| {
            assert!(bundle.bsk().is_none(), "binding key must be gone");
            for (i, action) in bundle.actions().iter().enumerate() {
                assert!(
                    action.rcv().is_none(),
                    "action {i}: value trapdoor must be gone"
                );
                let spend = action.spend();
                assert!(
                    spend.witness().is_none(),
                    "action {i}: witness must be gone"
                );
                assert!(spend.dummy_sk().is_none());
                assert!(spend.zip32_derivation().is_none());
                let output = action.output();
                assert!(output.recipient().is_none(), "action {i}: output recipient");
                assert!(output.value().is_none(), "action {i}: output value");
                assert!(output.rseed().is_none(), "action {i}: output rseed");
                assert!(output.ock().is_none(), "action {i}: output ock");
                assert!(output.user_address().is_none());
                if i == item_idx {
                    assert!(spend.fvk().is_some(), "item action keeps the seller's key");
                    assert!(spend.alpha().is_some(), "item action keeps its randomizer");
                    assert_eq!(spend.value().map(|v| v.inner()), Some(1_000));
                } else {
                    assert!(spend.fvk().is_none(), "action {i}: buyer key must be gone");
                    assert!(
                        spend.alpha().is_none(),
                        "action {i}: buyer randomizer must be gone"
                    );
                    assert!(spend.recipient().is_none());
                    assert!(spend.value().is_none());
                    assert!(spend.rho().is_none());
                    assert!(spend.rseed().is_none());
                }
            }
            Ok(())
        })
        .unwrap();
}

#[test]
fn an_envelope_without_a_proof_is_refused() {
    let f = fixture();
    let (pczt, item_idx) = unproven(&f);
    let redacted = redact_for_seller(pczt, item_idx).unwrap();
    let vk = verifying_key();
    let err = verify_proof(&redacted, &vk).unwrap_err().to_string();
    assert!(err.contains("no proof"), "{err}");
}

#[test]
fn a_proven_seller_copy_verifies_and_a_tampered_one_does_not() {
    let f = fixture();
    let (pczt, item_idx) = unproven(&f);
    let pk = ProvingKey::build(OrchardCircuitVersion::PostNu6_3);
    let proven = Prover::new(pczt)
        .create_ironwood_proof(&pk)
        .unwrap()
        .finish();
    let redacted = redact_for_seller(proven, item_idx).unwrap();
    let vk = verifying_key();
    verify_proof(&redacted, &vk).unwrap();

    // The same proof does not verify for a different bundle.
    let (other, other_idx) = unproven(&f);
    let other = redact_for_seller(other, other_idx).unwrap();
    assert!(verify_proof(&other, &vk).is_err());
}
