//! Two-party bundle construction.
//!
//! Builds the Ironwood bundle from two spends under two different full
//! viewing keys, both witnessed at one shared anchor. See
//! `tests/two_party_settlement.rs` for the proven in-memory flow.

use serde::{Deserialize, Serialize};

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
