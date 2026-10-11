//! # zsettle-core
//!
//! Atomic item-for-ZEC settlement on Zcash, in one shielded Ironwood transaction.
//!
//! An *item* is an ordinary shielded note named by its commitment (the nscv convention).
//! A *trade* is a two-action bundle: action A spends the seller's item note to a new item
//! note owned by the buyer; action B spends the buyer's ZEC note to the seller at the
//! listed price. The buyer proves the whole bundle holding only full viewing keys. The
//! seller signs action A only, the buyer signs action B only. Nothing is valid until both
//! have signed, and a seller who spends the item elsewhere first invalidates the trade
//! through the duplicate nullifier.
//!
//! Modules:
//! - [`wallet`]: light-wallet database access, note lookup, encrypted seed handling.
//! - [`broadcast`]: sending a finished transaction through lightwalletd.
//! - [`trade`]: two-party bundle construction and proving.
//! - [`pczt`]: PCZT role helpers (redact, sign, combine, extract).
//! - [`policy`]: the seller-side checks that run before a signature is produced.
//! - [`listing`]: the public listings file (prices, payout address, item viewing key).
//! - [`item`]: anchor, manifest, consignment encodings and item identity.
//! - [`verify`]: keyless provenance verification from chain data.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod broadcast;
pub mod item;
pub mod listing;
pub mod pczt;
pub mod policy;
pub mod trade;
pub mod verify;
pub mod wallet;

/// Zcash mainnet coin type (ZIP 32).
pub const COIN_TYPE_MAINNET: u32 = 133;

/// Default maximum number of blocks a seller signature stays valid.
/// Shorter than the stack default (40) to limit the buyer's free option.
pub const DEFAULT_MAX_EXPIRY_DELTA: u32 = 10;

/// ZIP 317 marginal fee per logical action, in zatoshis.
pub const ZIP317_MARGINAL_FEE: u64 = 5_000;

/// ZIP 317 minimum number of logical actions charged.
pub const ZIP317_GRACE_ACTIONS: u64 = 2;

/// Conventional ZIP 317 fee for a bundle with `actions` logical actions.
pub fn zip317_fee(actions: u64) -> u64 {
    ZIP317_MARGINAL_FEE * actions.max(ZIP317_GRACE_ACTIONS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fee_schedule() {
        assert_eq!(zip317_fee(1), 10_000);
        assert_eq!(zip317_fee(2), 10_000);
        assert_eq!(zip317_fee(3), 15_000);
    }
}
