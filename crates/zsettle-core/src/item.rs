//! What an item is.
//!
//! An item is an ordinary Ironwood (Orchard v3) note in a dedicated ZIP 32 account. Its
//! identity is its genesis note (`<txid>:<action index>`); its history is the chain of
//! actions that spend it, each one's output being the next note. Items MUST carry value: a
//! zero-value note is not bound to the commitment tree by the Action circuit, so nothing
//! would tie its successor to it.

/// Minimum item note value in zatoshis.
pub const MIN_ITEM_VALUE: u64 = 1_000;
