//! Note-sealed items: identity, anchor, manifest and consignment.
//!
//! Day 6–7 work. Encodings follow the nscv convention where it is cheap to do so
//! (`ZBT1` anchor, `ZBM1` manifest, 96-byte per-hop consignment entries), updated for
//! Ironwood v3 notes. Items MUST carry non-zero value: a zero-value note has no Merkle
//! binding in the Action circuit.

use serde::{Deserialize, Serialize};

/// Identity of an item: the extracted note commitment of its genesis note.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ItemId(pub [u8; 32]);

impl ItemId {
    /// Hex form used in listings and CLI output.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

/// Minimum item note value in zatoshis. Zero-value notes are not Merkle-bound.
pub const MIN_ITEM_VALUE: u64 = 1_000;
