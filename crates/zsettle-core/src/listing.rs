//! Listings: the public file a seller publishes.
//!
//! It carries everything a buyer needs and nothing more: the item account's Orchard full
//! viewing key (the consignment, and only that component), a payout address derived for
//! the market, and one entry per item with its note reference, price and expiry policy.
//! Prices are public by design; the seller's other accounts stay invisible because items
//! live in their own ZIP 32 account.
//!
//! The file is signed with the item account's spend authorizing key and verified against
//! the spend validating key inside the published viewing key, so a buyer who holds the
//! file knows it was written by whoever controls the items in it. Buyers refuse unsigned
//! or altered files.

use std::{fs, path::Path};

use anyhow::{anyhow, Context, Result};
use orchard::keys::{FullViewingKey, SpendAuthorizingKey};
use orchard::primitives::redpallas::{Signature, SpendAuth, VerificationKey};
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use zcash_address::unified::{Encoding, Fvk, Ufvk};
use zcash_keys::keys::UnifiedFullViewingKey;
use zcash_protocol::consensus::{Network, Parameters};

use crate::wallet::NoteRef;

/// Prefix of the signed message, so a listings signature can never be mistaken for a
/// signature over anything else.
const SIGNATURE_DOMAIN: &[u8] = b"zsettle-listings-v1\n";

/// One item for sale.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListedItem {
    /// The item note, as `<txid>:<output index>`.
    pub note: String,
    /// Display name.
    #[serde(default)]
    pub name: String,
    /// Price in zatoshis.
    pub price_zat: u64,
    /// Maximum blocks the seller's signature may stay valid.
    pub max_expiry_delta: u32,
    /// A short description for buyers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// One emoji shown as the item's picture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

/// Longest item description, in characters.
pub const MAX_DESCRIPTION_CHARS: usize = 280;

impl ListedItem {
    /// Parse the note reference.
    pub fn note_ref(&self) -> Result<NoteRef> {
        self.note.parse()
    }

    /// Check the description and icon: plain text of limited length, and an icon that is one
    /// emoji (a few non-ASCII characters), never text or markup.
    pub fn check_details(&self) -> Result<()> {
        check_details(self.description.as_deref(), self.icon.as_deref())
    }
}

/// See [`ListedItem::check_details`].
pub fn check_details(description: Option<&str>, icon: Option<&str>) -> Result<()> {
    if let Some(d) = description {
        if d.trim().is_empty() || d.chars().count() > MAX_DESCRIPTION_CHARS {
            return Err(anyhow!(
                "a description has 1 to {MAX_DESCRIPTION_CHARS} characters"
            ));
        }
        if d.chars().any(|c| c.is_control()) {
            return Err(anyhow!("a description is one line of plain text"));
        }
    }
    if let Some(i) = icon {
        let n = i.chars().count();
        if n == 0
            || n > 8
            || i.chars()
                .any(|c| c.is_ascii() || c.is_control() || c.is_whitespace())
        {
            return Err(anyhow!("an icon is one emoji"));
        }
    }
    Ok(())
}

/// A seller's published listings.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Listings {
    /// Format version.
    pub version: u32,
    /// Network name (`main` or `test`).
    pub network: String,
    /// Unified full viewing key of the item account, Orchard component only (the consignment).
    pub item_ufvk: String,
    /// Height from which a buyer must scan to see the item notes.
    pub item_birthday: u32,
    /// Seller's unified address for payments.
    pub payout_address: String,
    /// Items for sale.
    pub items: Vec<ListedItem>,
    /// Chain height at which the seller signed this version of the file.
    #[serde(default)]
    pub issued_at: u32,
    /// RedPallas signature over every other field, by the item account's spend key (hex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

/// The signed part of a listings file, serialized in a fixed field order.
#[derive(Serialize)]
struct Unsigned<'a> {
    version: u32,
    network: &'a str,
    item_ufvk: &'a str,
    item_birthday: u32,
    payout_address: &'a str,
    items: &'a [ListedItem],
    issued_at: u32,
}

impl Listings {
    /// Read a listings file. The signature is not checked here; call [`Listings::verify_signature`].
    pub fn read(path: &Path) -> Result<Self> {
        let text =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text, &path.display().to_string())
    }

    /// Parse listings from their JSON text; `what` names the source in error messages.
    pub fn parse(text: &str, what: &str) -> Result<Self> {
        let l: Listings = serde_json::from_str(text).with_context(|| format!("parsing {what}"))?;
        if l.version != 1 {
            return Err(anyhow!("unsupported listings version {}", l.version));
        }
        Ok(l)
    }

    /// Read a listings file and verify its signature.
    pub fn read_verified(path: &Path) -> Result<Self> {
        let l = Self::read(path)?;
        l.verify_signature()
            .with_context(|| format!("{} is not trustworthy", path.display()))?;
        Ok(l)
    }

    /// Write a listings file. The file is replaced atomically, so a reader never sees a
    /// partial write and a crash mid-write leaves the previous version in place.
    pub fn write(&self, path: &Path) -> Result<()> {
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(self)?)
            .with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
    }

    /// The network the file is for.
    pub fn network(&self) -> Result<Network> {
        match self.network.as_str() {
            "main" => Ok(Network::MainNetwork),
            "test" => Ok(Network::TestNetwork),
            other => Err(anyhow!("unknown network {other}")),
        }
    }

    /// The item account's Orchard full viewing key.
    pub fn item_fvk(&self) -> Result<FullViewingKey> {
        let network = self.network()?;
        UnifiedFullViewingKey::decode(&network, &self.item_ufvk)
            .map_err(|e| anyhow!("decoding item viewing key: {e}"))?
            .orchard()
            .cloned()
            .ok_or_else(|| anyhow!("the item viewing key has no Orchard component"))
    }

    /// Encode an Orchard full viewing key as a unified viewing key with that single
    /// component. Publishing the account's complete unified key would also hand out its
    /// Sapling and transparent keys, which the consignment does not need.
    pub fn encode_item_key(fvk: &FullViewingKey, network: Network) -> Result<String> {
        let ufvk = Ufvk::try_from_items(vec![Fvk::Orchard(fvk.to_bytes())])
            .map_err(|e| anyhow!("encoding viewing key: {e}"))?;
        Ok(ufvk.encode(&network.network_type()))
    }

    fn signing_bytes(&self) -> Result<Vec<u8>> {
        let unsigned = Unsigned {
            version: self.version,
            network: &self.network,
            item_ufvk: &self.item_ufvk,
            item_birthday: self.item_birthday,
            payout_address: &self.payout_address,
            items: &self.items,
            issued_at: self.issued_at,
        };
        let mut msg = SIGNATURE_DOMAIN.to_vec();
        msg.extend(serde_json::to_vec(&unsigned)?);
        Ok(msg)
    }

    /// Sign the file with the item account's spend authorizing key. The signature verifies
    /// under the account's spend validating key `ak`, which every holder of the viewing key
    /// has.
    pub fn sign(&mut self, ask: &SpendAuthorizingKey, issued_at: u32) -> Result<()> {
        self.issued_at = issued_at;
        // Randomizer zero: the signing key is `ask` itself and the verification key is `ak`.
        let key = ask.randomize(&pasta_curves::pallas::Scalar::from(0u64));
        let sig = key.sign(OsRng, &self.signing_bytes()?);
        self.signature = Some(hex::encode(<[u8; 64]>::from(&sig)));
        Ok(())
    }

    /// Check the signature against the spend validating key inside `item_ufvk`.
    pub fn verify_signature(&self) -> Result<()> {
        let sig_hex = self
            .signature
            .as_deref()
            .ok_or_else(|| anyhow!("the listings file is not signed"))?;
        let bytes: [u8; 64] = hex::decode(sig_hex)
            .context("listings signature is not hex")?
            .try_into()
            .map_err(|_| anyhow!("listings signature must be 64 bytes"))?;
        let fvk = self.item_fvk()?;
        let ak: [u8; 32] = fvk.to_bytes()[..32]
            .try_into()
            .expect("a full viewing key starts with the 32-byte spend validating key");
        let vk = VerificationKey::<SpendAuth>::try_from(ak)
            .map_err(|_| anyhow!("the item viewing key has an invalid spend validating key"))?;
        vk.verify(&self.signing_bytes()?, &Signature::from(bytes))
            .map_err(|_| anyhow!("the listings signature does not verify: the file was altered, or it was not signed by the key that controls these items"))
    }

    /// Short fingerprint of the item viewing key, for buyers to compare out of band: the
    /// signature only proves the file was written by whoever controls these items.
    pub fn key_fingerprint(&self) -> String {
        let hash = blake2b_simd::Params::new()
            .hash_length(32)
            .personal(b"zsettle_listkey_")
            .hash(self.item_ufvk.as_bytes());
        hex::encode(&hash.as_bytes()[..6])
    }

    /// Find an item by note reference string.
    pub fn find(&self, note: &str) -> Option<&ListedItem> {
        self.items.iter().find(|i| i.note == note)
    }

    /// Add or replace an item. The signature becomes stale; sign again before writing.
    pub fn upsert(&mut self, item: ListedItem) {
        if let Some(existing) = self.items.iter_mut().find(|i| i.note == item.note) {
            *existing = item;
        } else {
            self.items.push(item);
        }
        self.signature = None;
    }

    /// Remove an item (after it sold). The signature becomes stale; sign again before writing.
    pub fn remove(&mut self, note: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|i| i.note != note);
        let changed = self.items.len() != before;
        if changed {
            self.signature = None;
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchard::keys::SpendingKey;

    fn keys(seed: u8) -> (SpendAuthorizingKey, FullViewingKey) {
        let sk = SpendingKey::from_bytes([seed; 32]).unwrap();
        (SpendAuthorizingKey::from(&sk), FullViewingKey::from(&sk))
    }

    fn sample(fvk: &FullViewingKey) -> Listings {
        Listings {
            version: 1,
            network: "main".into(),
            item_ufvk: Listings::encode_item_key(fvk, Network::MainNetwork).unwrap(),
            item_birthday: 3_513_947,
            payout_address: "u1example".into(),
            items: vec![ListedItem {
                description: None,
                icon: None,
                note: "00".repeat(32) + ":1",
                name: "Item".into(),
                price_zat: 5_001,
                max_expiry_delta: 40,
            }],
            issued_at: 0,
            signature: None,
        }
    }

    #[test]
    fn orchard_only_key_round_trips() {
        let (_, fvk) = keys(9);
        let l = sample(&fvk);
        assert!(l.item_ufvk.starts_with("uview1"));
        assert_eq!(l.item_fvk().unwrap(), fvk);
    }

    #[test]
    fn signature_round_trip_and_tamper_detection() {
        let (ask, fvk) = keys(9);
        let mut l = sample(&fvk);
        assert!(l.verify_signature().is_err(), "unsigned must be refused");
        l.sign(&ask, 1).unwrap();
        l.verify_signature().unwrap();

        let text = serde_json::to_string(&l).unwrap();
        let back: Listings = serde_json::from_str(&text).unwrap();
        back.verify_signature().unwrap();

        let mut tampered = l.clone();
        tampered.payout_address = "u1attacker".into();
        assert!(tampered.verify_signature().is_err());

        let mut repriced = l.clone();
        repriced.items[0].price_zat = 1;
        assert!(repriced.verify_signature().is_err());

        let (other_ask, _) = keys(10);
        let mut wrong_key = l.clone();
        wrong_key.sign(&other_ask, 1).unwrap();
        assert!(wrong_key.verify_signature().is_err());
    }

    #[test]
    fn edits_invalidate_the_signature_until_resigned() {
        let (ask, fvk) = keys(9);
        let mut l = sample(&fvk);
        l.sign(&ask, 1).unwrap();
        let note = l.items[0].note.clone();
        assert!(l.remove(&note));
        assert!(l.signature.is_none());
        l.sign(&ask, 1).unwrap();
        l.verify_signature().unwrap();
    }

    #[test]
    fn details_are_signed_and_checked() {
        let (ask, fvk) = keys(9);
        let mut l = sample(&fvk);
        // An entry without details signs the same bytes as before details existed.
        let before = l.signing_bytes().unwrap();
        assert!(!String::from_utf8_lossy(&before).contains("description"));
        l.items[0].description = Some("Front row, Saturday".into());
        l.items[0].icon = Some("\u{1f39f}\u{fe0f}".into());
        l.items[0].check_details().unwrap();
        l.sign(&ask, 1).unwrap();
        l.verify_signature().unwrap();
        let mut edited = l.clone();
        edited.items[0].description = Some("Back row".into());
        assert!(edited.verify_signature().is_err());

        assert!(check_details(Some(""), None).is_err());
        assert!(check_details(Some("two\nlines"), None).is_err());
        assert!(check_details(Some(&"x".repeat(MAX_DESCRIPTION_CHARS + 1)), None).is_err());
        assert!(check_details(None, Some("<b>")).is_err());
        assert!(check_details(None, Some("A")).is_err());
        assert!(check_details(None, Some("\u{1f3b5}")).is_ok());
    }

    #[test]
    fn files_without_optional_fields_still_parse() {
        let text = r#"{"version":1,"network":"main","item_ufvk":"u","item_birthday":1,"payout_address":"a",
            "items":[{"note":"ab:0","price_zat":1,"max_expiry_delta":10}]}"#;
        let l: Listings = serde_json::from_str(text).unwrap();
        assert_eq!(l.items[0].name, "");
        assert!(l.signature.is_none());
    }

    #[test]
    fn write_is_atomic_and_readable() {
        let (_, fvk) = keys(9);
        let dir = std::env::temp_dir().join(format!("zsettle-listings-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("listings.json");
        let l = sample(&fvk);
        l.write(&path).unwrap();
        assert!(!dir.join("listings.json.tmp").exists());
        assert_eq!(Listings::read(&path).unwrap(), l);
        fs::remove_dir_all(&dir).unwrap();
    }
}
