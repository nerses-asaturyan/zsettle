//! Seller-side policy: every check that must pass before the seller signs the item action.
//!
//! The checks, in order:
//! 1. the expiry height is set, lies after the current chain tip and within
//!    `max_expiry_delta` of it, and the bundle's anchor is one of the seller wallet's own
//!    recent tree roots (a proof against a synthetic tree is worthless to the seller);
//! 2. exactly one action spends the listed item note (matched by nullifier) and that
//!    action carries a spend randomizer, so it can be signed; no other action spends any
//!    note of the listed item account;
//! 3. the outputs that decrypt under the seller's incoming viewing key and are worth more
//!    than the ZIP 317 marginal fee (anything smaller is dust no wallet spends) pay at least
//!    the listed price;
//! 4. the signer itself enforces `rk == randomize(ak, alpha)` for the action it signs, so the
//!    seller can never be tricked into authorizing a spend of a different note.
//!
//! The proof is checked separately (`crate::pczt::verify_proof`) so that a forged envelope
//! cannot even obtain a signature. Many offers for one item may be signed; only one can be
//! mined, since every one of them spends the same nullifier.

use std::cell::RefCell;

use anyhow::{anyhow, Result};
use orchard::keys::PreparedIncomingViewingKey;
use orchard::note::Nullifier;
use orchard::note_encryption::{CompactAction, IronwoodDomain, COMPACT_NOTE_SIZE};
use pczt::roles::verifier::Verifier;
use pczt::Pczt;
use thiserror::Error;
use zcash_note_encryption::{try_compact_note_decryption, EphemeralKeyBytes};

use crate::ZIP317_MARGINAL_FEE;

/// Why the seller refuses to sign.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PolicyError {
    /// No action spends the listed item note.
    #[error("the trade does not spend the listed item note")]
    ItemNotSpent,
    /// More than one action spends a note of the seller.
    #[error("the trade spends the item note more than once (actions {0} and {1})")]
    DuplicateItemSpend(usize, usize),
    /// The item action has no spend randomizer, so it cannot be signed.
    #[error("item action {0} has no spend randomizer")]
    MissingAlpha(usize),
    /// An action spends another note of the listed item account.
    #[error("action {0} spends another note of the listed item account")]
    ExtraSellerSpend(usize),
    /// Payment output missing or below the listed price.
    #[error("payment {got} zat below price {want} zat")]
    Underpaid {
        /// Amount found.
        got: u64,
        /// Listed price.
        want: u64,
    },
    /// The transaction never expires (expiry height 0): the signature would be a perpetual option.
    #[error("the transaction has no expiry height; a signature would never lapse")]
    NoExpiry,
    /// The expiry height is not after the chain tip the seller sees.
    #[error("expiry height {expiry} is not after the chain tip {tip}")]
    Expired {
        /// Expiry height in the PCZT.
        expiry: u32,
        /// Current chain tip the seller sees.
        tip: u32,
    },
    /// Expiry too far in the future (free option for the buyer).
    #[error("expiry height {expiry} exceeds tip {tip} by more than {max_delta} blocks")]
    ExpiryTooLong {
        /// Expiry height in the PCZT.
        expiry: u32,
        /// Current chain tip the seller sees.
        tip: u32,
        /// Policy limit.
        max_delta: u32,
    },
    /// The transaction is for another consensus branch than the one live at the tip.
    #[error("the transaction is for consensus branch {got:#010x}, the chain is on {want:#010x}")]
    WrongBranch {
        /// Branch id in the PCZT.
        got: u32,
        /// Branch id the seller's wallet expects at the next block.
        want: u32,
    },
    /// The item action's randomizer is zero, which would put the bare spend validating key on chain.
    #[error("item action {0} has a zero spend randomizer")]
    ZeroRandomizer(usize),
    /// The bundle's anchor is not a recent tree root of the seller's wallet.
    #[error("the anchor is not a recent tree root this wallet knows")]
    UnknownAnchor,
    /// The PCZT cannot be parsed as an Ironwood bundle.
    #[error("invalid Ironwood bundle: {0}")]
    Invalid(String),
}

/// What the seller agreed to.
#[derive(Clone, Debug)]
pub struct SellerPolicy {
    /// Listed price in zatoshis.
    pub price_zat: u64,
    /// Maximum blocks between the current tip and the transaction's expiry.
    pub max_expiry_delta: u32,
    /// Current chain tip as seen by the seller's wallet.
    pub tip: u32,
    /// Consensus branch id in force at the next block.
    pub branch_id: u32,
    /// The listed payout address. Only outputs to it count as payment: another address of the
    /// same account would let a sender learn whether an address is this wallet's.
    pub payout: orchard::Address,
}

/// Result of a successful check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Approved {
    /// Index of the action the seller must sign.
    pub item_action_index: usize,
    /// Total zatoshis paid to the seller by this transaction.
    pub paid_zat: u64,
    /// Expiry height of the transaction.
    pub expiry_height: u32,
}

/// Run the seller-side checks against a (redacted) trade PCZT.
/// `seller_nullifiers` are the nullifiers of the listed item account's unspent notes, which
/// the published viewing key derives, so the whole decision depends on public data and the
/// proof: nothing in a refusal reveals what else this wallet holds.
pub fn check(
    pczt: &Pczt,
    item_nullifier: &Nullifier,
    seller_nullifiers: &[Nullifier],
    seller_ivk: &PreparedIncomingViewingKey,
    policy: &SellerPolicy,
    known_anchors: &[orchard::Anchor],
) -> Result<Approved, PolicyError> {
    inspect(
        pczt,
        item_nullifier,
        Some((seller_nullifiers, seller_ivk)),
        policy,
        known_anchors,
    )
}

/// The checks that use nothing of the seller's own: branch, expiry, anchor, and exactly one
/// spend of the listed item with a randomizer. They run before the proof is verified, so a
/// refusal at this stage tells the sender nothing the listings file does not already give.
/// The payment and the other-seller-note check wait for the proof: with a valid proof the
/// sender knows every note it spends and every note it creates, so neither can be a probe
/// built from someone else's on-chain data. `paid_zat` is zero here.
pub fn check_public(
    pczt: &Pczt,
    item_nullifier: &Nullifier,
    policy: &SellerPolicy,
    known_anchors: &[orchard::Anchor],
) -> Result<Approved, PolicyError> {
    inspect(pczt, item_nullifier, None, policy, known_anchors)
}

fn inspect(
    pczt: &Pczt,
    item_nullifier: &Nullifier,
    secrets: Option<(&[Nullifier], &PreparedIncomingViewingKey)>,
    policy: &SellerPolicy,
    known_anchors: &[orchard::Anchor],
) -> Result<Approved, PolicyError> {
    let branch = *pczt.global().consensus_branch_id();
    if branch != policy.branch_id {
        return Err(PolicyError::WrongBranch {
            got: branch,
            want: policy.branch_id,
        });
    }
    let expiry = *pczt.global().expiry_height();
    // ZIP 203: an expiry height of zero means the transaction never expires.
    if expiry == 0 {
        return Err(PolicyError::NoExpiry);
    }
    if expiry <= policy.tip {
        return Err(PolicyError::Expired {
            expiry,
            tip: policy.tip,
        });
    }
    // The buyer sets expiry relative to the next block, so allow one block of slack.
    if expiry
        > policy
            .tip
            .saturating_add(policy.max_expiry_delta)
            .saturating_add(1)
    {
        return Err(PolicyError::ExpiryTooLong {
            expiry,
            tip: policy.tip,
            max_delta: policy.max_expiry_delta,
        });
    }

    let found: RefCell<Option<Result<(usize, u64), PolicyError>>> = RefCell::new(None);
    Verifier::new(pczt.clone())
        .with_ironwood::<anyhow::Error, _>(|bundle| {
            if !known_anchors.contains(bundle.anchor()) {
                *found.borrow_mut() = Some(Err(PolicyError::UnknownAnchor));
                return Ok(());
            }
            let mut item_idx: Option<usize> = None;
            let mut paid: u64 = 0;
            for (i, action) in bundle.actions().iter().enumerate() {
                let nf = action.spend().nullifier();
                if secrets.is_some_and(|(nfs, _)| nf != item_nullifier && nfs.contains(nf)) {
                    *found.borrow_mut() = Some(Err(PolicyError::ExtraSellerSpend(i)));
                    return Ok(());
                }
                if nf == item_nullifier {
                    if let Some(prev) = item_idx {
                        *found.borrow_mut() = Some(Err(PolicyError::DuplicateItemSpend(prev, i)));
                        return Ok(());
                    }
                    match action.spend().alpha() {
                        None => {
                            *found.borrow_mut() = Some(Err(PolicyError::MissingAlpha(i)));
                            return Ok(());
                        }
                        Some(alpha) if *alpha == pasta_curves::pallas::Scalar::from(0u64) => {
                            *found.borrow_mut() = Some(Err(PolicyError::ZeroRandomizer(i)));
                            return Ok(());
                        }
                        Some(_) => {}
                    }
                    item_idx = Some(i);
                }
                let Some((_, seller_ivk)) = secrets else {
                    continue;
                };
                // Does this output pay the seller? Trial-decrypt the compact ciphertext; the
                // decryption only succeeds when the plaintext matches the committed note.
                let enc = action.output().encrypted_note();
                let mut compact = [0u8; COMPACT_NOTE_SIZE];
                compact.copy_from_slice(&enc.enc_ciphertext.as_ref()[..COMPACT_NOTE_SIZE]);
                let act = CompactAction::from_parts(
                    *action.spend().nullifier(),
                    *action.output().cmx(),
                    EphemeralKeyBytes(enc.epk_bytes),
                    compact,
                );
                let domain = IronwoodDomain::for_compact_action(&act);
                if let Some((note, addr)) = try_compact_note_decryption(&domain, seller_ivk, &act) {
                    if addr != policy.payout {
                        continue;
                    }
                    // Dust never counts: a note at or below the marginal fee costs more to
                    // spend than it is worth, so it is not a payment.
                    let value = note.value().inner();
                    if value > ZIP317_MARGINAL_FEE {
                        paid = paid.saturating_add(value);
                    }
                }
            }
            *found.borrow_mut() = Some(match item_idx {
                Some(i) => Ok((i, paid)),
                None => Err(PolicyError::ItemNotSpent),
            });
            Ok(())
        })
        .map_err(|e| PolicyError::Invalid(format!("{e:?}")))?;

    let (item_action_index, paid_zat) = found
        .into_inner()
        .ok_or_else(|| PolicyError::Invalid("no Ironwood bundle".into()))??;
    if secrets.is_some() && paid_zat < policy.price_zat {
        return Err(PolicyError::Underpaid {
            got: paid_zat,
            want: policy.price_zat,
        });
    }
    Ok(Approved {
        item_action_index,
        paid_zat,
        expiry_height: expiry,
    })
}

/// Convenience: build a prepared incoming viewing key from a full viewing key.
pub fn prepared_ivk(fvk: &orchard::keys::FullViewingKey) -> PreparedIncomingViewingKey {
    PreparedIncomingViewingKey::new(&fvk.to_ivk(orchard::keys::Scope::External))
}

/// Human-readable summary of a policy result for logs and the CLI.
pub fn describe(result: &Result<Approved, PolicyError>) -> String {
    match result {
        Ok(a) => format!(
            "approved: sign action {} (paid {} zat, expires at {})",
            a.item_action_index, a.paid_zat, a.expiry_height
        ),
        Err(e) => format!("refused: {e}"),
    }
}

/// Wrap an arbitrary error for callers that want `anyhow`.
pub fn into_anyhow(e: PolicyError) -> anyhow::Error {
    anyhow!("{e}")
}
