//! Synthetic notes, a tiny commitment tree and a three-party fixture shared by the tests.

#![allow(dead_code)]

use incrementalmerkletree::{Hashable, Level};
use orchard::keys::{FullViewingKey, Scope, SpendingKey};
use orchard::note::{ExtractedNoteCommitment, Note, NoteVersion, RandomSeed, Rho};
use orchard::tree::{MerkleHashOrchard, MerklePath};
use orchard::value::NoteValue;
use rand::{RngCore, SeedableRng};
use rand_chacha::ChaCha20Rng;

pub const DEPTH: usize = orchard::NOTE_COMMITMENT_TREE_DEPTH;

/// A random v3 note to `addr` worth `value`.
pub fn note(rng: &mut ChaCha20Rng, addr: orchard::Address, value: u64) -> Note {
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
pub fn tree(notes: &[Note]) -> (Vec<MerklePath>, orchard::Anchor) {
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

/// Three keys and three notes in one tree: the item (position 0), a second note of the
/// seller's item account (position 1) and the buyer's paying note (position 2).
pub struct Fixture {
    pub fvk_item: FullViewingKey,
    pub fvk_seller: FullViewingKey,
    pub fvk_buyer: FullViewingKey,
    pub item_note: Note,
    pub extra_seller_note: Note,
    pub buyer_note: Note,
    pub paths: Vec<MerklePath>,
    pub anchor: orchard::Anchor,
}

pub fn fixture() -> Fixture {
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
