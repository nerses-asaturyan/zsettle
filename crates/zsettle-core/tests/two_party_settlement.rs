//! Proven core of zsettle: one Ironwood bundle with spends under two different full
//! viewing keys, proven by a party holding only those viewing keys, signed per party.
//!
//! Action A spends the seller's item note -> new item note to the buyer.
//! Action B spends the buyer's ZEC note -> ZEC note to the seller.
//! No single party's signatures finalize; both together validate.

use incrementalmerkletree::{Hashable, Level};
use orchard::builder::{Builder, BundleType};
use orchard::bundle::{BatchValidator, BundleVersion, Flags};
use orchard::circuit::{OrchardCircuitVersion, ProvingKey, VerifyingKey};
use orchard::keys::{FullViewingKey, Scope, SpendAuthorizingKey, SpendingKey};
use orchard::note::{ExtractedNoteCommitment, Note, NoteVersion, RandomSeed, Rho};
use orchard::tree::{MerkleHashOrchard, MerklePath};
use orchard::value::NoteValue;
use rand::rngs::ThreadRng;
use rand::Rng as _;

const DEPTH: usize = orchard::NOTE_COMMITMENT_TREE_DEPTH;

fn note(rng: &mut ThreadRng, addr: orchard::Address, value: u64) -> Note {
    loop {
        let mut rho_bytes = [0u8; 32];
        rng.fill_bytes(&mut rho_bytes);
        let Some(rho) = Rho::from_bytes(&rho_bytes).into() else {
            continue;
        };
        let mut rs = [0u8; 32];
        rng.fill_bytes(&mut rs);
        let Some(rseed) = RandomSeed::from_bytes(rs, &rho).into() else {
            continue;
        };
        if let Some(n) = Note::from_parts(
            addr,
            NoteValue::from_raw(value),
            rho,
            rseed,
            NoteVersion::V3,
        )
        .into()
        {
            return n;
        }
    }
}

#[test]
fn two_party_bundle_is_atomic() {
    let mut rng = rand::rng();

    let sk_item = SpendingKey::from_bytes([1u8; 32]).unwrap();
    let sk_buyer = SpendingKey::from_bytes([2u8; 32]).unwrap();
    let sk_seller = SpendingKey::from_bytes([3u8; 32]).unwrap();
    let fvk_item = FullViewingKey::from(&sk_item);
    let fvk_buyer = FullViewingKey::from(&sk_buyer);
    let fvk_seller = FullViewingKey::from(&sk_seller);
    let addr_item = fvk_item.address_at(0u32, Scope::External);
    let addr_buyer = fvk_buyer.address_at(0u32, Scope::External);
    let addr_seller = fvk_seller.address_at(0u32, Scope::External);

    let item_value = zsettle_core::item::MIN_ITEM_VALUE;
    let price = 1_000_000u64;
    let fee = zsettle_core::zip317_fee(2);
    let item_note = note(&mut rng, addr_item, item_value);
    let zec_note = note(&mut rng, addr_buyer, price + fee);

    // Two-leaf tree: position 0 = item note, position 1 = buyer's ZEC note.
    let cmx0 = ExtractedNoteCommitment::from(item_note.commitment());
    let cmx1 = ExtractedNoteCommitment::from(zec_note.commitment());
    let mut p0 = [MerkleHashOrchard::empty_leaf(); DEPTH];
    let mut p1 = [MerkleHashOrchard::empty_leaf(); DEPTH];
    p0[0] = MerkleHashOrchard::from_cmx(&cmx1);
    p1[0] = MerkleHashOrchard::from_cmx(&cmx0);
    for l in 1..DEPTH {
        p0[l] = MerkleHashOrchard::empty_root(Level::from(l as u8));
        p1[l] = MerkleHashOrchard::empty_root(Level::from(l as u8));
    }
    let mp0 = MerklePath::from_parts(0, p0);
    let mp1 = MerklePath::from_parts(1, p1);
    let anchor = mp0.root(cmx0);
    assert_eq!(anchor, mp1.root(cmx1));

    let make_bundle = |rng: &mut ThreadRng| {
        let mut b = Builder::new(
            BundleType::Transactional {
                bundle_required: false,
                pad_to_minimum: None,
            },
            BundleVersion::ironwood_v3(),
            Flags::ENABLED,
            anchor,
        )
        .unwrap();
        b.add_spend(fvk_item.clone(), item_note, mp0.clone())
            .unwrap();
        b.add_spend(fvk_buyer.clone(), zec_note, mp1.clone())
            .unwrap();
        b.add_output(
            Some(fvk_item.to_ovk(Scope::External)),
            addr_buyer,
            NoteValue::from_raw(item_value),
            [0u8; 512],
        )
        .unwrap();
        b.add_output(
            Some(fvk_buyer.to_ovk(Scope::External)),
            addr_seller,
            NoteValue::from_raw(price),
            [0u8; 512],
        )
        .unwrap();
        b.build::<i64>(rng).unwrap().unwrap().0
    };

    let bundle = make_bundle(&mut rng);
    assert_eq!(bundle.actions().len(), 2);
    assert_eq!(*bundle.value_balance(), fee as i64);

    let pk = ProvingKey::build(OrchardCircuitVersion::PostNu6_3);
    let sighash = [0x42u8; 32];
    let ask_item = SpendAuthorizingKey::from(&sk_item);
    let ask_buyer = SpendAuthorizingKey::from(&sk_buyer);
    let ask_stranger = SpendAuthorizingKey::from(&sk_seller);

    // Seller alone cannot finalize.
    let proven = bundle
        .create_proof(&pk, &mut rng)
        .expect("FVK holder proves");
    assert!(proven
        .prepare(&mut rng, sighash)
        .sign(&mut rng, &ask_item)
        .finalize()
        .is_err());

    // Buyer plus a stranger cannot finalize (item action stays unsigned).
    let proven = make_bundle(&mut rng).create_proof(&pk, &mut rng).unwrap();
    assert!(proven
        .prepare(&mut rng, sighash)
        .sign(&mut rng, &ask_stranger)
        .sign(&mut rng, &ask_buyer)
        .finalize()
        .is_err());

    // Seller signs A, buyer signs B: finalizes and validates.
    let proven = make_bundle(&mut rng).create_proof(&pk, &mut rng).unwrap();
    let authorized = proven
        .prepare(&mut rng, sighash)
        .sign(&mut rng, &ask_item)
        .sign(&mut rng, &ask_buyer)
        .finalize()
        .expect("both signed");
    let vk = VerifyingKey::build(OrchardCircuitVersion::PostNu6_3);
    authorized.verify_proof(&vk).unwrap();
    let mut v = BatchValidator::new(&vk);
    v.add_bundle(&authorized, sighash).unwrap();
    assert!(v.validate(&mut rng));
}
