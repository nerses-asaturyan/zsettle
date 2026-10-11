//! Listings: the public file a seller publishes.
//!
//! It carries everything a buyer needs and nothing more: the item account's full viewing
//! key (the consignment), the seller's payout address, and one entry per item with its note
//! reference, price and expiry policy. Prices are public by design; the seller's other
//! accounts stay invisible because items live in their own ZIP 32 account.
//!
//! An item with a signed trade outstanding is marked *under offer* until that trade's
//! expiry height. The seller service honours one offer at a time; once the expiry passes
//! with the note still unspent the item is listed again, and once the note is spent the
//! item is removed.

use std::{fs, path::Path};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::wallet::NoteRef;

/// A signed trade that has not settled yet.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Pending {
    /// Height after which the signed trade can no longer be mined.
    pub expiry_height: u32,
    /// Trade envelope the seller signed (file stem), for the seller's bookkeeping.
    #[serde(default)]
    pub trade: String,
}

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
    /// Set while a signed trade for this item is outstanding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<Pending>,
}

impl ListedItem {
    /// Parse the note reference.
    pub fn note_ref(&self) -> Result<NoteRef> {
        self.note.parse()
    }
}

/// A seller's published listings.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Listings {
    /// Format version.
    pub version: u32,
    /// Network name (`main` or `test`).
    pub network: String,
    /// Unified full viewing key of the item account (the consignment).
    pub item_ufvk: String,
    /// Height from which a buyer must scan to see the item notes.
    pub item_birthday: u32,
    /// Seller's unified address for payments.
    pub payout_address: String,
    /// Items for sale.
    pub items: Vec<ListedItem>,
}

impl Listings {
    /// Read a listings file.
    pub fn read(path: &Path) -> Result<Self> {
        let text =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let l: Listings =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        if l.version != 1 {
            return Err(anyhow!("unsupported listings version {}", l.version));
        }
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

    /// Find an item by note reference string.
    pub fn find(&self, note: &str) -> Option<&ListedItem> {
        self.items.iter().find(|i| i.note == note)
    }

    /// Add or replace an item.
    pub fn upsert(&mut self, item: ListedItem) {
        if let Some(existing) = self.items.iter_mut().find(|i| i.note == item.note) {
            *existing = item;
        } else {
            self.items.push(item);
        }
    }

    /// Remove an item (after it sold).
    pub fn remove(&mut self, note: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|i| i.note != note);
        self.items.len() != before
    }

    /// Mark an item as under offer. Returns `false` when the item is not listed.
    pub fn set_pending(&mut self, note: &str, pending: Pending) -> bool {
        match self.items.iter_mut().find(|i| i.note == note) {
            Some(item) => {
                item.pending = Some(pending);
                true
            }
            None => false,
        }
    }

    /// Clear the under-offer mark of an item. Returns `true` when something changed.
    pub fn clear_pending(&mut self, note: &str) -> bool {
        match self.items.iter_mut().find(|i| i.note == note) {
            Some(item) => item.pending.take().is_some(),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Listings {
        Listings {
            version: 1,
            network: "main".into(),
            item_ufvk: "uview1example".into(),
            item_birthday: 3_513_947,
            payout_address: "u1example".into(),
            items: vec![ListedItem {
                note: "00".repeat(32) + ":1",
                name: "Item".into(),
                price_zat: 5_000,
                max_expiry_delta: 10,
                pending: None,
            }],
        }
    }

    #[test]
    fn pending_round_trips_and_is_omitted_when_absent() {
        let mut l = sample();
        let plain = serde_json::to_string(&l).unwrap();
        assert!(!plain.contains("pending"));
        let note = l.items[0].note.clone();
        assert!(l.set_pending(
            &note,
            Pending {
                expiry_height: 42,
                trade: "t1".into()
            }
        ));
        let text = serde_json::to_string(&l).unwrap();
        let back: Listings = serde_json::from_str(&text).unwrap();
        assert_eq!(back, l);
        assert_eq!(back.items[0].pending.as_ref().unwrap().expiry_height, 42);
        assert!(l.clear_pending(&note));
        assert!(!l.clear_pending(&note));
        assert!(!l.set_pending(
            "missing",
            Pending {
                expiry_height: 1,
                trade: String::new()
            }
        ));
    }

    #[test]
    fn files_without_pending_still_parse() {
        let text = r#"{"version":1,"network":"main","item_ufvk":"u","item_birthday":1,"payout_address":"a",
            "items":[{"note":"ab:0","price_zat":1,"max_expiry_delta":10}]}"#;
        let l: Listings = serde_json::from_str(text).unwrap();
        assert_eq!(l.items[0].pending, None);
        assert_eq!(l.items[0].name, "");
    }

    #[test]
    fn write_is_atomic_and_readable() {
        let dir = std::env::temp_dir().join(format!("zsettle-listings-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("listings.json");
        let l = sample();
        l.write(&path).unwrap();
        assert!(!dir.join("listings.json.tmp").exists());
        assert_eq!(Listings::read(&path).unwrap(), l);
        fs::remove_dir_all(&dir).unwrap();
    }
}
