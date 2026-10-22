//! Shops run by visitors' browsers.
//!
//! A browser shop keeps its item key in its owner's page, so it cannot answer buyers by itself.
//! This server keeps a mailbox for it: the signed listings, the offers buyers post, and the
//! owner's replies. Buyers use the same `/shop/<code>/...` requests as for any shop; the owner's
//! page uploads listings, collects offers and posts replies, each request signed with the
//! shop's item key (so only that key's holder can use the mailbox). The code is the key's
//! fingerprint. Nothing here holds a key; a signed reply is the owner's half of a trade.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use zsettle_core::listing::Listings;
use zsettle_core::pczt::envelope_tag;

/// Mailboxes kept at once.
const MAX_SHOPS: usize = 200;
/// Offers waiting for one shop.
const MAX_PENDING: usize = 50;
/// Replies kept for one shop.
const MAX_REPLIES: usize = 500;
/// A shop counts as open this long after its page last asked for offers.
pub const OPEN_SECS: u64 = 45;
/// A mailbox, and a reply, is dropped this long after it was last used.
const KEEP_SECS: u64 = 86_400;
/// How far a signed request's time may be from this server's.
const MAX_SKEW_SECS: u64 = 300;
/// Largest listings file.
const MAX_LISTINGS_BYTES: usize = 256 * 1024;
/// Largest offer, as the seller service takes them.
const MAX_ENVELOPE_BYTES: usize = 1024 * 1024;

enum Reply {
    Signed(Vec<u8>),
    Refused(String),
}

struct Shop {
    raw: String,
    listings: Listings,
    /// Waiting offers: tag, bytes, when.
    offers: Vec<(String, Vec<u8>, u64)>,
    replies: HashMap<String, (Reply, u64)>,
    /// When the owner's page last asked for offers (zero: closed).
    seen: u64,
    used: u64,
}

/// Every browser shop of this server.
#[derive(Default)]
pub struct Mailboxes {
    shops: Mutex<HashMap<String, Shop>>,
}

fn signed_by(l: &Listings, body: &Value, msg: &str, now: u64) -> Result<()> {
    let at = body["at"].as_u64().ok_or_else(|| anyhow!("missing at"))?;
    if at.abs_diff(now) > MAX_SKEW_SECS {
        return Err(anyhow!(
            "the request's time is off; check this device's clock"
        ));
    }
    let sig = body["sig"].as_str().ok_or_else(|| anyhow!("missing sig"))?;
    l.verify_mailbox(sig, format!("{msg}\n{at}").as_bytes())
}

impl Mailboxes {
    /// Owner: upload signed listings (a new mailbox, or new listings for one).
    pub fn publish(&self, code: &str, body: &Value, now: u64) -> Result<Value> {
        let raw = body["listings"]
            .as_str()
            .ok_or_else(|| anyhow!("missing listings"))?;
        if raw.len() > MAX_LISTINGS_BYTES {
            return Err(anyhow!("the listings are too large"));
        }
        let l = Listings::parse(raw, "the listings")?;
        l.verify_signature()?;
        if l.key_fingerprint() != code {
            return Err(anyhow!("these listings belong to another shop"));
        }
        signed_by(&l, body, &format!("listings\n{code}\n{raw}"), now)?;
        let mut shops = self.shops.lock().unwrap();
        shops.retain(|_, s| s.used + KEEP_SECS > now);
        if !shops.contains_key(code) && shops.len() >= MAX_SHOPS {
            return Err(anyhow!(
                "this server holds many shops right now; try again later"
            ));
        }
        let shop = shops.entry(code.to_string()).or_insert_with(|| Shop {
            raw: String::new(),
            listings: l.clone(),
            offers: Vec::new(),
            replies: HashMap::new(),
            seen: 0,
            used: now,
        });
        shop.raw = raw.to_string();
        shop.listings = l;
        shop.used = now;
        Ok(json!({ "code": code, "items": shop.listings.items.len() }))
    }

    /// Owner: the offers waiting; marks the shop open.
    pub fn poll(&self, code: &str, body: &Value, now: u64) -> Result<Value> {
        let mut shops = self.shops.lock().unwrap();
        let shop = shops
            .get_mut(code)
            .ok_or_else(|| anyhow!("no such shop; publish first"))?;
        signed_by(&shop.listings, body, &format!("poll\n{code}"), now)?;
        shop.seen = now;
        shop.used = now;
        shop.replies.retain(|_, (_, at)| *at + KEEP_SECS > now);
        let offers: Vec<Value> = shop
            .offers
            .iter()
            .map(|(tag, bytes, _)| json!({ "tag": tag, "envelope": hex::encode(bytes) }))
            .collect();
        Ok(json!({ "offers": offers }))
    }

    /// Owner: the reply to one offer, signed or refused.
    pub fn reply(&self, code: &str, body: &Value, now: u64) -> Result<Value> {
        let tag = body["tag"].as_str().ok_or_else(|| anyhow!("missing tag"))?;
        let mut shops = self.shops.lock().unwrap();
        let shop = shops.get_mut(code).ok_or_else(|| anyhow!("no such shop"))?;
        let (payload, reply) = match (body["signed"].as_str(), body["refused"].as_str()) {
            (Some(h), None) => (format!("signed:{h}"), Reply::Signed(hex::decode(h)?)),
            (None, Some(r)) => (
                format!("refused:{r}"),
                Reply::Refused(r.chars().take(500).collect()),
            ),
            _ => return Err(anyhow!("give signed or refused")),
        };
        signed_by(
            &shop.listings,
            body,
            &format!("reply\n{code}\n{tag}\n{payload}"),
            now,
        )?;
        if !shop.offers.iter().any(|(t, _, _)| t == tag) {
            return Err(anyhow!("no such offer"));
        }
        shop.offers.retain(|(t, _, _)| t != tag);
        if shop.replies.len() >= MAX_REPLIES {
            let oldest = shop
                .replies
                .iter()
                .min_by_key(|(_, (_, at))| *at)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                shop.replies.remove(&k);
            }
        }
        shop.replies.insert(tag.to_string(), (reply, now));
        Ok(json!({ "ok": true }))
    }

    /// Owner: close the shop (its page is going away).
    pub fn close(&self, code: &str, body: &Value, now: u64) -> Result<Value> {
        let mut shops = self.shops.lock().unwrap();
        let shop = shops.get_mut(code).ok_or_else(|| anyhow!("no such shop"))?;
        signed_by(&shop.listings, body, &format!("close\n{code}"), now)?;
        shop.seen = 0;
        Ok(json!({ "closed": true }))
    }

    /// Whether `code` names a browser shop here.
    pub fn has(&self, code: &str) -> bool {
        self.shops.lock().unwrap().contains_key(code)
    }

    /// Buyer: the shop's signed listings, as uploaded.
    pub fn listings(&self, code: &str) -> Option<String> {
        self.shops.lock().unwrap().get(code).map(|s| s.raw.clone())
    }

    /// Buyer: leave an offer. The answer names its tag, as the seller service's does.
    pub fn offer(
        &self,
        code: &str,
        bytes: &[u8],
        now: u64,
    ) -> std::result::Result<Value, (u16, String)> {
        let mut shops = self.shops.lock().unwrap();
        let shop = shops
            .get_mut(code)
            .ok_or((404, "no such shop".to_string()))?;
        if shop.seen + OPEN_SECS <= now {
            return Err((
                503,
                "this shop is closed: its owner's page is not open".into(),
            ));
        }
        if bytes.is_empty() || bytes.len() > MAX_ENVELOPE_BYTES {
            return Err((413, "the envelope must be between 1 byte and 1 MiB".into()));
        }
        let tag = envelope_tag(bytes);
        // A refusal of an earlier copy of these bytes must not answer for this one.
        if matches!(shop.replies.get(&tag), Some((Reply::Refused(_), _))) {
            shop.replies.remove(&tag);
        }
        if !shop.replies.contains_key(&tag) && !shop.offers.iter().any(|(t, _, _)| *t == tag) {
            if shop.offers.len() >= MAX_PENDING {
                return Err((503, "too many offers waiting; try again later".into()));
            }
            shop.offers.push((tag.clone(), bytes.to_vec(), now));
        }
        Ok(json!({ "tag": tag, "reply": format!("/reply/{tag}") }))
    }

    /// Buyer: the reply to an offer: (status, content type, body).
    pub fn reply_for(&self, code: &str, tag: &str) -> (u16, &'static str, Vec<u8>) {
        let shops = self.shops.lock().unwrap();
        let Some(shop) = shops.get(code) else {
            return (404, "text/plain", b"no such shop".to_vec());
        };
        match shop.replies.get(tag) {
            Some((Reply::Signed(b), _)) => (200, "application/octet-stream", b.clone()),
            Some((Reply::Refused(r), _)) => (410, "text/plain", r.clone().into_bytes()),
            None if shop.offers.iter().any(|(t, _, _)| t == tag) => {
                (202, "text/plain", b"pending".to_vec())
            }
            None => (410, "text/plain", b"no longer available".to_vec()),
        }
    }

    /// The browser shops open now, for the market.
    pub fn open(&self, origin: &str, now: u64) -> Vec<Value> {
        self.shops
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, s)| s.seen + OPEN_SECS > now)
            .map(|(code, s)| {
                json!({
                    "base": format!("{origin}/shop/{code}"), "code": code,
                    "name": format!("Shop {code}"), "items": s.listings.items.len(), "house": false,
                })
            })
            .collect()
    }
}
