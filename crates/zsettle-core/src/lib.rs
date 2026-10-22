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

/// Default maximum time a seller signature stays valid, in 75-second reference blocks: 40,
/// about 50 minutes. That is 40 blocks before NU7 and 120 from NU7, whose blocks come every 25
/// seconds ([`reference_blocks`]): the wallet library's default transaction expiry on either
/// side ([`standard_expiry_delta`]), so a trade carries the expiry of any wallet transaction
/// and cannot be picked out on chain by it. The seller can end the option early by spending the
/// item itself.
pub const DEFAULT_MAX_EXPIRY_DELTA: u32 = 40;

/// Blocks per 75-second reference block at `height`: 1 before NU7, 3 from NU7, whose target
/// block spacing is 25 seconds.
pub fn spacing_ratio<P: zcash_protocol::consensus::Parameters>(params: &P, height: u32) -> u32 {
    use zcash_protocol::consensus::{BlockHeight, NetworkUpgrade, NU7_POW_TARGET_SPACING_RATIO};
    if params.is_nu_active(NetworkUpgrade::Nu7, BlockHeight::from(height)) {
        NU7_POW_TARGET_SPACING_RATIO
    } else {
        1
    }
}

/// A span of `reference` 75-second reference blocks, in blocks at `height`: the same duration
/// on either side of NU7.
pub fn reference_blocks<P: zcash_protocol::consensus::Parameters>(
    params: &P,
    height: u32,
    reference: u32,
) -> u32 {
    reference.saturating_mul(spacing_ratio(params, height))
}

/// The expiry delta the wallet library gives an ordinary transaction targeting `height`
/// (ZIP 203, ZIP 218): 40 blocks before NU7 and 120 from NU7, both about 50 minutes. A trade
/// and every other transaction zsettle builds uses the same.
pub fn standard_expiry_delta<P: zcash_protocol::consensus::Parameters>(
    params: &P,
    height: u32,
) -> u32 {
    use zcash_protocol::consensus::{BlockHeight, BranchId};
    zcash_primitives::transaction::builder::default_tx_expiry_delta(BranchId::for_height(
        params,
        BlockHeight::from(height),
    ))
}

#[cfg(test)]
mod spacing_tests {
    use super::*;
    use zcash_protocol::consensus::Network;

    #[test]
    fn expiry_follows_the_block_spacing() {
        // Testnet NU7 activated at 4,465,026; mainnet's is set at 3,543,405.
        for (net, nu7) in [
            (Network::TestNetwork, 4_465_026u32),
            (Network::MainNetwork, 3_543_405u32),
        ] {
            assert_eq!(standard_expiry_delta(&net, nu7 - 1), 40);
            assert_eq!(standard_expiry_delta(&net, nu7), 120);
            assert_eq!(
                reference_blocks(&net, nu7 - 1, DEFAULT_MAX_EXPIRY_DELTA),
                40
            );
            assert_eq!(reference_blocks(&net, nu7, DEFAULT_MAX_EXPIRY_DELTA), 120);
            // The default limit is exactly the standard expiry, on both sides of NU7.
            for h in [nu7 - 1, nu7, nu7 + 1_000] {
                assert_eq!(
                    reference_blocks(&net, h, DEFAULT_MAX_EXPIRY_DELTA),
                    standard_expiry_delta(&net, h)
                );
            }
        }
    }
}

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
