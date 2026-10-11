//! PCZT (ZIP 374) role helpers.
//!
//! The buyer runs Creator, IO Finalizer and Prover (see [`crate::trade`]), then *redacts*
//! before sending to the seller: the unredacted PCZT carries the buyer's full viewing key,
//! witnesses and note randomness. The seller runs Signer on the item action only. The
//! buyer combines, signs the payment action, finalizes spends and extracts the
//! transaction.

use anyhow::{anyhow, Result};
use orchard::circuit::{OrchardCircuitVersion, VerifyingKey};
use orchard::keys::SpendAuthorizingKey;
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
/// anchor, flags, value sum) and the spend randomizer `alpha` of the item action, which the
/// seller's signer needs. Removed: viewing keys, witnesses, note plaintext data, the buyer's
/// randomizers, the proof and the binding-signature key.
pub fn redact_for_seller(pczt: Pczt, item_action_index: usize) -> Result<Pczt> {
    let n = action_count(&pczt)?;
    let mut redactor = Redactor::new(pczt).redact_ironwood_with(|mut r| {
        r.clear_zkproof();
        r.clear_bsk();
        // Output-side plaintext data is the buyer's business on every action.
        r.redact_actions(|mut a| {
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

/// Sign one Ironwood action with a spend authorizing key. Fails if the key does not match
/// the action's randomized verification key.
pub fn sign_ironwood_action(pczt: Pczt, index: usize, ask: &SpendAuthorizingKey) -> Result<Pczt> {
    let mut signer = Signer::new(pczt).map_err(|e| anyhow!("signer: {e:?}"))?;
    signer
        .sign_ironwood(index, ask)
        .map_err(|e| anyhow!("signing action {index}: {e:?}"))?;
    Ok(signer.finish())
}

/// Sign every Ironwood action this key controls; returns the number of signatures made.
pub fn sign_matching_actions(pczt: Pczt, ask: &SpendAuthorizingKey) -> Result<(Pczt, usize)> {
    let n = action_count(&pczt)?;
    let mut signer = Signer::new(pczt).map_err(|e| anyhow!("signer: {e:?}"))?;
    let mut signed = 0;
    for i in 0..n {
        match signer.sign_ironwood(i, ask) {
            Ok(()) => signed += 1,
            Err(e) => {
                let msg = format!("{e:?}");
                if msg.contains("WrongSpendAuthorizingKey")
                    || msg.contains("MissingSpendAuthRandomizer")
                {
                    continue;
                }
                return Err(anyhow!("signing action {i}: {msg}"));
            }
        }
    }
    Ok((signer.finish(), signed))
}

/// Merge two PCZTs (same transaction, different filled-in fields or signatures).
pub fn combine(a: Pczt, b: Pczt) -> Result<Pczt> {
    Combiner::new(vec![a, b])
        .combine()
        .map_err(|e| anyhow!("combiner: {e:?}"))
}

/// Finalize spends and extract the transaction, verifying the proof and signatures.
pub fn extract(pczt: Pczt) -> Result<Transaction> {
    let vk = VerifyingKey::build(OrchardCircuitVersion::PostNu6_3);
    let finalized = SpendFinalizer::new(pczt)
        .finalize_spends()
        .map_err(|e| anyhow!("spend finalizer: {e:?}"))?;
    TransactionExtractor::new(finalized)
        .with_orchard(&vk)
        .extract()
        .map_err(|e| anyhow!("transaction extractor: {e:?}"))
}

/// Serialize a transaction to its network encoding.
pub fn tx_bytes(tx: &Transaction) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    tx.write(&mut out)?;
    Ok(out)
}
