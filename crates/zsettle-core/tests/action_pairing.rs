//! The item spend and the new item note must share one action: the new note's rho is then
//! the spent note's nullifier, which is the link `verify` follows from hop to hop. The
//! Orchard builder pairs spends with outputs at random, so the trade builder retries until
//! they coincide. This checks that it does, for bundles with and without change.

mod common;

use common::{fixture, Fixture};
use orchard::keys::{PreparedIncomingViewingKey, Scope};
use orchard::note_encryption::{CompactAction, IronwoodDomain, COMPACT_NOTE_SIZE};
use pczt::roles::{creator::Creator, io_finalizer::IoFinalizer, verifier::Verifier};
use zcash_note_encryption::{try_compact_note_decryption, EphemeralKeyBytes};
use zcash_primitives::transaction::builder::{BuildConfig, Builder, BundlePadding};
use zcash_primitives::transaction::fees::zip317::FeeError;
use zcash_protocol::consensus::{BlockHeight, Network};
use zcash_protocol::memo::MemoBytes;
use zcash_protocol::value::Zatoshis;
use zsettle_core::trade::build_paired;

const ITEM_VALUE: u64 = 1_000;
const BUYER_VALUE: u64 = 100_000;

/// The trade bundle: item spend, buyer spend, item output, payment output and, optionally,
/// change. Same shape as `build_trade`, on synthetic notes.
fn make_builder(f: &Fixture, with_change: bool) -> anyhow::Result<Builder<Network, ()>> {
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
    .with_expiry_height(target + 10);
    b.add_ironwood_spend::<FeeError>(f.fvk_item.clone(), f.item_note, f.paths[0].clone())?;
    b.add_ironwood_spend::<FeeError>(f.fvk_buyer.clone(), f.buyer_note, f.paths[2].clone())?;
    b.add_ironwood_output::<FeeError>(
        None,
        f.fvk_buyer.address_at(0u32, Scope::External),
        Zatoshis::from_u64(ITEM_VALUE).unwrap(),
        MemoBytes::empty(),
    )?;
    let (price, change) = if with_change {
        (5_000, BUYER_VALUE - 5_000 - zsettle_core::zip317_fee(3))
    } else {
        (BUYER_VALUE - zsettle_core::zip317_fee(2), 0)
    };
    b.add_ironwood_output::<FeeError>(
        None,
        f.fvk_seller.address_at(0u32, Scope::External),
        Zatoshis::from_u64(price).unwrap(),
        MemoBytes::empty(),
    )?;
    if change > 0 {
        b.add_ironwood_output::<FeeError>(
            None,
            f.fvk_buyer.address_at(0u32, Scope::Internal),
            Zatoshis::from_u64(change).unwrap(),
            MemoBytes::empty(),
        )?;
    }
    Ok(b)
}

#[test]
fn item_spend_and_item_output_share_an_action() {
    let f = fixture();
    let nf = f.item_note.nullifier(&f.fvk_item);
    let buyer_ivk = PreparedIncomingViewingKey::new(&f.fvk_buyer.to_ivk(Scope::External));
    for with_change in [false, true] {
        for _ in 0..8 {
            let (result, item_idx, payment_idx) =
                build_paired(|| make_builder(&f, with_change)).expect("paired bundle");
            assert_ne!(item_idx, payment_idx);
            assert_eq!(result.ironwood_meta.spend_action_index(0), Some(item_idx));
            assert_eq!(result.ironwood_meta.output_action_index(0), Some(item_idx));

            // The output of the item action is the buyer's new item note, and its rho is the
            // nullifier of the item note just spent.
            let pczt = Creator::build_from_parts(result.pczt_parts).unwrap();
            let pczt = IoFinalizer::new(pczt).finalize_io(rand::rng()).unwrap();
            Verifier::new(pczt)
                .with_ironwood::<anyhow::Error, _>(|bundle| {
                    let action = &bundle.actions()[item_idx];
                    assert_eq!(action.spend().nullifier(), &nf);
                    let enc = action.output().encrypted_note();
                    let mut compact = [0u8; COMPACT_NOTE_SIZE];
                    compact.copy_from_slice(&enc.enc_ciphertext.as_ref()[..COMPACT_NOTE_SIZE]);
                    let act = CompactAction::from_parts(
                        *action.spend().nullifier(),
                        *action.output().cmx(),
                        EphemeralKeyBytes(enc.epk_bytes),
                        compact,
                    );
                    let domain = IronwoodDomain::for_compact_action(&act);
                    let (note, _) = try_compact_note_decryption(&domain, &buyer_ivk, &act)
                        .expect("the item action's output decrypts for the buyer");
                    assert_eq!(note.value().inner(), ITEM_VALUE);
                    assert_eq!(note.rho().to_bytes(), nf.to_bytes());
                    Ok(())
                })
                .unwrap();
        }
    }
}
