//! Keyless provenance verification from chain data.
//!
//! Anyone holding an item's consignments (the full viewing keys of the item accounts it has
//! passed through) can trace the item without any spending key:
//!
//! 1. stream compact blocks from the earliest consignment's birthday and trial-decrypt
//!    Ironwood actions until the genesis note is found;
//! 2. derive the genesis note's nullifier with that consignment's nullifier key;
//! 3. keep streaming until an action spends that nullifier: by construction of a trade,
//!    the same action's output commitment is the successor note, now owned by the buyer;
//! 4. when another supplied consignment decrypts the successor, continue from there.
//!
//! The chain ends at a note none of the supplied keys can open (sold to a holder whose
//! consignment was not supplied) or at an unspent note one of them owns. The item name is
//! read from the full genesis transaction's memo.

use anyhow::{anyhow, Result};
use orchard::keys::{FullViewingKey, PreparedIncomingViewingKey, Scope};
use orchard::note::{ExtractedNoteCommitment, Nullifier};
use orchard::note_encryption::{CompactAction, IronwoodDomain, COMPACT_NOTE_SIZE};
use serde::Serialize;
use zcash_client_backend::proto::service::{
    compact_tx_streamer_client::CompactTxStreamerClient, BlockId, BlockRange, TxFilter,
};
use zcash_note_encryption::{try_compact_note_decryption, try_note_decryption, EphemeralKeyBytes};
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::{BlockHeight, BranchId, Network};
use zcash_protocol::memo::{Memo, MemoBytes};

use crate::broadcast::{connect, install_crypto_provider};
use crate::wallet::NoteRef;

/// A consignment: one item account's full viewing key and the height to scan from.
#[derive(Clone)]
pub struct Consignment {
    /// Name shown in the report (for example the listings file it came from).
    pub label: String,
    /// The item account's full viewing key.
    pub fvk: FullViewingKey,
    /// Height from which this account's notes can appear.
    pub birthday: u32,
}

/// One step in an item's history.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Hop {
    /// Transaction that created this hop's note.
    pub txid: String,
    /// Height of that transaction.
    pub height: u32,
    /// Note commitment of this hop's note.
    pub cmx: String,
    /// Value of the note, when a supplied key could decrypt it.
    pub value_zat: Option<u64>,
    /// Label of the consignment that decrypts this note; `None` for an unknown holder.
    pub holder: Option<String>,
}

/// Where the item is after the last hop.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Status {
    /// The last hop is unspent as of the scanned height and one of the supplied keys owns it.
    Held {
        /// Label of the consignment that owns the last hop.
        by: String,
    },
    /// The last hop was spent into a note none of the supplied keys can decrypt.
    TransferredToUnknownHolder,
}

/// Full provenance report.
#[derive(Clone, Debug, Serialize)]
pub struct Provenance {
    /// Item name from the genesis memo, if readable.
    pub name: Option<String>,
    /// Genesis note reference.
    pub genesis: String,
    /// Labels of the consignments used, in the order supplied.
    pub keys: Vec<String>,
    /// Hops in order, starting with the genesis note.
    pub hops: Vec<Hop>,
    /// Status after the last hop.
    pub status: Status,
    /// Last height scanned.
    pub scanned_to: u32,
}

struct Key {
    label: String,
    fvk: FullViewingKey,
    external: PreparedIncomingViewingKey,
    internal: PreparedIncomingViewingKey,
}

struct Keyring(Vec<Key>);

impl Keyring {
    fn new(consignments: &[Consignment]) -> Self {
        Keyring(
            consignments
                .iter()
                .map(|c| Key {
                    label: c.label.clone(),
                    fvk: c.fvk.clone(),
                    external: PreparedIncomingViewingKey::new(&c.fvk.to_ivk(Scope::External)),
                    internal: PreparedIncomingViewingKey::new(&c.fvk.to_ivk(Scope::Internal)),
                })
                .collect(),
        )
    }

    /// The first supplied key that decrypts the action's output, with the note.
    fn decrypt(&self, act: &CompactAction) -> Option<(usize, orchard::Note)> {
        let domain = IronwoodDomain::for_compact_action(act);
        self.0.iter().enumerate().find_map(|(i, k)| {
            try_compact_note_decryption(&domain, &k.external, act)
                .or_else(|| try_compact_note_decryption(&domain, &k.internal, act))
                .map(|(note, _)| (i, note))
        })
    }
}

/// Trace an item from its genesis note with the supplied consignments. Scanning starts at
/// the earliest birthday; `to_height` defaults to the server's tip.
pub fn trace_blocking(
    server: &str,
    network: Network,
    consignments: &[Consignment],
    genesis: NoteRef,
    to_height: Option<u32>,
) -> Result<Provenance> {
    install_crypto_provider();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(trace(server, network, consignments, genesis, to_height))
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
    consignments: &[Consignment],
    genesis: NoteRef,
    to_height: Option<u32>,
) -> Result<Provenance> {
    if consignments.is_empty() {
        return Err(anyhow!(
            "at least one consignment (listings file) is required"
        ));
    }
    let ring = Keyring::new(consignments);
    let from_height = consignments.iter().map(|c| c.birthday).min().unwrap_or(0);

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
    // Nullifier of the hop we are watching for a spend, and the key that owns it.
    let mut watch: Option<(Nullifier, usize)> = None;
    let mut scanned_to = from_height;

    'blocks: while let Some(block) = stream.message().await.map_err(|e| anyhow!("stream: {e}"))? {
        let height = u32::try_from(block.height)?;
        scanned_to = height;
        for vtx in &block.vtx {
            let txid = txid_hex(&vtx.txid);
            for (i, a) in vtx.ironwood_actions.iter().enumerate() {
                let act = compact_action(a)?;
                if hops.is_empty() {
                    // Still looking for the genesis note.
                    if txid == genesis_txid && i as u32 == genesis.index {
                        let Some((k, note)) = ring.decrypt(&act) else {
                            return Err(anyhow!(
                                "the genesis note does not decrypt with any supplied consignment"
                            ));
                        };
                        hops.push(Hop {
                            txid: txid.clone(),
                            height,
                            cmx: hex::encode(act.cmx().to_bytes()),
                            value_zat: Some(note.value().inner()),
                            holder: Some(ring.0[k].label.clone()),
                        });
                        watch = Some((note.nullifier(&ring.0[k].fvk), k));
                    }
                    continue;
                }
                let Some((nf, _)) = watch else { break 'blocks };
                if act.nullifier() != nf {
                    continue;
                }
                // Spent: the successor note is this action's output.
                match ring.decrypt(&act) {
                    Some((k, note)) => {
                        hops.push(Hop {
                            txid: txid.clone(),
                            height,
                            cmx: hex::encode(act.cmx().to_bytes()),
                            value_zat: Some(note.value().inner()),
                            holder: Some(ring.0[k].label.clone()),
                        });
                        watch = Some((note.nullifier(&ring.0[k].fvk), k));
                    }
                    None => {
                        hops.push(Hop {
                            txid: txid.clone(),
                            height,
                            cmx: hex::encode(act.cmx().to_bytes()),
                            value_zat: None,
                            holder: None,
                        });
                        watch = None;
                        break 'blocks;
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
    let status = match watch {
        Some((_, k)) => Status::Held {
            by: ring.0[k].label.clone(),
        },
        None => Status::TransferredToUnknownHolder,
    };

    // Item name from the genesis transaction's memo, read with the key that owns the genesis note.
    let genesis_key = ring
        .0
        .iter()
        .find(|k| Some(&k.label) == hops[0].holder.as_ref())
        .map(|k| &k.fvk);
    let name = match genesis_key {
        Some(fvk) => genesis_name(&mut client, network, fvk, genesis, hops[0].height)
            .await
            .ok()
            .flatten(),
        None => None,
    };

    Ok(Provenance {
        name,
        genesis: format!("{}:{}", genesis.txid, genesis.index),
        keys: ring.0.iter().map(|k| k.label.clone()).collect(),
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
