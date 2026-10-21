//! Ownership proofs: a holder shows anyone that they control an item note.
//!
//! The holder signs, with the item account's spend authorizing key, a statement naming the
//! item note, the item account's viewing key and a recent block: its height and its hash. No
//! one can know a block's hash before the block is mined, so the proof was made after that
//! block, and a checker accepts it only while the block is recent. That makes the proof
//! non-interactive: the holder makes it once and shares it, like an address, and it expires
//! by itself.
//!
//! Two optional fields are signed with the rest: a label the holder chooses (a name or a
//! handle), so a copy still says who made it, and a checker's code for a live check, which
//! ties the proof to that one check.
//!
//! The checker verifies the signature under the spend validating key inside the viewing key,
//! checks that the named block is on its chain with that hash and recent enough, then traces
//! the note with the same key: it must decrypt with it and still be unspent.
//!
//! The proof hands the checker the item account's viewing key, so the checker can see every
//! item that account has held and its history. ZEC balances and the payout address live in
//! other accounts and are not revealed.
//!
//! Limits: a shared proof is a bearer statement. Whoever has it can show it again until it
//! expires, so it proves "the holder of this key holds this item", not "the person in front of
//! you is that holder". A live check ties a proof to one check, and even that does not stop a
//! live relay by someone standing between the checker and the holder.

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
/// signature over anything else: a listings file, or a proof in another format.
const SIGNATURE_DOMAIN: &[u8] = b"zsettle-ownership-v2\n";

/// The proof format this build makes and reads.
pub const FORMAT_VERSION: u32 = 2;

/// Longest checker's code, in characters.
pub const MAX_CHALLENGE_CHARS: usize = 200;

/// Longest label, in characters.
pub const MAX_LABEL_CHARS: usize = 80;

/// Largest proof file a checker reads.
pub const MAX_PROOF_BYTES: u64 = 16 * 1024;

/// How many blocks after the block it names a proof is accepted by default: about 40 minutes
/// on the testnet (25-second blocks), about two hours on mainnet (75-second blocks).
pub const DEFAULT_MAX_AGE_BLOCKS: u32 = 100;

/// A signed statement that the holder of an item account controls one of its notes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
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
    /// Height of the recent block the proof is made after.
    pub height: u32,
    /// Hash of that block, as block explorers show it. It cannot be known before the block is
    /// mined, which is what makes the proof fresh.
    pub block_hash: String,
    /// A name or handle the holder chose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// A checker's code, when the proof was made for a live check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub challenge: Option<String>,
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
    height: u32,
    block_hash: &'a str,
    label: Option<&'a str>,
    challenge: Option<&'a str>,
}

/// One line of 1 to `max` characters, with no control characters.
fn check_line(text: &str, max: usize, what: &str) -> Result<()> {
    let n = text.chars().count();
    if n == 0 {
        return Err(anyhow!("the {what} is empty"));
    }
    if n > max {
        return Err(anyhow!(
            "the {what} is {n} characters long; at most {max} are allowed"
        ));
    }
    if text.chars().any(char::is_control) {
        return Err(anyhow!(
            "the {what} contains control characters (such as a newline)"
        ));
    }
    Ok(())
}

/// Check a checker's code: 1 to 200 characters, no control characters.
pub fn check_challenge(challenge: &str) -> Result<()> {
    check_line(challenge, MAX_CHALLENGE_CHARS, "challenge")
}

/// Check a label: 1 to 80 characters, no control characters.
pub fn check_label(label: &str) -> Result<()> {
    check_line(label, MAX_LABEL_CHARS, "label")
}

/// Check a block hash: 64 lower-case hex digits.
pub fn check_block_hash(hash: &str) -> Result<()> {
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(anyhow!(
            "the block hash {hash:?} is not 64 lower-case hex digits"
        ));
    }
    Ok(())
}

impl OwnershipProof {
    /// An unsigned proof for a note of the item account, made after the block `anchor`
    /// (height, hash). Call [`OwnershipProof::sign`] next.
    pub fn new(
        network: Network,
        item_fvk: &FullViewingKey,
        item_birthday: u32,
        note: NoteRef,
        anchor: (u32, &str),
        label: Option<&str>,
        challenge: Option<&str>,
    ) -> Result<Self> {
        let (height, block_hash) = anchor;
        check_block_hash(block_hash)?;
        if let Some(l) = label {
            check_label(l)?;
        }
        if let Some(c) = challenge {
            check_challenge(c)?;
        }
        Ok(OwnershipProof {
            version: FORMAT_VERSION,
            network: network_name(network).into(),
            item_ufvk: Listings::encode_item_key(item_fvk, network)?,
            item_birthday,
            note: note.to_string(),
            height,
            block_hash: block_hash.into(),
            label: label.map(str::to_string),
            challenge: challenge.map(str::to_string),
            signature: None,
        })
    }

    /// Parse a proof from its JSON text and check its shape. The signature is not checked
    /// here; call [`OwnershipProof::verify_signature`].
    pub fn parse(text: &str) -> Result<Self> {
        if text.len() as u64 > MAX_PROOF_BYTES {
            return Err(anyhow!("the proof is larger than {MAX_PROOF_BYTES} bytes"));
        }
        // Read the version on its own first, so a proof in the old format gets a plain answer
        // instead of a complaint about its fields.
        let head: serde_json::Value =
            serde_json::from_str(text).context("parsing the ownership proof")?;
        match head.get("version").and_then(serde_json::Value::as_u64) {
            Some(v) if v == u64::from(FORMAT_VERSION) => {}
            Some(1) => {
                return Err(anyhow!(
                    "this proof is in the old format, which named no block; ask the holder for a new proof"
                ))
            }
            Some(v) => return Err(anyhow!("unsupported ownership proof version {v}")),
            None => return Err(anyhow!("the ownership proof has no version")),
        }
        let p: OwnershipProof =
            serde_json::from_str(text).context("parsing the ownership proof")?;
        p.network()?;
        p.note_ref()?;
        check_block_hash(&p.block_hash)?;
        if let Some(l) = &p.label {
            check_label(l)?;
        }
        if let Some(c) = &p.challenge {
            check_challenge(c)?;
        }
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
            height: self.height,
            block_hash: &self.block_hash,
            label: self.label.as_deref(),
            challenge: self.challenge.as_deref(),
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

    fn hash() -> String {
        "0f".repeat(32)
    }

    fn sample(fvk: &FullViewingKey, challenge: Option<&str>) -> OwnershipProof {
        OwnershipProof::new(
            Network::MainNetwork,
            fvk,
            3_513_000,
            note(),
            (3_520_000, &hash()),
            Some("@alice"),
            challenge,
        )
        .unwrap()
    }

    #[test]
    fn round_trip_shared_and_live() {
        let (ask, fvk) = keys(9);
        for challenge in [None, Some("checker-42 at 10:15")] {
            let mut p = sample(&fvk, challenge);
            assert!(p.verify_signature().is_err(), "unsigned must be refused");
            p.sign(&ask).unwrap();
            p.verify_signature().unwrap();

            let back = OwnershipProof::parse(&p.to_json().unwrap()).unwrap();
            assert_eq!(back, p);
            back.verify_signature().unwrap();
            assert_eq!(back.item_fvk().unwrap(), fvk);
            assert_eq!(back.note_ref().unwrap(), note());
            assert_eq!(back.network().unwrap(), Network::MainNetwork);
            assert_eq!(back.challenge.as_deref(), challenge);
            assert_eq!(back.label.as_deref(), Some("@alice"));
        }
        // A shared proof carries no code at all, not an empty one.
        let shared = sample(&fvk, None).to_json().unwrap();
        assert!(!shared.contains("challenge"));
    }

    #[test]
    fn tampering_is_detected() {
        let (ask, fvk) = keys(9);
        let mut p = sample(&fvk, None);
        p.sign(&ask).unwrap();

        let mut t = p.clone();
        t.note = format!("{}:2", "ab".repeat(32));
        assert!(t.verify_signature().is_err());

        let mut t = p.clone();
        t.height += 1;
        assert!(t.verify_signature().is_err());

        let mut t = p.clone();
        t.block_hash = "1f".repeat(32);
        assert!(t.verify_signature().is_err(), "another block");

        let mut t = p.clone();
        t.item_birthday = 1;
        assert!(t.verify_signature().is_err());

        let mut t = p.clone();
        t.label = Some("@mallory".into());
        assert!(t.verify_signature().is_err(), "a changed label");

        let mut t = p.clone();
        t.label = None;
        assert!(t.verify_signature().is_err(), "a removed label");

        let mut t = p.clone();
        t.challenge = Some("a code added later".into());
        assert!(
            t.verify_signature().is_err(),
            "a shared proof turned into a live one"
        );

        let mut live = sample(&fvk, Some("code-1"));
        live.sign(&ask).unwrap();
        let mut t = live.clone();
        t.challenge = Some("code-2".into());
        assert!(t.verify_signature().is_err(), "another code");
        let mut t = live.clone();
        t.challenge = None;
        assert!(
            t.verify_signature().is_err(),
            "a live proof turned into a shared one"
        );
    }

    #[test]
    fn wrong_key_is_refused() {
        let (_, fvk) = keys(9);
        let (other_ask, other_fvk) = keys(10);
        let mut p = sample(&fvk, None);
        p.sign(&other_ask).unwrap();
        assert!(p.verify_signature().is_err());

        // Swapping in the signer's own viewing key after signing breaks the signature too:
        // the key is part of the signed message.
        p.item_ufvk = Listings::encode_item_key(&other_fvk, Network::MainNetwork).unwrap();
        assert!(p.verify_signature().is_err());
    }

    #[test]
    fn other_signatures_cannot_stand_in() {
        let (ask, fvk) = keys(9);
        let mut l = Listings {
            version: 1,
            network: "main".into(),
            item_ufvk: Listings::encode_item_key(&fvk, Network::MainNetwork).unwrap(),
            item_birthday: 3_513_000,
            payout_address: "u1example".into(),
            items: vec![ListedItem {
                description: None,
                icon: None,
                note: note().to_string(),
                name: "Item".into(),
                price_zat: 5_001,
                max_expiry_delta: 40,
            }],
            issued_at: 3_520_000,
            signature: None,
        };
        l.sign(&ask, 3_520_000).unwrap();
        let mut p = sample(&fvk, None);
        p.sign(&ask).unwrap();
        l.verify_signature().unwrap();
        p.verify_signature().unwrap();

        let mut replayed = p.clone();
        replayed.signature = l.signature.clone();
        assert!(replayed.verify_signature().is_err());

        let mut replayed = l.clone();
        replayed.signature = p.signature.clone();
        assert!(replayed.verify_signature().is_err());

        // The same fields under another domain (the listings one, or the old proof format)
        // do not verify either.
        for domain in [
            &b"zsettle-listings-v1\n"[..],
            &b"zsettle-ownership-v1\n"[..],
        ] {
            let mut msg = domain.to_vec();
            msg.extend(&p.signing_bytes().unwrap()[SIGNATURE_DOMAIN.len()..]);
            let mut forged = p.clone();
            forged.signature = Some(sign_message(&ask, &msg));
            assert!(forged.verify_signature().is_err());
        }
    }

    #[test]
    fn line_limits() {
        assert!(check_challenge("").is_err());
        assert!(check_challenge("a").is_ok());
        assert!(check_challenge(&"x".repeat(200)).is_ok());
        assert!(check_challenge(&"x".repeat(201)).is_err());
        // Characters, not bytes.
        assert!(check_challenge(&"é".repeat(200)).is_ok());
        assert!(check_challenge("line\nbreak").is_err());
        assert!(check_challenge("tab\there").is_err());
        assert!(check_challenge("nul\0").is_err());

        assert!(check_label("").is_err());
        assert!(check_label(&"x".repeat(80)).is_ok());
        assert!(check_label(&"x".repeat(81)).is_err());
        assert!(check_label("two\nlines").is_err());

        let (_, fvk) = keys(9);
        let h = hash();
        let make = |label, challenge| {
            OwnershipProof::new(
                Network::MainNetwork,
                &fvk,
                1,
                note(),
                (1, &h),
                label,
                challenge,
            )
        };
        assert!(make(None, Some("")).is_err());
        assert!(make(Some(""), None).is_err());
        assert!(make(None, None).is_ok());
    }

    #[test]
    fn block_hash_shape() {
        assert!(check_block_hash(&"0f".repeat(32)).is_ok());
        assert!(check_block_hash(&"0F".repeat(32)).is_err(), "upper case");
        assert!(check_block_hash(&"0f".repeat(31)).is_err(), "short");
        assert!(check_block_hash(&"zz".repeat(32)).is_err(), "not hex");
        let (_, fvk) = keys(9);
        assert!(OwnershipProof::new(
            Network::MainNetwork,
            &fvk,
            1,
            note(),
            (1, "abc"),
            None,
            None
        )
        .is_err());
    }

    #[test]
    fn parse_refuses_bad_shapes() {
        let (ask, fvk) = keys(9);
        let mut p = sample(&fvk, None);
        p.sign(&ask).unwrap();

        let mut bad = p.clone();
        bad.note = "not-a-note".into();
        assert!(OwnershipProof::parse(&bad.to_json().unwrap()).is_err());

        let mut bad = p.clone();
        bad.version = 3;
        assert!(OwnershipProof::parse(&bad.to_json().unwrap()).is_err());

        let mut bad = p.clone();
        bad.network = "regtest".into();
        assert!(OwnershipProof::parse(&bad.to_json().unwrap()).is_err());

        let mut bad = p.clone();
        bad.challenge = Some("x".repeat(201));
        assert!(OwnershipProof::parse(&bad.to_json().unwrap()).is_err());

        let mut bad = p.clone();
        bad.block_hash = "ab".into();
        assert!(OwnershipProof::parse(&bad.to_json().unwrap()).is_err());

        // A proof in the old format is refused with a plain answer.
        let old = r#"{"version":1,"network":"main","item_ufvk":"x","item_birthday":1,"note":"y","challenge":"c","height":1}"#;
        let e = OwnershipProof::parse(old).unwrap_err().to_string();
        assert!(e.contains("old format"), "{e}");

        // Unknown fields are refused.
        let mut v: serde_json::Value = serde_json::from_str(&p.to_json().unwrap()).unwrap();
        v["extra"] = serde_json::json!(1);
        assert!(OwnershipProof::parse(&v.to_string()).is_err());

        let huge = " ".repeat(MAX_PROOF_BYTES as usize + 1);
        assert!(OwnershipProof::parse(&huge).is_err());

        let unsigned = sample(&fvk, None).to_json().unwrap();
        assert!(!unsigned.contains("signature"));
        assert!(OwnershipProof::parse(&unsigned)
            .unwrap()
            .verify_signature()
            .is_err());
    }
}
