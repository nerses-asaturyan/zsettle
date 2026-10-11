//! Access to a light-wallet database and its encrypted seed.
//!
//! The on-disk layout is the one `zcash-devtool` creates: `data.sqlite` holds the wallet,
//! `keys.toml` holds the network, birthday and an age-encrypted mnemonic, and an age
//! identity file decrypts that mnemonic.

use std::{
    fs,
    io::Read,
    num::NonZeroU32,
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::{anyhow, Context, Result};
use bip0039::{English, Mnemonic};
use rand_core::OsRng;
use secrecy::{ExposeSecret, SecretVec, Zeroize};
use serde::Deserialize;
use zcash_client_backend::data_api::{
    chain::{error as chain_error, BlockCache, BlockSource},
    locking::LockFilter,
    scanning::ScanRange,
    wallet::TargetHeight,
    Account as _, AccountBirthday, AccountPurpose, InputSource, NullifierQuery, WalletRead,
    WalletWrite,
};
use zcash_client_backend::proto::compact_formats::CompactBlock;
use zcash_client_backend::wallet::Note;
use zcash_client_sqlite::{util::SystemClock, AccountUuid, WalletDb};
use zcash_keys::keys::{UnifiedFullViewingKey, UnifiedSpendingKey};
use zcash_protocol::{
    consensus::{BlockHeight, Network},
    ShieldedPool, TxId,
};

use crate::broadcast::{connect, install_crypto_provider};

/// The wallet database type used throughout zsettle.
pub type Db = WalletDb<rusqlite::Connection, Network, SystemClock, OsRng>;

/// A reference to a shielded note by the transaction that created it and its output index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoteRef {
    /// Transaction id.
    pub txid: TxId,
    /// Index of the output within the transaction's Ironwood bundle.
    pub index: u32,
}

impl std::str::FromStr for NoteRef {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        let (tx, idx) = s
            .split_once(':')
            .ok_or_else(|| anyhow!("note reference must be <txid>:<output index>"))?;
        Ok(NoteRef {
            txid: parse_txid(tx)?,
            index: idx.parse().context("output index")?,
        })
    }
}

/// Parse a transaction id in the display (byte-reversed hex) form.
pub fn parse_txid(s: &str) -> Result<TxId> {
    let mut bytes: [u8; 32] = hex::decode(s.trim())
        .context("txid hex")?
        .try_into()
        .map_err(|_| anyhow!("txid must be 32 bytes"))?;
    bytes.reverse();
    Ok(TxId::from_bytes(bytes))
}

/// Parse an account UUID as printed by `zcash-devtool wallet list-accounts`.
pub fn parse_account(s: &str) -> Result<AccountUuid> {
    Ok(AccountUuid::from_uuid(
        uuid::Uuid::parse_str(s.trim()).context("account uuid")?,
    ))
}

#[derive(Deserialize)]
struct KeysToml {
    mnemonic: Option<String>,
    network: Option<String>,
    birthday: Option<u32>,
}

/// A devtool-layout wallet directory.
pub struct Wallet {
    /// Directory containing `data.sqlite` and `keys.toml`.
    pub dir: PathBuf,
    /// Network the wallet was created for.
    pub network: Network,
    /// Height the wallet started scanning from.
    pub birthday: u32,
    mnemonic_ciphertext: Option<String>,
}

impl Wallet {
    /// Open a wallet directory and read its metadata (no secrets are decrypted).
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        let keys: KeysToml = toml::from_str(
            &fs::read_to_string(dir.join("keys.toml")).context("reading keys.toml")?,
        )
        .context("parsing keys.toml")?;
        let network = match keys.network.as_deref() {
            Some("main") => Network::MainNetwork,
            Some("test") => Network::TestNetwork,
            other => return Err(anyhow!("unsupported network in keys.toml: {other:?}")),
        };
        Ok(Wallet {
            dir,
            network,
            birthday: keys.birthday.unwrap_or(0),
            mnemonic_ciphertext: keys.mnemonic,
        })
    }

    /// Open the SQLite wallet database.
    pub fn db(&self) -> Result<Db> {
        WalletDb::for_path(
            self.dir.join("data.sqlite"),
            self.network,
            SystemClock,
            OsRng,
        )
        .context("opening wallet database")
    }

    /// Decrypt the mnemonic with the given age identity file and derive the BIP 39 seed.
    pub fn seed(&self, identity_file: &Path) -> Result<SecretVec<u8>> {
        let ciphertext = self
            .mnemonic_ciphertext
            .as_deref()
            .ok_or_else(|| anyhow!("wallet has no encrypted mnemonic (view-only wallet?)"))?;
        let identities = age::IdentityFile::from_file(identity_file.to_string_lossy().into_owned())
            .context("reading age identity file")?
            .into_identities()
            .map_err(|e| anyhow!("age identity file: {e}"))?;
        let decryptor = age::Decryptor::new(age::armor::ArmoredReader::new(ciphertext.as_bytes()))
            .map_err(|e| anyhow!("age decryptor: {e}"))?;
        let mut buf = vec![];
        let read = decryptor
            .decrypt(identities.iter().map(|i| i.as_ref() as &dyn age::Identity))
            .map_err(|e| anyhow!("age decrypt: {e}"))?
            .read_to_end(&mut buf);
        let mnemonic_bytes = SecretVec::new(buf);
        read?;
        let phrase = std::str::from_utf8(mnemonic_bytes.expose_secret())?;
        let mut seed_bytes = <Mnemonic<English>>::from_phrase(phrase)
            .map_err(|e| anyhow!("mnemonic: {e}"))?
            .to_seed("");
        let seed = SecretVec::new(seed_bytes.to_vec());
        seed_bytes.zeroize();
        Ok(seed)
    }

    /// Derive the unified spending key of a ZIP 32 account of this wallet.
    pub fn spending_key(
        &self,
        seed: &SecretVec<u8>,
        account_index: u32,
    ) -> Result<UnifiedSpendingKey> {
        let account = zip32::AccountId::try_from(account_index)
            .map_err(|_| anyhow!("invalid account index {account_index}"))?;
        UnifiedSpendingKey::from_seed(&self.network, seed.expose_secret(), account)
            .map_err(|e| anyhow!("deriving spending key: {e:?}"))
    }
}

/// Target height (next block) and anchor height for the given confirmation requirement.
pub fn heights(
    db: &Db,
    min_confirmations: u32,
) -> Result<(TargetHeight, zcash_protocol::consensus::BlockHeight)> {
    let min = NonZeroU32::new(min_confirmations.max(1)).expect("non-zero");
    db.get_target_and_anchor_heights(min)
        .map_err(|e| anyhow!("{e}"))?
        .ok_or_else(|| anyhow!("wallet has not synced any blocks yet"))
}

/// The unified full viewing key of an account.
pub fn account_ufvk(db: &Db, account: AccountUuid) -> Result<UnifiedFullViewingKey> {
    let acct = db
        .get_account(account)
        .map_err(|e| anyhow!("{e}"))?
        .ok_or_else(|| anyhow!("account {} not found in wallet", account.expose_uuid()))?;
    acct.ufvk()
        .cloned()
        .ok_or_else(|| anyhow!("account {} has no full viewing key", account.expose_uuid()))
}

/// An Ironwood note the wallet knows about, with its tree position.
pub struct IronwoodNote {
    /// The note itself.
    pub note: orchard::Note,
    /// Position in the Ironwood note commitment tree.
    pub position: incrementalmerkletree::Position,
}

/// Look up a specific Ironwood note. `None` when the wallet has no spendable note with this
/// reference at the target height: it is unknown to the wallet, or already spent.
pub fn find_ironwood_note(
    db: &Db,
    r: NoteRef,
    target_height: TargetHeight,
) -> Result<Option<IronwoodNote>> {
    let Some(received) = db
        .get_spendable_note(
            &r.txid,
            ShieldedPool::Ironwood,
            r.index,
            target_height,
            LockFilter::Unfiltered,
        )
        .map_err(|e| anyhow!("{e}"))?
    else {
        return Ok(None);
    };
    let note = match received.note() {
        Note::Orchard {
            note,
            pool: orchard::ValuePool::Ironwood,
        } => *note,
        Note::Orchard { pool, .. } => {
            return Err(anyhow!("note is in the {pool:?} pool, not Ironwood"))
        }
        Note::Sapling(_) => return Err(anyhow!("note is a Sapling note")),
    };
    Ok(Some(IronwoodNote {
        note,
        position: received.note_commitment_tree_position(),
    }))
}

/// Look up a specific Ironwood note by transaction id and output index.
pub fn ironwood_note(db: &Db, r: NoteRef, target_height: TargetHeight) -> Result<IronwoodNote> {
    find_ironwood_note(db, r, target_height)?.ok_or_else(|| {
        anyhow!(
            "note {}:{} not found or not spendable at this height",
            r.txid,
            r.index
        )
    })
}

/// Highest block the wallet has scanned, if any.
pub fn max_scanned_height(db: &Db) -> Result<Option<u32>> {
    Ok(db
        .block_max_scanned()
        .map_err(|e| anyhow!("{e}"))?
        .map(|m| u32::from(m.block_height())))
}

/// Nullifiers of every unspent Ironwood note in this wallet, across all accounts.
pub fn unspent_ironwood_nullifiers(db: &Db) -> Result<Vec<orchard::note::Nullifier>> {
    Ok(db
        .get_ironwood_nullifiers(NullifierQuery::Unspent)
        .map_err(|e| anyhow!("{e}"))?
        .into_iter()
        .map(|(_, nf)| nf)
        .collect())
}

/// Import a unified full viewing key as a view-only account, scanning from `birthday`.
pub fn import_viewing_key(
    db: &mut Db,
    network: Network,
    name: &str,
    ufvk_str: &str,
    birthday: u32,
    server: &str,
) -> Result<AccountUuid> {
    let ufvk = UnifiedFullViewingKey::decode(&network, ufvk_str)
        .map_err(|e| anyhow!("decoding viewing key: {e}"))?;
    // The tree state one block before the birthday lets the scanner start exactly there.
    let tree_state =
        crate::broadcast::fetch_tree_state_blocking(server, birthday.saturating_sub(1))?;
    let account_birthday = AccountBirthday::from_treestate(tree_state, None)
        .map_err(|e| anyhow!("birthday: {e:?}"))?;
    let account = db
        .import_account_ufvk(
            name,
            &ufvk,
            &account_birthday,
            AccountPurpose::ViewOnly,
            None,
        )
        .map_err(|e| anyhow!("importing viewing key: {e}"))?;
    Ok(account.id())
}

/// Find the account whose unified full viewing key matches the given encoding.
pub fn find_account_by_ufvk(
    db: &Db,
    network: Network,
    ufvk_str: &str,
) -> Result<Option<AccountUuid>> {
    for id in db.get_account_ids().map_err(|e| anyhow!("{e}"))? {
        if let Ok(ufvk) = account_ufvk(db, id) {
            if ufvk.encode(&network) == ufvk_str {
                return Ok(Some(id));
            }
        }
    }
    Ok(None)
}

/// The largest spendable Ironwood note of an account, if any.
pub fn largest_spendable_note(
    db: &Db,
    account: AccountUuid,
    target_height: TargetHeight,
) -> Result<Option<NoteRef>> {
    use zcash_client_backend::data_api::{wallet::ConfirmationsPolicy, MaxSpendMode, TargetValue};
    let notes = db
        .select_spendable_notes(
            account,
            TargetValue::AllFunds(MaxSpendMode::MaxSpendable),
            &[ShieldedPool::Ironwood],
            target_height,
            ConfirmationsPolicy::default(),
            &[],
            LockFilter::Unfiltered,
        )
        .map_err(|e| anyhow!("{e}"))?;
    let best = notes
        .ironwood()
        .iter()
        .max_by_key(|n| n.note().value().inner())
        .map(|n| NoteRef {
            txid: *n.txid(),
            index: u32::from(n.output_index()),
        });
    Ok(best)
}

// ---- Synchronisation ----------------------------------------------------------------

/// Blocks fetched per batch while syncing; each batch is scanned and dropped before the next.
const SYNC_BATCH_BLOCKS: u32 = 500;

/// Error type of the in-memory block cache. The cache cannot fail; the traits need a type.
#[derive(Debug)]
pub struct CacheError;

impl std::fmt::Display for CacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("block cache error")
    }
}

impl std::error::Error for CacheError {}

/// Compact blocks held in memory while they are scanned. The synchronizer downloads a
/// batch, scans it and deletes it, so the cache never holds more than one batch and
/// nothing besides the wallet database touches the disk.
#[derive(Default)]
pub struct MemBlockCache {
    blocks: Mutex<Vec<CompactBlock>>,
}

fn height_of(block: &CompactBlock) -> BlockHeight {
    BlockHeight::from_u32(u32::try_from(block.height).unwrap_or(u32::MAX))
}

impl MemBlockCache {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<CompactBlock>> {
        self.blocks.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl BlockSource for MemBlockCache {
    type Error = CacheError;

    fn with_blocks<F, WalletErrT>(
        &self,
        from_height: Option<BlockHeight>,
        limit: Option<usize>,
        mut with_block: F,
    ) -> Result<(), chain_error::Error<WalletErrT, Self::Error>>
    where
        F: FnMut(CompactBlock) -> Result<(), chain_error::Error<WalletErrT, Self::Error>>,
    {
        let mut blocks: Vec<CompactBlock> = self
            .lock()
            .iter()
            .filter(|b| match from_height {
                Some(h) => height_of(b) >= h,
                None => true,
            })
            .cloned()
            .collect();
        blocks.sort_by_key(|b| b.height);
        for b in blocks.into_iter().take(limit.unwrap_or(usize::MAX)) {
            with_block(b)?;
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl BlockCache for MemBlockCache {
    fn get_tip_height(
        &self,
        range: Option<&ScanRange>,
    ) -> Result<Option<BlockHeight>, Self::Error> {
        Ok(self
            .lock()
            .iter()
            .map(height_of)
            .filter(|h| match range {
                Some(r) => r.block_range().contains(h),
                None => true,
            })
            .max())
    }

    async fn read(&self, range: &ScanRange) -> Result<Vec<CompactBlock>, Self::Error> {
        let mut blocks: Vec<CompactBlock> = self
            .lock()
            .iter()
            .filter(|b| range.block_range().contains(&height_of(b)))
            .cloned()
            .collect();
        blocks.sort_by_key(|b| b.height);
        Ok(blocks)
    }

    async fn insert(&self, compact_blocks: Vec<CompactBlock>) -> Result<(), Self::Error> {
        let mut guard = self.lock();
        for b in compact_blocks {
            guard.retain(|x| x.height != b.height);
            guard.push(b);
        }
        Ok(())
    }

    async fn delete(&self, range: ScanRange) -> Result<(), Self::Error> {
        self.lock()
            .retain(|b| !range.block_range().contains(&height_of(b)));
        Ok(())
    }
}

/// Outcome of a sync pass.
#[derive(Clone, Debug, serde::Serialize)]
pub struct SyncReport {
    /// Chain tip the server reported.
    pub chain_tip: Option<u32>,
    /// Highest block the wallet has scanned.
    pub scanned_to: Option<u32>,
}

/// Bring the wallet up to date with a lightwalletd server: refresh the commitment-tree
/// roots and the chain tip, then download and scan every block range the wallet still
/// needs. Blocks are cached in memory only.
pub fn sync_blocking(wallet: &Wallet, db: &mut Db, server: &str) -> Result<SyncReport> {
    install_crypto_provider();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let mut client = connect(server).await?;
        let cache = MemBlockCache::default();
        zcash_client_backend::sync::run(
            &mut client,
            &wallet.network,
            &cache,
            &mut *db,
            SYNC_BATCH_BLOCKS,
        )
        .await
        .map_err(|e| anyhow!("sync: {e}"))
    })?;
    Ok(SyncReport {
        chain_tip: db
            .chain_height()
            .map_err(|e| anyhow!("{e}"))?
            .map(u32::from),
        scanned_to: max_scanned_height(db)?,
    })
}
