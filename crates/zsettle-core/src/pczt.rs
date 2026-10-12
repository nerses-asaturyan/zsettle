//! PCZT (ZIP 374) role helpers.
//!
//! The buyer runs Creator, IO Finalizer and Prover (see [`crate::trade`]), then *redacts*
//! before sending to the seller: the unredacted PCZT carries the buyer's full viewing key,
//! witnesses, note randomness and value-commitment trapdoors. The seller verifies the proof,
//! runs the policy and signs the item action only. The buyer combines, signs the payment
//! action, finalizes spends and extracts the transaction.

use std::cell::RefCell;

use anyhow::{anyhow, Result};
use blake2b_simd::Params;
use orchard::circuit::{Instance, OrchardCircuitVersion, VerifyingKey};
use orchard::keys::SpendAuthorizingKey;
use orchard::pczt::SignerError;
use pczt::roles::{
    combiner::Combiner, redactor::Redactor, signer::Signer, spend_finalizer::SpendFinalizer,
    tx_extractor::TransactionExtractor, verifier::Verifier,
};
use pczt::Pczt;
use zcash_primitives::transaction::Transaction;

/// Serialize a PCZT to bytes.
pub fn to_bytes(pczt: &Pczt) -> Result<Vec<u8>> {
    pczt.clone()
        .serialize()
        .map_err(|e| anyhow!("encoding PCZT: {e:?}"))
}

/// Parse a PCZT from bytes.
pub fn from_bytes(bytes: &[u8]) -> Result<Pczt> {
    Pczt::parse(bytes).map_err(|e| anyhow!("invalid PCZT: {e:?}"))
}

/// Content tag of an envelope (first 16 bytes of a personalised BLAKE2b-256; not keyed, so anyone holding the envelope bytes can compute it, as hex). Used to
/// name the seller's reply after the envelope it answers, so two envelopes with the same file
/// name never collide and the same envelope always maps to the same reply.
pub fn envelope_tag(bytes: &[u8]) -> String {
    let hash = Params::new()
        .hash_length(32)
        .personal(b"zsettle_envelope")
        .hash(bytes);
    hex::encode(&hash.as_bytes()[..16])
}

/// Number of actions in the Ironwood bundle.
pub fn action_count(pczt: &Pczt) -> Result<usize> {
    let mut n = 0usize;
    Verifier::new(pczt.clone())
        .with_ironwood::<anyhow::Error, _>(|bundle| {
            n = bundle.actions().len();
            Ok(())
        })
        .map_err(|e| anyhow!("invalid Ironwood bundle: {e:?}"))?;
    Ok(n)
}

/// Nullifiers of every action's spend, in action order.
pub fn action_nullifiers(pczt: &Pczt) -> Result<Vec<orchard::note::Nullifier>> {
    let mut out = Vec::new();
    Verifier::new(pczt.clone())
        .with_ironwood::<anyhow::Error, _>(|bundle| {
            out = bundle
                .actions()
                .iter()
                .map(|a| *a.spend().nullifier())
                .collect();
            Ok(())
        })
        .map_err(|e| anyhow!("invalid Ironwood bundle: {e:?}"))?;
    Ok(out)
}

/// Strip everything the seller does not need to check and sign the trade.
///
/// Kept: the fields that determine the sighash (nullifiers, rk, cmx, ciphertexts, cv_net,
/// anchor, flags, value sum), the proof (public data; the seller verifies it before
/// signing) and the spend randomizer `alpha` of the item action, which the seller's signer
/// needs. Removed: viewing keys, witnesses, note plaintext data, the buyer's randomizers,
/// the value-commitment trapdoors (with them the seller could solve for every value in the
/// bundle) and the binding-signature key.
pub fn redact_for_seller(pczt: Pczt, item_action_index: usize) -> Result<Pczt> {
    let n = action_count(&pczt)?;
    let mut redactor = Redactor::new(pczt).redact_ironwood_with(|mut r| {
        r.clear_bsk();
        // Output-side plaintext data and the value trapdoor are the buyer's business on
        // every action, the item action included.
        r.redact_actions(|mut a| {
            a.clear_rcv();
            a.clear_output_rseed();
            a.clear_output_recipient();
            a.clear_output_value();
            a.clear_output_ock();
            a.clear_output_zip32_derivation();
            a.clear_output_user_address();
            a.clear_spend_witness();
            a.clear_spend_zip32_derivation();
            a.clear_spend_dummy_sk();
            a.clear_spend_proprietary();
            a.clear_output_proprietary();
        });
    });
    // The item action keeps its spend fields: they describe the seller's own note and the
    // signer verifies the nullifier and `rk` against them before signing. Every other
    // action loses its viewing key, note data and randomizer.
    for i in 0..n {
        if i != item_action_index {
            redactor = redactor.redact_ironwood_with(|mut r| {
                r.redact_action(i, |mut a| {
                    a.clear_spend_fvk();
                    a.clear_spend_rseed();
                    a.clear_spend_recipient();
                    a.clear_spend_value();
                    a.clear_spend_rho();
                    a.clear_spend_alpha();
                })
            });
        }
    }
    Ok(redactor.finish())
}

/// Verify the Ironwood proof a (possibly redacted) PCZT carries against the public inputs
/// of its actions: anchor, value commitments, nullifiers, randomized keys and commitments.
/// A valid proof shows that every spent note sits in the tree at the anchor and that the
/// values balance; a forged envelope cannot have one.
pub fn verify_proof(pczt: &Pczt, vk: &VerifyingKey) -> Result<()> {
    let outcome: RefCell<Option<Result<()>>> = RefCell::new(None);
    Verifier::new(pczt.clone())
        .with_ironwood::<anyhow::Error, _>(|bundle| {
            *outcome.borrow_mut() = Some(verify_bundle_proof(bundle, vk));
            Ok(())
        })
        .map_err(|e| anyhow!("invalid Ironwood bundle: {e:?}"))?;
    outcome
        .into_inner()
        .unwrap_or_else(|| Err(anyhow!("the envelope has no Ironwood bundle")))
}

fn verify_bundle_proof(bundle: &orchard::pczt::Bundle, vk: &VerifyingKey) -> Result<()> {
    let proof = bundle
        .zkproof()
        .as_ref()
        .ok_or_else(|| anyhow!("the envelope carries no proof"))?;
    let instances = bundle
        .actions()
        .iter()
        .map(|a| {
            Instance::from_parts(
                *bundle.anchor(),
                a.cv_net().clone(),
                *a.spend().nullifier(),
                a.spend().rk().clone(),
                *a.output().cmx(),
                *bundle.flags(),
            )
            .ok_or_else(|| anyhow!("an action carries an identity randomized key"))
        })
        .collect::<Result<Vec<_>>>()?;
    proof
        .verify(vk, &instances)
        .map_err(|e| anyhow!("the proof does not verify: {e:?}"))
}

/// The verifying key of the circuit this build targets. Building it takes a few seconds;
/// long-running callers keep one.
pub fn verifying_key() -> VerifyingKey {
    VerifyingKey::build(OrchardCircuitVersion::PostNu6_3)
}

/// Sign one Ironwood action with a spend authorizing key. Fails if the key does not match
/// the action's randomized verification key.
pub fn sign_ironwood_action(pczt: Pczt, index: usize, ask: &SpendAuthorizingKey) -> Result<Pczt> {
    let mut signer = Signer::new(pczt).map_err(|e| anyhow!("signer: {e:?}"))?;
    signer
        .sign_ironwood(rand::rng(), index, ask)
        .map_err(|e| anyhow!("signing action {index}: {e:?}"))?;
    Ok(signer.finish())
}

/// Sign every Ironwood action this key controls; returns the number of signatures made.
pub fn sign_matching_actions(pczt: Pczt, ask: &SpendAuthorizingKey) -> Result<(Pczt, usize)> {
    use pczt::roles::signer::Error as SignError;
    let n = action_count(&pczt)?;
    let mut signer = Signer::new(pczt).map_err(|e| anyhow!("signer: {e:?}"))?;
    let mut signed = 0;
    for i in 0..n {
        match signer.sign_ironwood(rand::rng(), i, ask) {
            Ok(()) => signed += 1,
            Err(SignError::IronwoodSign(
                SignerError::WrongSpendAuthorizingKey | SignerError::MissingSpendAuthRandomizer,
            )) => continue,
            Err(e) => return Err(anyhow!("signing action {i}: {e:?}")),
        }
    }
    Ok((signer.finish(), signed))
}

/// Merge two PCZTs (same transaction, different filled-in fields or signatures). Fields
/// present in both with different values are a conflict and fail the merge.
pub fn combine(a: Pczt, b: Pczt) -> Result<Pczt> {
    Combiner::new(vec![a, b])
        .combine()
        .map_err(|e| anyhow!("combiner: {e:?}"))
}

/// Finalize spends and extract the transaction, verifying the proof and signatures.
pub fn extract(pczt: Pczt) -> Result<Transaction> {
    let vk = verifying_key();
    let finalized = SpendFinalizer::new(pczt)
        .finalize_spends()
        .map_err(|e| anyhow!("spend finalizer: {e:?}"))?;
    TransactionExtractor::new(finalized)
        .with_orchard(&vk)
        .extract(rand::rng())
        .map_err(|e| anyhow!("transaction extractor: {e:?}"))
}

/// Serialize a transaction to its network encoding.
pub fn tx_bytes(tx: &Transaction) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    tx.write(&mut out)?;
    Ok(out)
}
