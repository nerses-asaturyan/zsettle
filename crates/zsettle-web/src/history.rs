//! Item history and ownership-proof checks, in the browser: the same trace as the tools
//! (`zsettle verify`, `zsettle check-proof`), fed with blocks fetched through the relay.

use std::str::FromStr;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use wasm_bindgen::prelude::*;
use zcash_protocol::consensus::Network;

use zsettle_core::{
    listing::Listings,
    ownership::{check_challenge, OwnershipProof, DEFAULT_MAX_AGE_REFERENCE_BLOCKS},
    verify::{Consignment, NoteNotFound, Provenance, Status, Tracer},
    wallet::{account_birthday, account_orchard_fvk, NoteRef},
};

use crate::{js, progress, WebWallet, ITEM_ACCOUNT};

/// Blocks fetched per request while tracing.
const TRACE_BATCH: u32 = 500;

fn obj(v: Value) -> JsValue {
    js_sys::JSON::parse(&v.to_string()).unwrap_or(JsValue::NULL)
}

impl WebWallet {
    /// Trace `genesis` with the consignments, reading blocks from its transaction's block to
    /// the tip (on to the tip even once resolved, so the server does not learn where the item
    /// last moved).
    async fn trace(
        &self,
        network: Network,
        consignments: &[Consignment],
        genesis: NoteRef,
        cb: &Option<js_sys::Function>,
    ) -> Result<Provenance> {
        let tip = self.relay.tip().await?;
        let (genesis_height, raw) = match self.relay.raw_tx(&genesis.txid).await {
            Ok((h, raw)) => (h, raw),
            Err(_) => {
                return Err(NoteNotFound("the genesis transaction is not in a block".into()).into())
            }
        };
        let from = Tracer::start_height(consignments, Some(genesis_height));
        let mut tracer = Tracer::new(consignments, genesis, Some(genesis_height))?;
        let mut start = from;
        'scan: while start <= tip {
            let end = (start + TRACE_BATCH - 1).min(tip);
            let blocks = self.relay.blocks(start, end).await?;
            // A short or gapped answer would skip a spend: every block, in order, or none.
            if blocks.len() as u64 != u64::from(end - start + 1)
                || blocks
                    .iter()
                    .zip(start..=end)
                    .any(|(b, h)| b.height != u64::from(h))
            {
                return Err(anyhow!(
                    "the server sent blocks {start} to {end} incomplete; try again"
                ));
            }
            for block in &blocks {
                if !tracer.feed(block)? {
                    break 'scan;
                }
            }
            progress(cb, json!({ "scanned": end, "tip": tip }));
            start = end + 1;
        }
        tracer.finish(network, from, Some(&raw))
    }
}

#[wasm_bindgen]
impl WebWallet {
    /// The history of an item note of this wallet, or of a shop's item (`note`), read with
    /// the keys of the shops' signed listings (`[{ label, text }]`) and this wallet's item
    /// key (labelled "you"). A bought item is traced from the shop's note it was bought as.
    pub async fn history(
        &self,
        note: String,
        listings: String,
        on_progress: Option<js_sys::Function>,
    ) -> Result<JsValue, JsError> {
        let r: Result<Value> = async {
            let network = self.wallet.network;
            let mut cs = Vec::new();
            for l in serde_json::from_str::<Vec<Value>>(&listings)? {
                let text = l["text"]
                    .as_str()
                    .ok_or_else(|| anyhow!("listings without text"))?;
                let label = l["label"].as_str().unwrap_or("shop").to_string();
                let parsed = Listings::parse(text, "the shop's listings")?;
                parsed
                    .verify_signature()
                    .map_err(|e| anyhow!("the listings of {label} are not trustworthy: {e:#}"))?;
                if parsed.network()? != network {
                    return Err(anyhow!("the listings of {label} are for another network"));
                }
                cs.push(Consignment {
                    label,
                    fvk: parsed.item_fvk()?,
                    birthday: parsed.item_birthday,
                });
            }
            {
                let db = self.wallet.db()?;
                let account = self.account(&db, ITEM_ACCOUNT.0)?;
                cs.push(Consignment {
                    label: "you".into(),
                    fvk: account_orchard_fvk(&db, account)?,
                    birthday: account_birthday(&db, account)?,
                });
            }
            let genesis = self
                .item_meta()?
                .get(&note)
                .and_then(|m| m["bought_from"].as_str().map(str::to_string))
                .unwrap_or(note);
            let p = self
                .trace(network, &cs, NoteRef::from_str(&genesis)?, &on_progress)
                .await?;
            Ok(serde_json::to_value(&p)?)
        }
        .await;
        r.map(obj).map_err(js)
    }

    /// Check an ownership proof someone shared, as `zsettle check-proof` does: signature,
    /// network, the block it names (on this chain, with that hash, recent), then on chain
    /// whether the note is still held.
    pub async fn check_proof(
        &self,
        proof: String,
        challenge: Option<String>,
        on_progress: Option<js_sys::Function>,
    ) -> Result<JsValue, JsError> {
        let r = self
            .check_proof_inner(
                &proof,
                challenge.filter(|c| !c.is_empty()).as_deref(),
                &on_progress,
            )
            .await;
        r.map(obj).map_err(js)
    }

    async fn check_proof_inner(
        &self,
        text: &str,
        challenge: Option<&str>,
        cb: &Option<js_sys::Function>,
    ) -> Result<Value> {
        if let Some(c) = challenge {
            check_challenge(c)?;
        }
        let p = OwnershipProof::parse(text)?;
        let network = self.wallet.network;
        let mut report = json!({
            "valid": false, "held": false, "note": p.note, "name": null, "label": p.label,
            "challenge": p.challenge, "live": false, "block_height": p.height,
            "block_hash": p.block_hash, "age_blocks": null, "max_age_blocks": 0,
            "scanned_to": null, "reason": null,
        });
        let refused = if let Err(e) = p.verify_signature() {
            Some(format!("{e:#}"))
        } else if p.network()? != network {
            Some(format!("the proof is for the {} network", p.network))
        } else {
            match (challenge, p.challenge.as_deref()) {
                (Some(want), Some(got)) if want == got => None,
                (Some(_), Some(_)) => Some("the proof answers another code; it was made for a different check".into()),
                (Some(_), None) => Some("you asked for a live check, but the proof carries no code: it is a shared proof".into()),
                (None, _) => None,
            }
        };
        if let Some(reason) = refused {
            report["reason"] = json!(reason);
            return Ok(report);
        }
        report["live"] = json!(challenge.is_some());
        // The block the proof names must be on this chain with that hash, and recent.
        let tip = self.relay.tip().await?;
        if p.height > tip {
            report["reason"] = json!(format!(
                "the proof names block {}, which the server does not have yet (its tip is {tip}); try again in a minute",
                p.height
            ));
            return Ok(report);
        }
        let block = self.relay.blocks(p.height, p.height).await?;
        let hash = block
            .first()
            .and_then(|b| zcash_primitives::block::BlockHash::try_from_slice(&b.hash))
            .map(|h| h.to_string())
            .ok_or_else(|| anyhow!("the relay sent no block {}", p.height))?;
        if hash != p.block_hash {
            report["reason"] = json!(format!(
                "block {} on this chain has another hash: the proof names a block that is not on this chain (another chain, a reorganised block, or an altered proof)",
                p.height
            ));
            return Ok(report);
        }
        let max_age =
            zsettle_core::reference_blocks(&network, tip, DEFAULT_MAX_AGE_REFERENCE_BLOCKS);
        report["max_age_blocks"] = json!(max_age);
        let age = tip.saturating_sub(p.height);
        report["age_blocks"] = json!(age);
        if age > max_age {
            report["reason"] = json!(format!(
                "the proof was made {age} blocks ago; this check accepts proofs up to {max_age} blocks old, so ask for a fresh one"
            ));
            return Ok(report);
        }
        report["valid"] = json!(true);
        let prover = Consignment {
            label: "prover".into(),
            fvk: p.item_fvk()?,
            birthday: p.item_birthday,
        };
        let trace = match self
            .trace(network, std::slice::from_ref(&prover), p.note_ref()?, cb)
            .await
        {
            Ok(t) => t,
            Err(e) => {
                if let Some(missing) = e.downcast_ref::<NoteNotFound>() {
                    report["reason"] = json!(format!(
                        "the note is not on chain as a note of the proof's key: {missing}"
                    ));
                    return Ok(report);
                }
                return Err(e.context("tracing the proof's note"));
            }
        };
        report["name"] = json!(trace.name);
        report["scanned_to"] = json!(trace.scanned_to);
        match (&trace.status, trace.hops.len()) {
            (Status::Held { .. }, 1) => report["held"] = json!(true),
            (Status::Held { .. }, _) => {
                report["reason"] = json!(format!(
                    "the note was spent in {}; the item moved to another note of the same account, so ask for a proof naming the current note",
                    trace.hops[1].txid
                ))
            }
            (Status::TransferredToUnknownHolder, _) => {
                report["reason"] = json!(format!(
                    "the note was spent in {}; the item moved to another holder",
                    trace.hops.last().map(|h| h.txid.as_str()).unwrap_or("?")
                ))
            }
            (Status::LineageBroken { txid }, _) => {
                report["reason"] = json!(format!("the note was spent in {txid}, outside a trade"))
            }
        }
        Ok(report)
    }
}
