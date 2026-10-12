//! # zsettle-core
//!
//! Atomic item-for-ZEC settlement on Zcash, in one shielded Ironwood transaction.
//!
//! An *item* is an ordinary shielded note, identified by the transaction and action index
//! that created it.
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
//! - [`mint`]: minting item notes and listing an account's unspent notes.
//! - [`item`]: what an item is and its minimum value.
//! - [`verify`]: provenance verification from chain data with viewing keys only.
//! - [`ownership`]: signed proofs that the holder of an item account controls a note now.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod broadcast;
pub mod item;
pub mod listing;
pub mod mint;
pub mod ownership;
pub mod pczt;
pub mod policy;
pub mod trade;
pub mod verify;
pub mod wallet;

/// Default maximum number of blocks a seller signature stays valid. Equal to the librustzcash
/// default transaction expiry, so a trade carries the same expiry as any wallet transaction
/// and cannot be picked out on chain by it. The seller can end the option early by spending
/// the item itself.
pub const DEFAULT_MAX_EXPIRY_DELTA: u32 = 40;

/// ZIP 317 marginal fee per logical action, in zatoshis.
pub const ZIP317_MARGINAL_FEE: u64 = 5_000;

/// ZIP 317 minimum number of logical actions charged.
pub const ZIP317_GRACE_ACTIONS: u64 = 2;

/// Lowest price a listing may carry. A payment at or below the ZIP 317 marginal fee is dust
/// that standard wallets refuse to spend.
pub const MIN_PRICE_ZAT: u64 = ZIP317_MARGINAL_FEE + 1;

/// Largest trade envelope the seller service reads. A real one is about 10 KB with its proof.
pub const MAX_ENVELOPE_BYTES: u64 = 1 << 20;

/// Parse an amount typed by a person: zatoshis as a whole number (`100000`, `100000 zat`), or
/// ZEC with a decimal point or a unit (`0.001`, `0.001 ZEC`, `1zec`), at most eight decimals.
pub fn parse_zat(text: &str) -> Result<u64, String> {
    let lower = text.trim().to_ascii_lowercase();
    let (number, zec) = if let Some(n) = lower.strip_suffix("zec") {
        (n.trim(), true)
    } else if let Some(n) = lower.strip_suffix("zat") {
        (n.trim(), false)
    } else {
        (lower.as_str(), lower.contains('.'))
    };
    let bad = || format!("not an amount: {text:?} (zatoshis, or ZEC such as 0.001)");
    if number.is_empty() {
        return Err(bad());
    }
    if !zec {
        return number.parse::<u64>().map_err(|_| bad());
    }
    let (whole, frac) = number.split_once('.').unwrap_or((number, ""));
    if frac.len() > 8 || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("not an amount: {text:?} (at most eight decimals)"));
    }
    let whole: u64 = if whole.is_empty() {
        0
    } else {
        whole.parse().map_err(|_| bad())?
    };
    let frac: u64 = if frac.is_empty() {
        0
    } else {
        format!("{frac:0<8}").parse().map_err(|_| bad())?
    };
    whole
        .checked_mul(100_000_000)
        .and_then(|w| w.checked_add(frac))
        .ok_or_else(|| format!("amount too large: {text:?}"))
}

/// Conventional ZIP 317 fee for a bundle with `actions` logical actions.
pub fn zip317_fee(actions: u64) -> u64 {
    ZIP317_MARGINAL_FEE * actions.max(ZIP317_GRACE_ACTIONS)
}

#[cfg(test)]
mod tests {
    #[test]
    fn amounts_in_zatoshis_or_zec() {
        use super::parse_zat;
        assert_eq!(parse_zat("100000"), Ok(100_000));
        assert_eq!(parse_zat(" 100000 zat "), Ok(100_000));
        assert_eq!(parse_zat("0.001"), Ok(100_000));
        assert_eq!(parse_zat("0.001 ZEC"), Ok(100_000));
        assert_eq!(parse_zat("1zec"), Ok(100_000_000));
        assert_eq!(parse_zat(".5"), Ok(50_000_000));
        assert_eq!(parse_zat("2.00000001"), Ok(200_000_001));
        assert!(parse_zat("0.000000001").is_err());
        assert!(parse_zat("abc").is_err());
        assert!(parse_zat("").is_err());
    }

    use super::*;

    #[test]
    fn fee_schedule() {
        assert_eq!(zip317_fee(1), 10_000);
        assert_eq!(zip317_fee(2), 10_000);
        assert_eq!(zip317_fee(3), 15_000);
    }
}
