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

/// Mailboxes kept at once; when full, the longest-closed one makes room.
const MAX_SHOPS: usize = 200;
/// Mailboxes made from one visitor (an IPv4 address or an IPv6 /64) and kept at once.
const MAX_SHOPS_PER_VISITOR: usize = 3;
/// Offers waiting for one shop, and their bytes.
const MAX_PENDING: usize = 20;
const MAX_PENDING_BYTES: usize = 8 * 1024 * 1024;
/// Offers handed to the owner's page in one poll.
const MAX_POLL_OFFERS: usize = 5;
/// An offer the owner did not answer in this long is dropped (a closed shop's at once).
const OFFER_SECS: u64 = 15 * 60;
/// Replies kept for one shop, and the size of one signed reply.
const MAX_REPLIES: usize = 200;
const MAX_REPLY_BYTES: usize = 256 * 1024;
/// Offers and replies held by all mailboxes together.
const MAX_TOTAL_BYTES: usize = 256 * 1024 * 1024;
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
    /// The visitor that made the mailbox, and the time of its newest listings.
    owner: String,
    listings_at: u64,
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

impl Shop {
    fn bytes(&self) -> usize {
        self.offers.iter().map(|o| o.1.len()).sum::<usize>()
            + self
                .replies
                .values()
                .map(|(r, _)| match r {
                    Reply::Signed(b) => b.len(),
                    Reply::Refused(r) => r.len(),
                })
                .sum::<usize>()
    }

    fn open(&self, now: u64) -> bool {
        self.seen + OPEN_SECS > now
    }

    /// Drop old replies, and offers nobody will answer: old ones, and all of a closed shop's.
    fn prune(&mut self, now: u64) {
        let open = self.open(now);
        self.offers.retain(|o| open && o.2 + OFFER_SECS > now);
        self.replies.retain(|_, (_, at)| *at + KEEP_SECS > now);
    }
}

fn total_bytes(shops: &HashMap<String, Shop>) -> usize {
    shops.values().map(Shop::bytes).sum()
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
    /// Owner: upload signed listings (a new mailbox, or new listings for one). `visitor`
    /// is who asks, for the limit of mailboxes per visitor.
    pub fn publish(&self, code: &str, body: &Value, visitor: &str, now: u64) -> Result<Value> {
        let raw = body["listings"]
            .as_str()
            .ok_or_else(|| anyhow!("missing listings"))?;
        if raw.len() > MAX_LISTINGS_BYTES {
            return Err(anyhow!("the listings are too large"));
        }
        let l = Listings::parse(raw, "the listings")?;
        l.verify_signature()?;
        // Only the plain Orchard key, written the one way: no other spelling of the same key
        // (extra items in the unified key) can be searched for a code that matches another's.
        if Listings::encode_item_key(&l.item_fvk()?, l.network()?)? != l.item_ufvk {
            return Err(anyhow!("the listings' item key is not in its plain form"));
        }
        if l.key_fingerprint() != code {
            return Err(anyhow!("these listings belong to another shop"));
        }
        signed_by(&l, body, &format!("listings\n{code}\n{raw}"), now)?;
        let at = body["at"].as_u64().unwrap_or(0);
        let mut shops = self.shops.lock().unwrap();
        shops.retain(|_, s| s.used + KEEP_SECS > now);
        if let Some(shop) = shops.get(code) {
            // The code stays with the key that made the mailbox, and listings never go back
            // to an older signed request.
            if shop.listings.item_ufvk != l.item_ufvk {
                return Err(anyhow!("these listings belong to another shop"));
            }
            if at < shop.listings_at {
                return Err(anyhow!("newer listings are already here"));
            }
        } else {
            if shops.values().filter(|s| s.owner == visitor).count() >= MAX_SHOPS_PER_VISITOR {
                return Err(anyhow!(
                    "you opened several shops lately; try again tomorrow"
                ));
            }
            if shops.len() >= MAX_SHOPS {
                // Make room: the shop closed longest ago goes.
                let oldest = shops
                    .iter()
                    .filter(|(_, s)| !s.open(now))
                    .min_by_key(|(_, s)| s.used)
                    .map(|(k, _)| k.clone());
                match oldest {
                    Some(k) => {
                        shops.remove(&k);
                    }
                    None => {
                        return Err(anyhow!(
                            "this server holds many shops right now; try again later"
                        ))
                    }
                }
            }
        }
        let shop = shops.entry(code.to_string()).or_insert_with(|| Shop {
            raw: String::new(),
            listings: l.clone(),
            owner: visitor.to_string(),
            listings_at: 0,
            offers: Vec::new(),
            replies: HashMap::new(),
            seen: 0,
            used: now,
        });
        shop.raw = raw.to_string();
        shop.listings = l;
        shop.listings_at = at;
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
        shop.prune(now);
        let offers: Vec<Value> = shop
            .offers
            .iter()
            .take(MAX_POLL_OFFERS)
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
            (Some(h), None) if h.len() <= 2 * MAX_REPLY_BYTES => {
                (format!("signed:{h}"), Reply::Signed(hex::decode(h)?))
            }
            (Some(_), None) => return Err(anyhow!("the reply is too large")),
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
        shop.prune(now);
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
        let total = total_bytes(&shops);
        let shop = shops
            .get_mut(code)
            .ok_or((404, "no such shop".to_string()))?;
        shop.prune(now);
        if !shop.open(now) {
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
            let pending: usize = shop.offers.iter().map(|o| o.1.len()).sum();
            if shop.offers.len() >= MAX_PENDING
                || pending + bytes.len() > MAX_PENDING_BYTES
                || total + bytes.len() > MAX_TOTAL_BYTES
            {
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
            .filter(|(_, s)| s.open(now))
            .map(|(code, s)| {
                json!({
                    "base": format!("{origin}/shop/{code}"), "code": code,
                    "name": format!("Shop {code}"), "items": s.listings.items.len(), "house": false,
                    "network": s.listings.network,
                })
            })
            .collect()
    }
}
