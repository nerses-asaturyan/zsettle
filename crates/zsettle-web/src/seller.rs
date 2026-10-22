//! Selling from the browser: mint items, sign the listings, cancel offers, and run a shop
//! that judges and signs offers while its page is open.
//!
//! The checks are the seller service's (`zsettle-seller serve`), in the same order: the
//! public ones first, then the proof (once per envelope), then the ones that use the
//! seller's own keys. The item key is held in memory only while the shop is open.

use std::collections::HashSet;
use std::str::FromStr;

use anyhow::{anyhow, Context, Result};
use orchard::keys::{FullViewingKey, Scope, SpendAuthorizingKey};
use serde_json::{json, Value};
use wasm_bindgen::prelude::*;
use zcash_keys::address::{Address, UnifiedAddress};
use zcash_protocol::consensus::{BlockHeight, BranchId, Network};

use zsettle_core::{
    listing::{ListedItem, Listings},
    mint::{self, MintRequest, NoteState},
    pczt as pczt_ops,
    policy::{check, check_public, prepared_ivk, PolicyError, SellerPolicy},
    trade::{build_transfer, Output},
    wallet::{
        account_birthday, account_orchard_fvk, account_unspent_nullifiers, heights,
        ironwood_note, largest_spendable_note, max_scanned_height, next_orchard_address,
        record_transaction, recent_ironwood_anchors, Db, NoteRef,
    },
    DEFAULT_MAX_EXPIRY_DELTA, MIN_PRICE_ZAT, ZIP317_MARGINAL_FEE,
};

use crate::{js, seed_of, WebWallet, ITEM_ACCOUNT, PAY_ACCOUNT};

/// Longest item name.
const MAX_NAME_CHARS: usize = 80;
/// Anchors (tree roots of recent blocks) a trade may be built on.
const ANCHOR_DEPTH: u32 = 100;
/// Blocks a buyer may be ahead of this wallet.
const LAG_SLACK: u32 = 5;
/// A trade has two or three actions.
const MAX_TRADE_ACTIONS: usize = 3;
/// Envelope tags whose proof was verified, kept to skip a second verification.
const MAX_VERIFIED_TAGS: usize = 10_000;

/// The shop's open state: the item key, and the proofs already verified.
#[derive(Default)]
pub(crate) struct ShopState {
    ask: Option<SpendAuthorizingKey>,
    verified: HashSet<String>,
}

fn obj(v: Value) -> JsValue {
    js_sys::JSON::parse(&v.to_string()).unwrap_or(JsValue::NULL)
}

impl WebWallet {
    fn item_keys(&self, db: &Db, mnemonic: &str) -> Result<(SpendAuthorizingKey, FullViewingKey)> {
        let seed = seed_of(mnemonic)?;
        let usk = self.wallet.spending_key(&seed, ITEM_ACCOUNT.1)?;
        let fvk = FullViewingKey::from(usk.orchard());
        let account = self.account(db, ITEM_ACCOUNT.0)?;
        if fvk != account_orchard_fvk(db, account)? {
            return Err(anyhow!("the recovery phrase is not this wallet's"));
        }
        Ok((SpendAuthorizingKey::from(usk.orchard()), fvk))
    }

    fn payout_fvk(&self, db: &Db) -> Result<FullViewingKey> {
        account_orchard_fvk(db, self.account(db, PAY_ACCOUNT.0)?)
    }
}

/// The Orchard receiver of the listed payout address.
fn listed_payout(l: &Listings, network: Network) -> Result<orchard::Address> {
    match Address::decode(&network, &l.payout_address) {
        Some(Address::Unified(ua)) => ua.orchard().copied(),
        _ => None,
    }
    .ok_or_else(|| anyhow!("the listings payout address is not a unified address with an Orchard receiver"))
}

#[wasm_bindgen]
impl WebWallet {
    /// Mint items: one note each at a fresh address of the item account, named in its memo,
    /// paid by the paying account. Returns `{ txid, notes }` once the server took it.
    pub async fn mint(&mut self, mnemonic: String, names: Vec<String>) -> Result<JsValue, JsError> {
        let r = self.mint_inner(&mnemonic, names).await.map_err(js)?;
        Ok(obj(r))
    }

    async fn mint_inner(&mut self, mnemonic: &str, names: Vec<String>) -> Result<Value> {
        let names: Vec<String> = names.into_iter().map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).collect();
        if names.is_empty() || names.len() > 20 {
            return Err(anyhow!("give between one and twenty item names"));
        }
        if names.iter().any(|n| n.chars().count() > MAX_NAME_CHARS) {
            return Err(anyhow!("an item name has at most {MAX_NAME_CHARS} characters"));
        }
        let seed = seed_of(mnemonic)?;
        let mut db = self.wallet.db()?;
        let next = max_scanned_height(&db)?.unwrap_or(0) + 1;
        self.relay.check_consensus(self.wallet.network, next).await?;
        let req = MintRequest {
            from_account: self.account(&db, PAY_ACCOUNT.0)?,
            from_account_index: PAY_ACCOUNT.1,
            item_account: self.account(&db, ITEM_ACCOUNT.0)?,
            value_zat: zsettle_core::item::MIN_ITEM_VALUE,
            names: names.clone(),
        };
        let (bytes, txid, indices) = mint::mint(&self.wallet, &mut db, &seed, &req)?;
        self.relay.send(&bytes).await?;
        let notes: Vec<Option<String>> = indices
            .iter()
            .map(|i| i.map(|i| format!("{txid}:{i}")))
            .collect();
        for (note, name) in notes.iter().zip(&names) {
            if let Some(n) = note {
                self.set_item_meta(n, &json!({ "name": name }))?;
            }
        }
        Ok(json!({ "txid": txid, "notes": notes }))
    }

    /// Add or replace an item in the signed listings (`listings` is the current file, or
    /// empty for a first one); returns the new signed listings as JSON.
    #[allow(clippy::too_many_arguments)]
    pub fn publish(
        &self,
        mnemonic: String,
        listings: String,
        note: String,
        name: String,
        price_zat: u64,
        description: Option<String>,
        icon: Option<String>,
    ) -> Result<String, JsError> {
        (|| {
            let mut db = self.wallet.db()?;
            let (ask, item_fvk) = self.item_keys(&db, &mnemonic)?;
            let mut l = self.listings_or_new(&mut db, &listings, &item_fvk)?;
            let (target, _) = heights(&db, 1)?;
            let r = NoteRef::from_str(&note)?;
            let n = ironwood_note(&db, r, target)?;
            if item_fvk.scope_for_address(&n.note.recipient()).is_none() {
                return Err(anyhow!("that note is not one of this wallet's items"));
            }
            if name.chars().count() > MAX_NAME_CHARS {
                return Err(anyhow!("a name has at most {MAX_NAME_CHARS} characters"));
            }
            l.upsert(ListedItem {
                note: note.clone(),
                name,
                price_zat,
                max_expiry_delta: DEFAULT_MAX_EXPIRY_DELTA,
                description: description.map(|d| d.trim().to_string()).filter(|d| !d.is_empty()),
                icon: icon.map(|i| i.trim().to_string()).filter(|i| !i.is_empty()),
            });
            for item in &l.items {
                item.check_details().with_context(|| format!("item {}", item.note))?;
                if item.price_zat < MIN_PRICE_ZAT {
                    return Err(anyhow!(
                        "the price must be at least {MIN_PRICE_ZAT} zat: a payment at or below the network's marginal fee is dust no wallet spends"
                    ));
                }
            }
            l.sign(&ask, max_scanned_height(&db)?.unwrap_or(0))?;
            l.verify_signature()?;
            Ok(serde_json::to_string(&l)?)
        })()
        .map_err(js)
    }

    /// Remove an item from the signed listings; returns them signed again.
    pub fn delist(&self, mnemonic: String, listings: String, note: String) -> Result<String, JsError> {
        (|| {
            let db = self.wallet.db()?;
            let (ask, item_fvk) = self.item_keys(&db, &mnemonic)?;
            let mut l = Listings::parse(&listings, "your listings")?;
            if l.item_fvk()? != item_fvk {
                return Err(anyhow!("these listings belong to another wallet"));
            }
            if !l.remove(&note) {
                return Err(anyhow!("that item is not listed"));
            }
            l.sign(&ask, max_scanned_height(&db)?.unwrap_or(0))?;
            Ok(serde_json::to_string(&l)?)
        })()
        .map_err(js)
    }

    /// End every signed offer on an item for good: move it to a fresh note of the item
    /// account, the fee paid from the paying account. Returns `{ txid, fee }`.
    pub async fn cancel(&mut self, mnemonic: String, note: String) -> Result<JsValue, JsError> {
        let r = self.cancel_inner(&mnemonic, &note).await.map_err(js)?;
        Ok(obj(r))
    }

    async fn cancel_inner(&mut self, mnemonic: &str, note: &str) -> Result<Value> {
        let mut db = self.wallet.db()?;
        let (item_ask, _) = self.item_keys(&db, mnemonic)?;
        let seed = seed_of(mnemonic)?;
        let usk_pay = self.wallet.spending_key(&seed, PAY_ACCOUNT.1)?;
        let item_account = self.account(&db, ITEM_ACCOUNT.0)?;
        let pay_account = self.account(&db, PAY_ACCOUNT.0)?;
        let item_note = NoteRef::from_str(note)?;
        let min_conf = 3;
        let (target, _) = heights(&db, min_conf)?;
        let item_value = ironwood_note(&db, item_note, target)?.note.value().inner();
        let fee = zsettle_core::zip317_fee(2);
        // The smallest paying note that covers the fee exactly or leaves change above dust.
        let mut exclude = Vec::new();
        let mut fitting = Vec::new();
        while let Some(n) = largest_spendable_note(&db, pay_account, target, &exclude)? {
            exclude.push(n);
            let v = ironwood_note(&db, n, target)?.note.value().inner();
            if v == fee || v > fee + ZIP317_MARGINAL_FEE {
                fitting.push((v, n));
            }
        }
        let Some(&(pay_value, pay_note)) = fitting.iter().min_by_key(|(v, _)| *v) else {
            return Err(anyhow!("no note can pay the {fee} zat fee yet"));
        };
        let mut outputs = vec![Output { account: item_account, value: item_value, internal: false }];
        if pay_value > fee {
            outputs.push(Output { account: pay_account, value: pay_value - fee, internal: true });
        }
        let next = max_scanned_height(&db)?.unwrap_or(0) + 1;
        self.relay.check_consensus(self.wallet.network, next).await?;
        let built = build_transfer(
            &mut db,
            self.wallet.network,
            &[(item_account, item_note), (pay_account, pay_note)],
            &outputs,
            min_conf,
            true,
        )?;
        let (pczt, n1) = pczt_ops::sign_matching_actions(built.pczt, &item_ask)?;
        let (pczt, n2) = pczt_ops::sign_matching_actions(pczt, &SpendAuthorizingKey::from(usk_pay.orchard()))?;
        if n1 != 1 || n2 != 1 {
            return Err(anyhow!("expected one signature per account, got {n1} and {n2}"));
        }
        let tx = pczt_ops::extract(pczt)?;
        let bytes = pczt_ops::tx_bytes(&tx)?;
        self.relay.send(&bytes).await?;
        record_transaction(&self.wallet, &mut db, &tx)?;
        Ok(json!({ "txid": tx.txid().to_string(), "fee": built.fee }))
    }

    /// Open the shop: the item key stays in this worker until [`WebWallet::shop_close`].
    pub fn shop_open(&mut self, mnemonic: String) -> Result<(), JsError> {
        (|| {
            let db = self.wallet.db()?;
            let (ask, _) = self.item_keys(&db, &mnemonic)?;
            self.shop.ask = Some(ask);
            Ok(())
        })()
        .map_err(js)
    }

    /// Close the shop and drop the item key.
    pub fn shop_close(&mut self) {
        self.shop = ShopState::default();
    }

    /// Whether the shop is open.
    pub fn shop_is_open(&self) -> bool {
        self.shop.ask.is_some()
    }

    /// Sign a mailbox request with the open shop's item key (hex).
    pub fn shop_auth(&self, message: String) -> Result<String, JsError> {
        let ask = self.shop.ask.as_ref().ok_or_else(|| JsError::new("the shop is closed"))?;
        Ok(Listings::sign_mailbox(ask, message.as_bytes()))
    }

    /// Drop from the listings every item the wallet has seen spent (sold, or moved by a
    /// cancel), and sign them again when that changed anything. Returns
    /// `{ listings: <json or null>, sold: [{ note, name, price_zat, txid }] }`.
    pub fn shop_reconcile(&self, listings: String) -> Result<JsValue, JsError> {
        (|| {
            let ask = self.shop.ask.as_ref().ok_or_else(|| anyhow!("the shop is closed"))?;
            let db = self.wallet.db()?;
            let mut l = Listings::parse(&listings, "your listings")?;
            let mut sold = Vec::new();
            let mut kept = Vec::new();
            for item in std::mem::take(&mut l.items) {
                let Ok(r) = item.note_ref() else { continue };
                match mint::note_state(&self.wallet, &db, r)? {
                    NoteState::Spent { txid } => sold.push(json!({ "note": item.note, "name": item.name, "price_zat": item.price_zat, "txid": txid })),
                    _ => kept.push(item),
                }
            }
            l.items = kept;
            if sold.is_empty() {
                return Ok(json!({ "listings": null, "sold": [] }));
            }
            l.sign(ask, max_scanned_height(&db)?.unwrap_or(0))?;
            Ok(json!({ "listings": serde_json::to_string(&l)?, "sold": sold }))
        })()
        .map(obj)
        .map_err(js)
    }

    /// Judge one offer against the shop's listings: `{ state: "signed", reply: <hex>,
    /// name, paid_zat }`, `{ state: "rejected", reason }` or `{ state: "deferred", reason }`.
    pub fn shop_sign(&mut self, envelope: &[u8], listings: String) -> Result<JsValue, JsError> {
        let v = match self.shop_sign_inner(envelope, &listings) {
            Ok((item, paid, bytes)) => json!({ "state": "signed", "reply": hex::encode(bytes), "name": item.name, "paid_zat": paid }),
            Err(Judged::Reject(reason)) => json!({ "state": "rejected", "reason": reason }),
            Err(Judged::Defer(reason)) => json!({ "state": "deferred", "reason": reason }),
        };
        Ok(obj(v))
    }
}

/// An offer not signed now.
enum Judged {
    /// Final: the buyer is told why.
    Reject(String),
    /// Judged again on the next pass: the wallet may be behind the buyer.
    Defer(String),
}

impl WebWallet {
    fn shop_sign_inner(&mut self, bytes: &[u8], listings: &str) -> std::result::Result<(ListedItem, u64, Vec<u8>), Judged> {
        let internal = |e: anyhow::Error| Judged::Defer(format!("error: {e:#}"));
        let ask = self.shop.ask.clone().ok_or_else(|| Judged::Defer("the shop is closed".into()))?;
        if bytes.len() as u64 > zsettle_core::MAX_ENVELOPE_BYTES {
            return Err(Judged::Reject(format!("too large: {} bytes", bytes.len())));
        }
        let l = Listings::parse(listings, "your listings").map_err(internal)?;
        let pczt = pczt_ops::from_bytes(bytes).map_err(|e| Judged::Reject(format!("unreadable: {e:#}")))?;
        let actions = pczt_ops::action_count(&pczt).map_err(|e| Judged::Reject(format!("unreadable: {e:#}")))?;
        if actions > MAX_TRADE_ACTIONS {
            return Err(Judged::Reject(format!("refused: {actions} actions; a trade has two or three")));
        }
        let expiry = *pczt.global().expiry_height();
        let mut db = self.wallet.db().map_err(internal)?;
        let anchors = recent_ironwood_anchors(&mut db, ANCHOR_DEPTH).map_err(internal)?;
        // Stage one: public data only.
        let outcome = self.process(&db, &l, &pczt, &anchors, false).map_err(internal)?;
        let horizon = zsettle_core::reference_blocks(&self.wallet.network, outcome.tip + 1, DEFAULT_MAX_EXPIRY_DELTA)
            .saturating_add(1 + LAG_SLACK);
        if outcome.item.is_none() && outcome.unavailable > 0 && expiry > outcome.tip && expiry <= outcome.tip.saturating_add(horizon) {
            return Err(Judged::Defer("spends no listed item this wallet can spend yet".into()));
        }
        match outcome.result {
            Ok(_) => {}
            Err(PolicyError::UnknownAnchor) if expiry > outcome.tip => {
                return Err(Judged::Defer("anchor not known to this wallet yet".into()))
            }
            Err(PolicyError::ExpiryTooLong { expiry: e, tip, max_delta })
                if e <= tip.saturating_add(max_delta).saturating_add(1 + LAG_SLACK) =>
            {
                return Err(Judged::Defer("expiry past the limit; the buyer may be ahead of this wallet".into()))
            }
            Err(e) => return Err(Judged::Reject(format!("refused: {e}"))),
        }
        let item = outcome.item.ok_or_else(|| Judged::Reject("error: approved trade without a listed item".into()))?;
        // Stage two: the proof, once per envelope.
        let tag = pczt_ops::envelope_tag(bytes);
        if !self.shop.verified.contains(&tag) {
            let vk = zsettle_core::pczt::cached_verifying_key();
            pczt_ops::verify_proof(&pczt, vk).map_err(|e| Judged::Reject(format!("refused: {e:#}")))?;
            if self.shop.verified.len() >= MAX_VERIFIED_TAGS {
                self.shop.verified.clear();
            }
            self.shop.verified.insert(tag);
        }
        // Stage three: the checks that use the seller's own keys.
        let approved = self
            .process(&db, &l, &pczt, &anchors, true)
            .map_err(internal)?
            .result
            .map_err(|e| Judged::Reject(format!("refused: {e}")))?;
        let signed = pczt_ops::sign_ironwood_action(pczt, approved.item_action_index, &ask)
            .map_err(|e| Judged::Reject(format!("signing failed: {e:#}")))?;
        let out = pczt_ops::to_bytes(&signed).map_err(|e| Judged::Reject(format!("signing failed: {e:#}")))?;
        Ok((item, approved.paid_zat, out))
    }

    /// Match a trade against the listings and run the policy checks.
    fn process(
        &self,
        db: &Db,
        l: &Listings,
        pczt: &pczt::Pczt,
        anchors: &[orchard::Anchor],
        with_secrets: bool,
    ) -> Result<Outcome> {
        let network = self.wallet.network;
        let (target, _) = heights(db, 1)?;
        let tip = u32::from(BlockHeight::from(target)).saturating_sub(1);
        let item_account = self.account(db, ITEM_ACCOUNT.0)?;
        let item_fvk = account_orchard_fvk(db, item_account)?;
        if l.item_fvk()? != item_fvk {
            return Err(anyhow!("the listings belong to another wallet"));
        }
        let seller_nfs = account_unspent_nullifiers(db, item_account)?;
        let trade_nfs = pczt_ops::action_nullifiers(pczt)?;
        let mut matched = None;
        let mut unavailable = 0usize;
        for item in &l.items {
            let Ok(r) = item.note_ref() else { continue };
            let Ok(n) = ironwood_note(db, r, target) else {
                unavailable += 1;
                continue;
            };
            if item_fvk.scope_for_address(&n.note.recipient()).is_none() {
                continue;
            }
            let nf = n.note.nullifier(&item_fvk);
            if trade_nfs.contains(&nf) {
                matched = Some((item.clone(), nf));
                break;
            }
        }
        let Some((item, nf)) = matched else {
            return Ok(Outcome { item: None, result: Err(PolicyError::ItemNotSpent), unavailable, tip });
        };
        let payout_fvk = self.payout_fvk(db)?;
        let payout = listed_payout(l, network)?;
        if payout_fvk.scope_for_address(&payout) != Some(Scope::External) {
            return Err(anyhow!("the listings payout address is not this wallet's"));
        }
        let ivk = prepared_ivk(&payout_fvk);
        let policy = SellerPolicy {
            price_zat: item.price_zat,
            max_expiry_delta: zsettle_core::reference_blocks(&network, tip + 1, item.max_expiry_delta),
            tip,
            branch_id: u32::from(BranchId::for_height(&network, BlockHeight::from(tip + 1))),
            payout,
        };
        let result = if with_secrets {
            check(pczt, &nf, &seller_nfs, &ivk, &policy, anchors)
        } else {
            check_public(pczt, &nf, &policy, anchors)
        };
        Ok(Outcome { item: Some(item), result, unavailable, tip })
    }

    /// The current listings, or new ones for this wallet: the item key and its birthday, and a
    /// payout address derived for the market (not the account's default address).
    fn listings_or_new(&self, db: &mut Db, listings: &str, item_fvk: &FullViewingKey) -> Result<Listings> {
        if !listings.trim().is_empty() {
            let l = Listings::parse(listings, "your listings")?;
            if &l.item_fvk()? != item_fvk {
                return Err(anyhow!("these listings belong to another wallet"));
            }
            l.verify_signature()?;
            return Ok(l);
        }
        let network = self.wallet.network;
        let payout = next_orchard_address(db, self.account(db, PAY_ACCOUNT.0)?)?;
        let ua = UnifiedAddress::from_receivers(Some(payout), None, None, None, None)
            .ok_or_else(|| anyhow!("cannot build the payout address"))?;
        Ok(Listings {
            version: 1,
            network: crate::network_name(network).into(),
            item_ufvk: Listings::encode_item_key(item_fvk, network)?,
            item_birthday: account_birthday(db, self.account(db, ITEM_ACCOUNT.0)?)?,
            payout_address: ua.encode(&network),
            items: vec![],
            issued_at: 0,
            signature: None,
        })
    }
}

struct Outcome {
    item: Option<ListedItem>,
    result: std::result::Result<zsettle_core::policy::Approved, PolicyError>,
    unavailable: usize,
    tip: u32,
}

/// The shop code of signed listings: the fingerprint of their item key.
#[wasm_bindgen]
pub fn listings_code(listings: &str) -> Result<String, JsError> {
    Listings::parse(listings, "listings")
        .map(|l| l.key_fingerprint())
        .map_err(js)
}
