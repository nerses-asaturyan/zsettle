//! Provenance verification from chain data with viewing keys only.
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
use zcash_client_backend::proto::compact_formats::CompactBlock;
#[cfg(feature = "native")]
use zcash_client_backend::proto::service::{BlockId, BlockRange, TxFilter};
use zcash_note_encryption::{try_compact_note_decryption, try_note_decryption, EphemeralKeyBytes};
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::{BlockHeight, BranchId, Network};
use zcash_protocol::memo::{Memo, MemoBytes};

#[cfg(feature = "native")]
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
    /// The last hop was spent into a note none of the supplied keys can decrypt: sold to a
    /// holder whose listings were not supplied, or spent outside the protocol.
    TransferredToUnknownHolder,
    /// The last hop was spent by a transaction whose successor note has a different value,
    /// which a trade never does: the item was moved or spent outside the protocol.
    LineageBroken {
        /// Transaction that broke the lineage.
        txid: String,
    },
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

/// The starting note was not found with the supplied keys: it is not mined, not in the
/// scanned range, or none of the keys decrypts it. [`trace_blocking`] returns it inside its
/// error, so a caller can tell it from a failed connection with `downcast_ref`.
#[derive(Debug)]
pub struct NoteNotFound(pub String);

impl std::fmt::Display for NoteNotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NoteNotFound {}

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

/// Trace an item from its genesis note with the supplied consignments. By default the
/// server is asked where the genesis transaction is, so scanning starts at that block and
/// the item name can be read from the transaction; with `private` the server is not told which
/// item is traced: scanning starts at the earliest listing birthday (which still shows the
/// server which consignment is of interest) and the name is left out.
/// `to_height` defaults to the server's tip.
#[cfg(feature = "native")]
pub fn trace_blocking(
    server: &str,
    network: Network,
    consignments: &[Consignment],
    genesis: NoteRef,
    to_height: Option<u32>,
    private: bool,
) -> Result<Provenance> {
    install_crypto_provider();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(trace(
            server,
            network,
            consignments,
            genesis,
            to_height,
            private,
        ))
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

/// The scan of a trace, fed one compact block at a time in height order: the same for a
/// gRPC stream (the tools) and for blocks fetched through a relay (the browser).
pub struct Tracer {
    ring: Keyring,
    genesis: NoteRef,
    genesis_txid: String,
    genesis_height: Option<u32>,
    hops: Vec<Hop>,
    /// Nullifier of the hop watched for a spend, and the key that owns it.
    watch: Option<(Nullifier, usize)>,
    broken: Option<String>,
    scanned_to: u32,
    finished: bool,
}

impl Tracer {
    /// A trace of `genesis` with the given consignments. `genesis_height`, when known, is the
    /// block of the genesis transaction: past it without the note, no later block can hold it.
    pub fn new(
        consignments: &[Consignment],
        genesis: NoteRef,
        genesis_height: Option<u32>,
    ) -> Result<Self> {
        if consignments.is_empty() {
            return Err(anyhow!(
                "at least one consignment (listings file) is required"
            ));
        }
        Ok(Tracer {
            ring: Keyring::new(consignments),
            genesis,
            genesis_txid: genesis.txid.to_string(),
            genesis_height,
            hops: Vec::new(),
            watch: None,
            broken: None,
            scanned_to: 0,
            finished: false,
        })
    }

    /// Where the scan starts: the earliest consignment birthday, or the genesis block when
    /// it is known and later.
    pub fn start_height(consignments: &[Consignment], genesis_height: Option<u32>) -> u32 {
        let from = consignments.iter().map(|c| c.birthday).min().unwrap_or(0);
        genesis_height.map_or(from, |g| from.max(g))
    }

    /// Scan one block. Returns `false` when the scan can stop: the genesis block went by
    /// without the note. After the trace resolved it keeps returning `true`, so the caller
    /// reads on to the tip and the server does not learn where the item last moved.
    pub fn feed(&mut self, block: &CompactBlock) -> Result<bool> {
        let height = u32::try_from(block.height)?;
        self.scanned_to = height;
        if self.finished {
            return Ok(true);
        }
        let ring = &self.ring;
        'txs: for vtx in &block.vtx {
            let txid = txid_hex(&vtx.txid);
            for (i, a) in vtx.ironwood_actions.iter().enumerate() {
                let act = compact_action(a)?;
                if self.hops.is_empty() {
                    // Still looking for the genesis note.
                    if txid == self.genesis_txid && i as u32 == self.genesis.index {
                        let Some((k, note)) = ring.decrypt(&act) else {
                            return Err(NoteNotFound(
                                "the genesis note does not decrypt with any supplied consignment"
                                    .into(),
                            )
                            .into());
                        };
                        self.hops.push(Hop {
                            txid: txid.clone(),
                            height,
                            cmx: hex::encode(act.cmx().to_bytes()),
                            value_zat: Some(note.value().inner()),
                            holder: Some(ring.0[k].label.clone()),
                        });
                        self.watch = Some((note.nullifier(&ring.0[k].fvk), k));
                    }
                    continue;
                }
                let Some((nf, _)) = self.watch else {
                    self.finished = true;
                    break 'txs;
                };
                if act.nullifier() != nf {
                    continue;
                }
                // Spent: the successor note is this action's output.
                match ring.decrypt(&act) {
                    Some((k, note)) => {
                        let previous = self.hops.last().and_then(|h| h.value_zat);
                        self.hops.push(Hop {
                            txid: txid.clone(),
                            height,
                            cmx: hex::encode(act.cmx().to_bytes()),
                            value_zat: Some(note.value().inner()),
                            holder: Some(ring.0[k].label.clone()),
                        });
                        if previous.is_some_and(|v| v != note.value().inner()) {
                            self.broken = Some(txid.clone());
                            self.watch = None;
                            self.finished = true;
                            break 'txs;
                        }
                        self.watch = Some((note.nullifier(&ring.0[k].fvk), k));
                    }
                    None => {
                        self.hops.push(Hop {
                            txid: txid.clone(),
                            height,
                            cmx: hex::encode(act.cmx().to_bytes()),
                            value_zat: None,
                            holder: None,
                        });
                        self.watch = None;
                        self.finished = true;
                        break 'txs;
                    }
                }
            }
        }
        // The genesis transaction sits in a known block: once that block is behind without
        // the note, no later block can hold it. Stop instead of reading on to the tip, which
        // a checker handed a bad proof would otherwise do for the whole chain.
        Ok(!(self.hops.is_empty() && self.genesis_height.is_some_and(|g| height >= g)))
    }

    /// The report, with the item name read from the genesis transaction's raw bytes when
    /// given.
    pub fn finish(
        self,
        network: Network,
        from_height: u32,
        raw: Option<&[u8]>,
    ) -> Result<Provenance> {
        if self.hops.is_empty() {
            return Err(NoteNotFound(format!(
                "genesis note {}:{} not found between heights {from_height} and {}",
                self.genesis.txid, self.genesis.index, self.scanned_to
            ))
            .into());
        }
        let status = match (self.watch, self.broken) {
            (_, Some(txid)) => Status::LineageBroken { txid },
            (Some((_, k)), None) => Status::Held {
                by: self.ring.0[k].label.clone(),
            },
            (None, None) => Status::TransferredToUnknownHolder,
        };
        // Item name from the genesis transaction's memo, read with the key that owns it.
        let genesis_key = self
            .ring
            .0
            .iter()
            .find(|k| Some(&k.label) == self.hops[0].holder.as_ref())
            .map(|k| &k.fvk);
        let name = match (genesis_key, raw) {
            (Some(fvk), Some(raw)) => {
                genesis_name(network, fvk, self.genesis, self.hops[0].height, raw)
                    .ok()
                    .flatten()
            }
            _ => None,
        };
        Ok(Provenance {
            name,
            genesis: format!("{}:{}", self.genesis.txid, self.genesis.index),
            keys: self.ring.0.iter().map(|k| k.label.clone()).collect(),
            hops: self.hops,
            status,
            scanned_to: self.scanned_to,
        })
    }
}

#[cfg(feature = "native")]
async fn trace(
    server: &str,
    network: Network,
    consignments: &[Consignment],
    genesis: NoteRef,
    to_height: Option<u32>,
    private: bool,
) -> Result<Provenance> {
    if consignments.is_empty() {
        return Err(anyhow!(
            "at least one consignment (listings file) is required"
        ));
    }
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
    // Unless privacy from the server matters, ask it where the genesis transaction is: start
    // there instead of at the birthday, and keep the bytes for the name.
    let (raw, genesis_height) = if private {
        (None, None)
    } else {
        let raw = client
            .get_transaction(TxFilter {
                block: None,
                index: 0,
                hash: genesis.txid.as_ref().to_vec(),
            })
            .await
            .map_err(|e| anyhow!("the server does not know the genesis transaction: {e}"))?
            .into_inner();
        let genesis_height = u32::try_from(raw.height)?;
        if genesis_height == 0 {
            return Err(NoteNotFound("the genesis transaction is not mined yet".into()).into());
        }
        (Some(raw.data), Some(genesis_height))
    };
    let from_height = Tracer::start_height(consignments, genesis_height);
    let mut tracer = Tracer::new(consignments, genesis, genesis_height)?;
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
    while let Some(block) = stream.message().await.map_err(|e| anyhow!("stream: {e}"))? {
        if !tracer.feed(&block)? {
            break;
        }
    }
    tracer.finish(network, from_height, raw.as_deref())
}

/// The item name in the genesis note's memo, read with the key that owns the note.
pub fn genesis_name(
    network: Network,
    fvk: &FullViewingKey,
    genesis: NoteRef,
    height: u32,
    raw: &[u8],
) -> Result<Option<String>> {
    let branch = BranchId::for_height(&network, BlockHeight::from(height));
    let tx = Transaction::read(raw, branch).map_err(|e| anyhow!("parsing genesis tx: {e}"))?;
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
