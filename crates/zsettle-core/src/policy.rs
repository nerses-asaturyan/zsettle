//! Seller-side policy: every check that must pass before the seller signs action A.
//!
//! The checks, in order:
//! 1. the PCZT is redacted (no buyer full viewing key, witness or rseed present);
//! 2. exactly one action spends a note the seller controls, and it is the listed item;
//! 3. the item action's output pays the item note to the buyer at the item's value;
//! 4. an output decrypts under the seller's incoming viewing key for at least the price;
//! 5. expiry height is within `max_expiry_delta` of the current chain tip;
//! 6. `rk == randomize(ak, alpha)` for the action the seller signs;
//! 7. no other action requires a signature from the seller.

use thiserror::Error;

/// Why the seller refuses to sign.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PolicyError {
    /// The PCZT still carries data that would leak the buyer's keys or notes.
    #[error("pczt is not redacted")]
    NotRedacted,
    /// The seller would have to sign more than the listed item action.
    #[error("extra spend requires seller signature at action {0}")]
    ExtraSpend(usize),
    /// Payment output missing or below the listed price.
    #[error("payment {got} zat below price {want} zat")]
    Underpaid {
        /// Amount found.
        got: u64,
        /// Listed price.
        want: u64,
    },
    /// Expiry too far in the future (free option for the buyer).
    #[error("expiry delta {0} exceeds policy")]
    ExpiryTooLong(u32),
    /// The item action's output does not deliver the item to the buyer correctly.
    #[error("item output mismatch")]
    ItemOutputMismatch,
}
