//! PCZT (ZIP 374) role helpers.
//!
//! The buyer runs Creator, Constructor, IO Finalizer and Prover, then
//! *redacts* before sending to the seller (the unredacted PCZT carries the buyer's full
//! viewing key, witness and rseed). The seller runs Signer on the item action only.
//! The buyer runs Signer on the payment action, Combiner, Spend Finalizer and
//! Transaction Extractor.

/// Transport-neutral container for a PCZT in flight between buyer and seller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TradeEnvelope {
    /// Serialized PCZT bytes (postcard encoding as produced by the `pczt` crate).
    pub pczt: Vec<u8>,
    /// Whether redaction has been applied. Sellers must refuse `false`.
    pub redacted: bool,
}
