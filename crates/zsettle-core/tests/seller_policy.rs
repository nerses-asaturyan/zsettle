//! Seller policy against synthetic trades: the checks must approve a correct trade and
//! refuse underpayment, long expiry, a missing item spend and an extra seller spend.

use incrementalmerkletree::{Hashable, Level};
use orchard::keys::{FullViewingKey, Scope, SpendingKey};
use orchard::note::{ExtractedNoteCommitment, Note, NoteVersion, RandomSeed, Rho};
use orchard::tree::{MerkleHashOrchard, MerklePath};
use orchard::value::NoteValue;
use pczt::roles::{creator::Creator, io_finalizer::IoFinalizer};
use pczt::Pczt;
use rand::{RngCore, SeedableRng};
use rand_chacha::ChaCha20Rng;
use zcash_primitives::transaction::builder::{BuildConfig, Builder, BundlePadding};
use zcash_primitives::transaction::fees::zip317::{FeeError, FeeRule};
use zcash_protocol::consensus::{BlockHeight, Network};
use zcash_protocol::memo::MemoBytes;
use zcash_protocol::value::Zatoshis;
use zsettle_core::policy::{check, prepared_ivk, PolicyError, SellerPolicy};

const DEPTH: usize = orchard::NOTE_COMMITMENT_TREE_DEPTH;
const TARGET: u32 = 3_514_200;

fn note(rng: &mut ChaCha20Rng, addr: orchard::Address, value: u64) -> Note {
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

/// A small tree holding the given notes at positions 0..n; returns their paths and the anchor.
fn tree(notes: &[Note]) -> (Vec<MerklePath>, orchard::Anchor) {
    let leaves: Vec<MerkleHashOrchard> = notes
        .iter()
        .map(|n| MerkleHashOrchard::from_cmx(&ExtractedNoteCommitment::from(n.commitment())))
        .collect();
    // Level-0 siblings; levels above hold the empty roots of a tree with <= 4 leaves.
    let empty = MerkleHashOrchard::empty_leaf();
    let get = |i: usize| leaves.get(i).copied().unwrap_or(empty);
    let l1 = [
        MerkleHashOrchard::combine(Level::from(0), &get(0), &get(1)),
        MerkleHashOrchard::combine(Level::from(0), &get(2), &get(3)),
    ];
    let mut paths = Vec::new();
    for (i, _) in notes.iter().enumerate() {
        let mut p = [empty; DEPTH];
        p[0] = get(i ^ 1);
        p[1] = l1[(i / 2) ^ 1];
        for (l, slot) in p.iter_mut().enumerate().skip(2) {
            *slot = MerkleHashOrchard::empty_root(Level::from(l as u8));
        }
        paths.push(MerklePath::from_parts(i as u32, p));
    }
    let anchor = paths[0].root(ExtractedNoteCommitment::from(notes[0].commitment()));
    for (i, p) in paths.iter().enumerate() {
        assert_eq!(
            anchor,
            p.root(ExtractedNoteCommitment::from(notes[i].commitment()))
        );
    }
    (paths, anchor)
}

struct Fixture {
    fvk_item: FullViewingKey,
    fvk_seller: FullViewingKey,
    fvk_buyer: FullViewingKey,
    item_note: Note,
    extra_seller_note: Note,
    buyer_note: Note,
    paths: Vec<MerklePath>,
    anchor: orchard::Anchor,
}

fn fixture() -> Fixture {
    let mut rng = ChaCha20Rng::seed_from_u64(11);
    let fvk_item = FullViewingKey::from(&SpendingKey::from_bytes([1u8; 32]).unwrap());
    let fvk_buyer = FullViewingKey::from(&SpendingKey::from_bytes([2u8; 32]).unwrap());
    let fvk_seller = FullViewingKey::from(&SpendingKey::from_bytes([3u8; 32]).unwrap());
    let item_note = note(&mut rng, fvk_item.address_at(0u32, Scope::External), 1_000);
    let extra_seller_note = note(&mut rng, fvk_item.address_at(1u32, Scope::External), 1_000);
    let buyer_note = note(
        &mut rng,
        fvk_buyer.address_at(0u32, Scope::External),
        100_000,
    );
    let (paths, anchor) = tree(&[item_note, extra_seller_note, buyer_note]);
    Fixture {
        fvk_item,
        fvk_seller,
        fvk_buyer,
        item_note,
        extra_seller_note,
        buyer_note,
        paths,
        anchor,
    }
}

/// Build an unsigned, unproven trade PCZT. `price` is what the buyer actually pays.
fn trade(f: &Fixture, price: u64, expiry_delta: u32, spend_extra: bool) -> Pczt {
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
    .with_expiry_height(target + expiry_delta);
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
    }
}

#[test]
fn approves_a_correct_trade() {
    let f = fixture();
    let pczt = trade(&f, 5_000, 10, false);
    let nf = f.item_note.nullifier(&f.fvk_item);
    let extra_nf = f.extra_seller_note.nullifier(&f.fvk_item);
    let ivk = prepared_ivk(&f.fvk_seller);
    let ok = check(&pczt, &nf, &[nf, extra_nf], &ivk, &policy(5_000)).expect("approved");
    assert_eq!(ok.paid_zat, 5_000);
    assert_eq!(ok.expiry_height, TARGET + 10);
}

#[test]
fn refuses_underpayment() {
    let f = fixture();
    let pczt = trade(&f, 4_999, 10, false);
    let nf = f.item_note.nullifier(&f.fvk_item);
    let ivk = prepared_ivk(&f.fvk_seller);
    assert_eq!(
        check(&pczt, &nf, &[nf], &ivk, &policy(5_000)),
        Err(PolicyError::Underpaid {
            got: 4_999,
            want: 5_000
        })
    );
}

#[test]
fn refuses_long_expiry() {
    let f = fixture();
    let pczt = trade(&f, 5_000, 40, false);
    let nf = f.item_note.nullifier(&f.fvk_item);
    let ivk = prepared_ivk(&f.fvk_seller);
    assert!(matches!(
        check(&pczt, &nf, &[nf], &ivk, &policy(5_000)),
        Err(PolicyError::ExpiryTooLong { .. })
    ));
}

#[test]
fn refuses_when_the_item_is_not_spent() {
    let f = fixture();
    let pczt = trade(&f, 5_000, 10, false);
    let other_nf = f.extra_seller_note.nullifier(&f.fvk_item);
    let ivk = prepared_ivk(&f.fvk_seller);
    assert_eq!(
        check(&pczt, &other_nf, &[other_nf], &ivk, &policy(5_000)),
        Err(PolicyError::ItemNotSpent)
    );
}

#[test]
fn refuses_an_extra_seller_spend() {
    let f = fixture();
    let pczt = trade(&f, 5_000, 10, true);
    let nf = f.item_note.nullifier(&f.fvk_item);
    let extra_nf = f.extra_seller_note.nullifier(&f.fvk_item);
    let ivk = prepared_ivk(&f.fvk_seller);
    assert!(matches!(
        check(&pczt, &nf, &[nf, extra_nf], &ivk, &policy(5_000)),
        Err(PolicyError::ExtraSellerSpend(_))
    ));
}

#[test]
fn payment_to_someone_else_does_not_count() {
    let f = fixture();
    let pczt = trade(&f, 5_000, 10, false);
    let nf = f.item_note.nullifier(&f.fvk_item);
    // The buyer's own key sees the item and change outputs, not a payment to the seller.
    let ivk = prepared_ivk(&f.fvk_buyer);
    assert!(matches!(
        check(&pczt, &nf, &[nf], &ivk, &policy(5_000)),
        Err(PolicyError::Underpaid { .. })
    ));
}
