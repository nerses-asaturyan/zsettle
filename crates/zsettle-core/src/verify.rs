//! Keyless provenance verification from chain data.
//!
//! Anyone holding an item's consignment (the item account's full viewing key) can trace
//! the item without any spending key:
//!
//! 1. stream compact blocks from the item's birthday and trial-decrypt Ironwood actions
//!    with the consignment's incoming viewing key until the genesis note is found;
//! 2. derive the genesis note's nullifier with the consignment's nullifier key;
//! 3. keep streaming until an action spends that nullifier: by construction of a trade,
//!    the same action's output commitment is the successor note, now owned by the buyer.
//!
//! The successor belongs to a new holder, so tracing stops there unless that holder's
//! consignment is supplied too. The genesis name is read from the full genesis transaction.

use anyhow::{anyhow, Result};
use orchard::keys::{FullViewingKey, PreparedIncomingViewingKey, Scope};
use orchard::note::{ExtractedNoteCommitment, Nullifier};
use orchard::note_encryption::{CompactAction, IronwoodDomain};
use serde::Serialize;
use zcash_client_backend::proto::service::{
    compact_tx_streamer_client::CompactTxStreamerClient, BlockId, BlockRange, TxFilter,
};
use zcash_note_encryption::{
    try_compact_note_decryption, try_note_decryption, EphemeralKeyBytes, COMPACT_NOTE_SIZE,
};
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::{BlockHeight, BranchId, Network};
use zcash_protocol::memo::{Memo, MemoBytes};

use crate::wallet::NoteRef;

/// One step in an item's history.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Hop {
    /// Transaction that created this hop's note.
    pub txid: String,
    /// Height of that transaction.
    pub height: u32,
    /// Note commitment of this hop's note.
    pub cmx: String,
    /// Value of the note, when the verifier could decrypt it.
    pub value_zat: Option<u64>,
    /// Whether the verifier could decrypt this note (it holds this hop's viewing key).
    pub decryptable: bool,
}

/// Result of walking an item's history.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub enum Status {
    /// The last known hop is unspent as of the scanned height; held by the account whose key was used.
    CurrentlyHeld,
    /// The last known hop was spent into a successor the verifier cannot decrypt (sold or moved).
    TransferredToUnknownHolder,
    /// The last known hop was spent into a successor it could decrypt (same key, internal move).
    MovedWithinAccount,
}

/// Full provenance report.
#[derive(Clone, Debug, Serialize)]
pub struct Provenance {
    /// Item name from the genesis memo, if readable.
    pub name: Option<String>,
    /// Genesis note reference.
    pub genesis: String,
    /// Hops in order, starting with the genesis note.
    pub hops: Vec<Hop>,
    /// Status after the last hop.
    pub status: Status,
    /// Last height scanned.
    pub scanned_to: u32,
}

fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Trace an item from its genesis note. `from_height` is where scanning starts (the
/// consignment's birthday); `to_height` defaults to the server's tip.
pub fn trace_blocking(
    server: &str,
    network: Network,
    fvk: &FullViewingKey,
    genesis: NoteRef,
    from_height: u32,
    to_height: Option<u32>,
) -> Result<Provenance> {
    install_crypto_provider();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(trace(server, network, fvk, genesis, from_height, to_height))
}

async fn connect(server: &str) -> Result<CompactTxStreamerClient<tonic::transport::Channel>> {
    let uri: tonic::transport::Uri = server.parse().map_err(|e| anyhow!("server url: {e}"))?;
    let host = uri
        .host()
        .ok_or_else(|| anyhow!("server url has no host"))?
        .to_string();
    let mut endpoint = tonic::transport::Channel::builder(uri.clone());
    if uri.scheme_str() == Some("https") {
        endpoint = endpoint
            .tls_config(
                tonic::transport::ClientTlsConfig::new()
                    .domain_name(host)
                    .with_webpki_roots(),
            )
            .map_err(|e| anyhow!("tls config: {e}"))?;
    }
    let channel = endpoint
        .connect()
        .await
        .map_err(|e| anyhow!("connecting to {server}: {e}"))?;
    Ok(CompactTxStreamerClient::new(channel))
}

fn compact_action(
    a: &zcash_client_backend::proto::compact_formats::CompactOrchardAction,
) -> Result<CompactAction> {
    let nf: [u8; 32] = a
        .nullifier
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("bad nullifier length"))?;
    let cmx: [u8; 32] = a
        .cmx
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("bad cmx length"))?;
    let epk: [u8; 32] = a
        .ephemeral_key
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("bad epk length"))?;
    let mut ct = [0u8; COMPACT_NOTE_SIZE];
    ct.copy_from_slice(
        a.ciphertext
            .get(..COMPACT_NOTE_SIZE)
            .ok_or_else(|| anyhow!("short ciphertext"))?,
    );
    let nf =
        Option::from(Nullifier::from_bytes(&nf)).ok_or_else(|| anyhow!("invalid nullifier"))?;
    let cmx = Option::from(ExtractedNoteCommitment::from_bytes(&cmx))
        .ok_or_else(|| anyhow!("invalid cmx"))?;
    Ok(CompactAction::from_parts(
        nf,
        cmx,
        EphemeralKeyBytes(epk),
        ct,
    ))
}

fn txid_hex(bytes: &[u8]) -> String {
    let mut b = bytes.to_vec();
    b.reverse();
    hex::encode(b)
}

async fn trace(
    server: &str,
    network: Network,
    fvk: &FullViewingKey,
    genesis: NoteRef,
    from_height: u32,
    to_height: Option<u32>,
) -> Result<Provenance> {
    let mut client = connect(server).await?;
    let tip = match to_height {
        Some(h) => h,
        None => {
            let id = client
                .get_latest_block(zcash_client_backend::proto::service::ChainSpec {})
                .await
                .map_err(|e| anyhow!("get_latest_block: {e}"))?
                .into_inner();
            u32::try_from(id.height)?
        }
    };
    let pivk = PreparedIncomingViewingKey::new(&fvk.to_ivk(Scope::External));
    let pivk_int = PreparedIncomingViewingKey::new(&fvk.to_ivk(Scope::Internal));
    let genesis_txid = genesis.txid.to_string();

    let mut stream = client
        .get_block_range(BlockRange {
            start: Some(BlockId {
                height: u64::from(from_height),
                hash: vec![],
            }),
            end: Some(BlockId {
                height: u64::from(tip),
                hash: vec![],
            }),
            pool_types: vec![],
        })
        .await
        .map_err(|e| anyhow!("get_block_range: {e}"))?
        .into_inner();

    let mut hops: Vec<Hop> = Vec::new();
    // The nullifier of the hop we are currently watching for a spend.
    let mut watch: Option<Nullifier> = None;
    let mut status = Status::CurrentlyHeld;
    let mut scanned_to = from_height;

    while let Some(block) = stream.message().await.map_err(|e| anyhow!("stream: {e}"))? {
        let height = u32::try_from(block.height)?;
        scanned_to = height;
        for vtx in &block.vtx {
            let txid = txid_hex(&vtx.txid);
            for (i, a) in vtx.ironwood_actions.iter().enumerate() {
                let act = compact_action(a)?;
                if hops.is_empty() {
                    // Still looking for the genesis note.
                    if txid == genesis_txid && i as u32 == genesis.index {
                        let domain = IronwoodDomain::for_compact_action(&act);
                        let Some((note, _)) = try_compact_note_decryption(&domain, &pivk, &act)
                        else {
                            return Err(anyhow!(
                                "the genesis note does not decrypt with this consignment key"
                            ));
                        };
                        hops.push(Hop {
                            txid: txid.clone(),
                            height,
                            cmx: hex::encode(act.cmx().to_bytes()),
                            value_zat: Some(note.value().inner()),
                            decryptable: true,
                        });
                        watch = Some(note.nullifier(fvk));
                    }
                    continue;
                }
                if let Some(nf) = watch {
                    if act.nullifier() == nf {
                        // Spent: the successor is this action's output.
                        let domain = IronwoodDomain::for_compact_action(&act);
                        let decrypted = try_compact_note_decryption(&domain, &pivk, &act)
                            .or_else(|| try_compact_note_decryption(&domain, &pivk_int, &act));
                        match decrypted {
                            Some((note, _)) => {
                                hops.push(Hop {
                                    txid: txid.clone(),
                                    height,
                                    cmx: hex::encode(act.cmx().to_bytes()),
                                    value_zat: Some(note.value().inner()),
                                    decryptable: true,
                                });
                                watch = Some(note.nullifier(fvk));
                                status = Status::MovedWithinAccount;
                            }
                            None => {
                                hops.push(Hop {
                                    txid: txid.clone(),
                                    height,
                                    cmx: hex::encode(act.cmx().to_bytes()),
                                    value_zat: None,
                                    decryptable: false,
                                });
                                watch = None;
                                status = Status::TransferredToUnknownHolder;
                            }
                        }
                    }
                }
            }
        }
    }
    if hops.is_empty() {
        return Err(anyhow!(
            "genesis note {}:{} not found between heights {from_height} and {tip}",
            genesis.txid,
            genesis.index
        ));
    }
    if watch.is_some() {
        // The last hop decrypts with this key and is unspent: the key holder has it.
        status = Status::CurrentlyHeld;
    }

    // Item name from the genesis transaction's memo.
    let name = genesis_name(&mut client, network, fvk, genesis, hops[0].height)
        .await
        .ok()
        .flatten();

    Ok(Provenance {
        name,
        genesis: format!("{}:{}", genesis.txid, genesis.index),
        hops,
        status,
        scanned_to,
    })
}

async fn genesis_name(
    client: &mut CompactTxStreamerClient<tonic::transport::Channel>,
    network: Network,
    fvk: &FullViewingKey,
    genesis: NoteRef,
    height: u32,
) -> Result<Option<String>> {
    let raw = client
        .get_transaction(TxFilter {
            block: None,
            index: 0,
            hash: genesis.txid.as_ref().to_vec(),
        })
        .await
        .map_err(|e| anyhow!("get_transaction: {e}"))?
        .into_inner();
    let branch = BranchId::for_height(&network, BlockHeight::from(height));
    let tx = Transaction::read(raw.data.as_slice(), branch)
        .map_err(|e| anyhow!("parsing genesis tx: {e}"))?;
    let bundle = match tx.ironwood_bundle() {
        Some(b) => b,
        None => return Ok(None),
    };
    let pivk = PreparedIncomingViewingKey::new(&fvk.to_ivk(Scope::External));
    let action = bundle
        .actions()
        .get(genesis.index as usize)
        .ok_or_else(|| anyhow!("no such action"))?;
    let domain = IronwoodDomain::for_action(action);
    let Some((_note, _addr, memo)) = try_note_decryption(&domain, &pivk, action) else {
        return Ok(None);
    };
    let memo = Memo::try_from(MemoBytes::from_bytes(&memo).map_err(|e| anyhow!("memo: {e}"))?)
        .map_err(|e| anyhow!("memo: {e}"))?;
    Ok(match memo {
        Memo::Text(t) => Some(
            t.to_string()
                .trim_start_matches("zsettle item: ")
                .to_string(),
        ),
        _ => None,
    })
}
