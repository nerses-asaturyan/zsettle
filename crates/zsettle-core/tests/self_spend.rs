//! A rotation is a plain one-note transfer: two actions after padding, the conventional fee,
//! the whole remaining value to a fresh address of the same account, nothing else.

mod common;

use common::fixture;
use orchard::keys::{PreparedIncomingViewingKey, Scope};
use orchard::note_encryption::{CompactAction, IronwoodDomain};
use pczt::roles::{creator::Creator, io_finalizer::IoFinalizer, verifier::Verifier};
use zcash_note_encryption::{try_compact_note_decryption, EphemeralKeyBytes, COMPACT_NOTE_SIZE};
use zcash_primitives::transaction::builder::{BuildConfig, Builder, BundlePadding};
use zcash_primitives::transaction::fees::zip317::{FeeError, FeeRule};
use zcash_protocol::consensus::{BlockHeight, Network};
use zcash_protocol::memo::MemoBytes;
use zcash_protocol::value::Zatoshis;

#[test]
fn a_rotation_is_an_ordinary_two_action_transfer() {
    let f = fixture();
    let target = BlockHeight::from(3_514_200);
    let fee = zsettle_core::zip317_fee(2);
    let to = f.fvk_buyer.address_at(7u32, Scope::External);
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
    .with_expiry_height(target + zsettle_core::DEFAULT_MAX_EXPIRY_DELTA);
    b.add_ironwood_spend::<FeeError>(f.fvk_buyer.clone(), f.buyer_note, f.paths[2].clone())
        .unwrap();
    b.add_ironwood_output::<FeeError>(
        Some(f.fvk_buyer.to_ovk(Scope::External)),
        to,
        Zatoshis::from_u64(100_000 - fee).unwrap(),
        MemoBytes::empty(),
    )
    .unwrap();
    let result = b
        .build_for_pczt(rand::rngs::OsRng, &FeeRule::standard())
        .unwrap();
    let pczt = Creator::build_from_parts(result.pczt_parts).unwrap();
    let pczt = IoFinalizer::new(pczt).finalize_io().unwrap();
    assert_eq!(*pczt.global().expiry_height(), u32::from(target) + 40);
    let ivk = PreparedIncomingViewingKey::new(&f.fvk_buyer.to_ivk(Scope::External));
    Verifier::new(pczt)
        .with_ironwood::<anyhow::Error, _>(|bundle| {
            assert_eq!(
                bundle.actions().len(),
                2,
                "padded to the two-action minimum"
            );
            let mut found = 0;
            for action in bundle.actions() {
                let enc = action.output().encrypted_note();
                let mut compact = [0u8; COMPACT_NOTE_SIZE];
                compact.copy_from_slice(&enc.enc_ciphertext[..COMPACT_NOTE_SIZE]);
                let act = CompactAction::from_parts(
                    *action.spend().nullifier(),
                    *action.output().cmx(),
                    EphemeralKeyBytes(enc.epk_bytes),
                    compact,
                );
                let domain = IronwoodDomain::for_compact_action(&act);
                if let Some((note, _)) = try_compact_note_decryption(&domain, &ivk, &act) {
                    assert_eq!(note.value().inner(), 100_000 - fee);
                    found += 1;
                }
            }
            assert_eq!(found, 1, "exactly one real output, to the account itself");
            Ok(())
        })
        .unwrap();
}
