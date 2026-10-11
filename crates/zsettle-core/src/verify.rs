//! Keyless provenance verification.
//!
//! Given an item's genesis commitment and the chain's nullifier set and
//! commitment tree, walk the hops: each hop's successor note has `rho == nf(previous)`;
//! recompute `cmx` from the consignment's note data with the v3 `rcm` derivation; the
//! current holder is the last unspent hop.

/// Result of walking an item's history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Provenance {
    /// The item exists and the last hop is unspent.
    CurrentlyHeld {
        /// Number of transfers since genesis.
        hops: u32,
    },
    /// The last known hop was spent but no valid successor was found.
    Burned,
    /// The chain data contradicts the consignment.
    Invalid(String),
}
