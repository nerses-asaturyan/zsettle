//! Listings: the public file a seller publishes.
//!
//! It carries everything a buyer needs and nothing more: the item account's full viewing
//! key (the consignment), the seller's payout address, and one entry per item with its note
//! reference, price and expiry policy. Prices are public by design; the seller's other
//! accounts stay invisible because items live in their own ZIP 32 account.

use std::{fs, path::Path};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::wallet::NoteRef;

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

    /// Write a listings file.
    pub fn write(&self, path: &Path) -> Result<()> {
        fs::write(path, serde_json::to_string_pretty(self)?)
            .with_context(|| format!("writing {}", path.display()))
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
}
