//! Ownership proofs: a holder shows a checker that they control an item note right now.
//!
//! The checker picks a challenge, any fresh text. The holder signs it, together with the note
//! reference, the item account's viewing key and the current chain height, with the item
//! account's spend authorizing key. The checker verifies the signature under the spend
//! validating key inside that viewing key, then traces the note on chain with the same key:
//! the note must decrypt with it and still be unspent.
//!
//! The proof hands the checker the item account's viewing key, so the checker can see every
//! item that account has held and its history. ZEC balances and the payout address live in
//! other accounts and are not revealed.

use anyhow::{anyhow, Context, Result};
use orchard::keys::{FullViewingKey, SpendAuthorizingKey};
use serde::{Deserialize, Serialize};
use zcash_protocol::consensus::Network;

use crate::listing::{
    decode_item_key, network_from_name, network_name, sign_message, verify_message, Listings,
    SignatureError,
};
use crate::wallet::NoteRef;

/// Prefix of the signed message, so an ownership signature can never be mistaken for a
/// signature over anything else, a listings file included.
const SIGNATURE_DOMAIN: &[u8] = b"zsettle-ownership-v1\n";

/// Longest challenge, in characters.
pub const MAX_CHALLENGE_CHARS: usize = 200;

/// Largest proof file a checker reads.
pub const MAX_PROOF_BYTES: u64 = 16 * 1024;

/// A signed statement that the holder of an item account controls one of its notes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OwnershipProof {
    /// Format version.
    pub version: u32,
    /// Network name (`main` or `test`).
    pub network: String,
    /// Unified full viewing key of the item account, Orchard component only.
    pub item_ufvk: String,
    /// Height from which a checker must scan to see the account's notes.
    pub item_birthday: u32,
    /// The item note, as `<txid>:<output index>`.
    pub note: String,
    /// The text the checker chose.
    pub challenge: String,
    /// Chain height the holder's wallet had reached when signing.
    pub height: u32,
    /// RedPallas signature over every other field, by the item account's spend key (hex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

/// The signed part of a proof, serialized in a fixed field order.
#[derive(Serialize)]
struct Unsigned<'a> {
    version: u32,
    network: &'a str,
    item_ufvk: &'a str,
    item_birthday: u32,
    note: &'a str,
    challenge: &'a str,
    height: u32,
}

/// Check a challenge: 1 to 200 characters, no control characters.
pub fn check_challenge(challenge: &str) -> Result<()> {
    let n = challenge.chars().count();
    if n == 0 {
        return Err(anyhow!("the challenge is empty"));
    }
    if n > MAX_CHALLENGE_CHARS {
        return Err(anyhow!(
            "the challenge is {n} characters long; at most {MAX_CHALLENGE_CHARS} are allowed"
        ));
    }
    if challenge.chars().any(char::is_control) {
        return Err(anyhow!(
            "the challenge contains control characters (such as a newline)"
        ));
    }
    Ok(())
}

impl OwnershipProof {
    /// An unsigned proof for a note of the item account. Call [`OwnershipProof::sign`] next.
    pub fn new(
        network: Network,
        item_fvk: &FullViewingKey,
        item_birthday: u32,
        note: NoteRef,
        challenge: &str,
        height: u32,
    ) -> Result<Self> {
        check_challenge(challenge)?;
        Ok(OwnershipProof {
            version: 1,
            network: network_name(network).into(),
            item_ufvk: Listings::encode_item_key(item_fvk, network)?,
            item_birthday,
            note: note.to_string(),
            challenge: challenge.into(),
            height,
            signature: None,
        })
    }

    /// Parse a proof from its JSON text and check its shape. The signature is not checked
    /// here; call [`OwnershipProof::verify_signature`].
    pub fn parse(text: &str) -> Result<Self> {
        if text.len() as u64 > MAX_PROOF_BYTES {
            return Err(anyhow!("the proof is larger than {MAX_PROOF_BYTES} bytes"));
        }
        let p: OwnershipProof =
            serde_json::from_str(text).context("parsing the ownership proof")?;
        if p.version != 1 {
            return Err(anyhow!("unsupported ownership proof version {}", p.version));
        }
        p.network()?;
        p.note_ref()?;
        check_challenge(&p.challenge)?;
        Ok(p)
    }

    /// The proof as pretty JSON, the form written to files and printed.
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// The network the proof is for.
    pub fn network(&self) -> Result<Network> {
        network_from_name(&self.network)
    }

    /// The item account's Orchard full viewing key.
    pub fn item_fvk(&self) -> Result<FullViewingKey> {
        decode_item_key(self.network()?, &self.item_ufvk)
    }

    /// The note the proof is about.
    pub fn note_ref(&self) -> Result<NoteRef> {
        self.note
            .parse()
            .with_context(|| format!("the proof's note {:?} is not <txid>:<index>", self.note))
    }

    fn signing_bytes(&self) -> Result<Vec<u8>> {
        let unsigned = Unsigned {
            version: self.version,
            network: &self.network,
            item_ufvk: &self.item_ufvk,
            item_birthday: self.item_birthday,
            note: &self.note,
            challenge: &self.challenge,
            height: self.height,
        };
        let mut msg = SIGNATURE_DOMAIN.to_vec();
        msg.extend(serde_json::to_vec(&unsigned)?);
        Ok(msg)
    }

    /// Sign with the item account's spend authorizing key. The signature verifies under the
    /// account's spend validating key `ak`, which the viewing key in the proof carries.
    pub fn sign(&mut self, ask: &SpendAuthorizingKey) -> Result<()> {
        self.signature = Some(sign_message(ask, &self.signing_bytes()?));
        Ok(())
    }

    /// Check the signature against the spend validating key inside `item_ufvk`.
    pub fn verify_signature(&self) -> Result<()> {
        let sig_hex = self
            .signature
            .as_deref()
            .ok_or_else(|| anyhow!("the ownership proof is not signed"))?;
        verify_message(
            &self.item_fvk()?,
            sig_hex,
            &self.signing_bytes()?,
            "ownership proof",
        )
        .map_err(|e| match e {
            SignatureError::Malformed(e) => e,
            SignatureError::Mismatch => anyhow!("the ownership proof signature does not verify: the proof was altered, or it was not signed by the key that controls this item account"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::listing::ListedItem;
    use orchard::keys::SpendingKey;

    fn keys(seed: u8) -> (SpendAuthorizingKey, FullViewingKey) {
        let sk = SpendingKey::from_bytes([seed; 32]).unwrap();
        (SpendAuthorizingKey::from(&sk), FullViewingKey::from(&sk))
    }

    fn note() -> NoteRef {
        (format!("{}:1", "ab".repeat(32))).parse().unwrap()
    }

    fn sample(fvk: &FullViewingKey) -> OwnershipProof {
        OwnershipProof::new(
            Network::MainNetwork,
            fvk,
            3_513_000,
            note(),
            "checker-42 at 10:15",
            3_520_000,
        )
        .unwrap()
    }

    #[test]
    fn round_trip() {
        let (ask, fvk) = keys(9);
        let mut p = sample(&fvk);
        assert!(p.verify_signature().is_err(), "unsigned must be refused");
        p.sign(&ask).unwrap();
        p.verify_signature().unwrap();

        let back = OwnershipProof::parse(&p.to_json().unwrap()).unwrap();
        assert_eq!(back, p);
        back.verify_signature().unwrap();
        assert_eq!(back.item_fvk().unwrap(), fvk);
        assert_eq!(back.note_ref().unwrap(), note());
        assert_eq!(back.network().unwrap(), Network::MainNetwork);
    }

    #[test]
    fn tampering_is_detected() {
        let (ask, fvk) = keys(9);
        let mut p = sample(&fvk);
        p.sign(&ask).unwrap();

        let mut other_note = p.clone();
        other_note.note = format!("{}:2", "ab".repeat(32));
        assert!(other_note.verify_signature().is_err());

        let mut other_challenge = p.clone();
        other_challenge.challenge = "another checker".into();
        assert!(other_challenge.verify_signature().is_err());

        let mut other_height = p.clone();
        other_height.height += 1;
        assert!(other_height.verify_signature().is_err());

        let mut other_birthday = p.clone();
        other_birthday.item_birthday = 1;
        assert!(other_birthday.verify_signature().is_err());
    }

    #[test]
    fn wrong_key_is_refused() {
        let (_, fvk) = keys(9);
        let (other_ask, other_fvk) = keys(10);
        let mut p = sample(&fvk);
        p.sign(&other_ask).unwrap();
        assert!(p.verify_signature().is_err());

        // Swapping in the signer's own viewing key after signing breaks the signature too:
        // the key is part of the signed message.
        p.item_ufvk = Listings::encode_item_key(&other_fvk, Network::MainNetwork).unwrap();
        assert!(p.verify_signature().is_err());
    }

    #[test]
    fn listings_and_proofs_cannot_stand_in_for_each_other() {
        let (ask, fvk) = keys(9);
        let mut l = Listings {
            version: 1,
            network: "main".into(),
            item_ufvk: Listings::encode_item_key(&fvk, Network::MainNetwork).unwrap(),
            item_birthday: 3_513_000,
            payout_address: "u1example".into(),
            items: vec![ListedItem {
                note: note().to_string(),
                name: "Item".into(),
                price_zat: 5_001,
                max_expiry_delta: 40,
            }],
            issued_at: 3_520_000,
            signature: None,
        };
        l.sign(&ask, 3_520_000).unwrap();
        let mut p = sample(&fvk);
        p.sign(&ask).unwrap();
        l.verify_signature().unwrap();
        p.verify_signature().unwrap();

        let mut replayed = p.clone();
        replayed.signature = l.signature.clone();
        assert!(replayed.verify_signature().is_err());

        let mut replayed = l.clone();
        replayed.signature = p.signature.clone();
        assert!(replayed.verify_signature().is_err());

        // The same fields under the other domain do not verify either.
        let mut msg = b"zsettle-listings-v1\n".to_vec();
        msg.extend(&p.signing_bytes().unwrap()[SIGNATURE_DOMAIN.len()..]);
        let cross = sign_message(&ask, &msg);
        let mut forged = p.clone();
        forged.signature = Some(cross);
        assert!(forged.verify_signature().is_err());
    }

    #[test]
    fn challenge_limits() {
        assert!(check_challenge("").is_err());
        assert!(check_challenge("a").is_ok());
        assert!(check_challenge(&"x".repeat(200)).is_ok());
        assert!(check_challenge(&"x".repeat(201)).is_err());
        // Characters, not bytes.
        assert!(check_challenge(&"é".repeat(200)).is_ok());
        assert!(check_challenge("line\nbreak").is_err());
        assert!(check_challenge("tab\there").is_err());
        assert!(check_challenge("nul\0").is_err());
        let (_, fvk) = keys(9);
        assert!(OwnershipProof::new(Network::MainNetwork, &fvk, 1, note(), "", 1).is_err());
    }

    #[test]
    fn parse_refuses_bad_shapes() {
        let (ask, fvk) = keys(9);
        let mut p = sample(&fvk);
        p.sign(&ask).unwrap();

        let mut bad_note = p.clone();
        bad_note.note = "not-a-note".into();
        assert!(OwnershipProof::parse(&bad_note.to_json().unwrap()).is_err());

        let mut bad_version = p.clone();
        bad_version.version = 2;
        assert!(OwnershipProof::parse(&bad_version.to_json().unwrap()).is_err());

        let mut bad_network = p.clone();
        bad_network.network = "regtest".into();
        assert!(OwnershipProof::parse(&bad_network.to_json().unwrap()).is_err());

        let mut long_challenge = p.clone();
        long_challenge.challenge = "x".repeat(201);
        assert!(OwnershipProof::parse(&long_challenge.to_json().unwrap()).is_err());

        let huge = " ".repeat(MAX_PROOF_BYTES as usize + 1);
        assert!(OwnershipProof::parse(&huge).is_err());

        let unsigned = sample(&fvk).to_json().unwrap();
        assert!(!unsigned.contains("signature"));
        assert!(OwnershipProof::parse(&unsigned)
            .unwrap()
            .verify_signature()
            .is_err());
    }
}
