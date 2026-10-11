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
use rand::rngs::ThreadRng;
use secrecy::{ExposeSecret, SecretVec, Zeroize};
use serde::{Deserialize, Serialize};
use zcash_client_backend::data_api::{
    chain::{error as chain_error, BlockCache, BlockSource},
    locking::LockFilter,
    scanning::ScanRange,
    wallet::TargetHeight,
    Account as _, AccountBirthday, AccountPurpose, InputSource, NullifierQuery,
    WalletCommitmentTrees, WalletRead, WalletWrite,
};
use zcash_client_backend::proto::compact_formats::CompactBlock;
use zcash_client_backend::wallet::Note;
use zcash_client_sqlite::{util::SystemClock, AccountUuid, WalletDb};
use zcash_keys::keys::{UnifiedAddressRequest, UnifiedFullViewingKey, UnifiedSpendingKey};
use zcash_protocol::{
    consensus::{BlockHeight, Network},
    ShieldedPool, TxId,
};

use crate::broadcast::{connect, install_crypto_provider};

/// The wallet database type used throughout zsettle.
pub type Db = WalletDb<rusqlite::Connection, Network, SystemClock, ThreadRng>;

/// A reference to a shielded note by the transaction that created it and its output index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoteRef {
    /// Transaction id.
    pub txid: TxId,
    /// Index of the output within the transaction's Ironwood bundle.
    pub index: u32,
}

impl std::fmt::Display for NoteRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.txid, self.index)
    }
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

    /// Open the SQLite wallet database and bring its schema up to date with the wallet
    /// library this build uses. A migration that needs the seed fails here; run
    /// [`Wallet::upgrade`] (the `upgrade --identity` command) once to apply it.
    pub fn db(&self) -> Result<Db> {
        let mut db = self.open_db()?;
        zcash_client_sqlite::wallet::init::init_wallet_db(&mut db, None).map_err(|e| {
            anyhow!("wallet database migration: {e:?}; if it needs the seed, run the upgrade command with --identity")
        })?;
        Ok(db)
    }

    fn open_db(&self) -> Result<Db> {
        WalletDb::for_path(
            self.dir.join("data.sqlite"),
            self.network,
            SystemClock,
            rand::rng(),
        )
        .context("opening wallet database")
    }

    /// Run the wallet database migrations with the seed available.
    pub fn upgrade(&self, identity_file: &Path) -> Result<()> {
        let seed = self.seed(identity_file)?;
        let mut db = self.open_db()?;
        zcash_client_sqlite::wallet::init::init_wallet_db(&mut db, Some(seed))
            .map_err(|e| anyhow!("wallet database migration: {e:?}"))
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

/// The Orchard full viewing key of an account.
pub fn account_orchard_fvk(db: &Db, account: AccountUuid) -> Result<orchard::keys::FullViewingKey> {
    account_ufvk(db, account)?
        .orchard()
        .cloned()
        .ok_or_else(|| {
            anyhow!(
                "account {} has no Orchard/Ironwood key",
                account.expose_uuid()
            )
        })
}

/// Birthday height of an account (where its notes can start to appear).
pub fn account_birthday(db: &Db, account: AccountUuid) -> Result<u32> {
    Ok(u32::from(
        db.get_account_birthday(account)
            .map_err(|e| anyhow!("{e}"))?,
    ))
}

/// A fresh diversified Orchard address of an account. The wallet records it, so notes sent
/// there are recognised like any other, and nothing ties it to the account's default address.
pub fn next_orchard_address(db: &mut Db, account: AccountUuid) -> Result<orchard::Address> {
    let (ua, _index) = db
        .get_next_available_address(account, UnifiedAddressRequest::ORCHARD)
        .map_err(|e| anyhow!("{e}"))?
        .ok_or_else(|| {
            anyhow!(
                "account {} cannot derive an Orchard address",
                account.expose_uuid()
            )
        })?;
    ua.orchard()
        .copied()
        .ok_or_else(|| anyhow!("derived address has no Orchard receiver"))
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
    find_ironwood_note(db, r, target_height)?
        .ok_or_else(|| anyhow!("note {r} not found or not spendable at this height"))
}

/// Highest block the wallet has scanned, if any.
pub fn max_scanned_height(db: &Db) -> Result<Option<u32>> {
    Ok(db
        .block_max_scanned()
        .map_err(|e| anyhow!("{e}"))?
        .map(|m| u32::from(m.block_height())))
}

/// Ironwood tree roots at the wallet's checkpoints for the last `depth` scanned blocks: the
/// anchors a trade may legitimately use. An envelope whose anchor is not among them was
/// proven against a tree the chain does not have.
pub fn recent_ironwood_anchors(db: &mut Db, depth: u32) -> Result<Vec<orchard::Anchor>> {
    let Some(tip) = max_scanned_height(db)? else {
        return Ok(Vec::new());
    };
    let first = tip.saturating_sub(depth);
    let anchors = db.with_ironwood_tree_mut::<_, _, anyhow::Error>(|tree| {
        let mut anchors = Vec::new();
        for h in first..=tip {
            if let Some(root) = tree
                .root_at_checkpoint_id(&BlockHeight::from(h))
                .map_err(|e| anyhow!("{e:?}"))?
            {
                anchors.push(orchard::Anchor::from(root));
            }
        }
        Ok(anchors)
    })?;
    Ok(anchors.unwrap_or_default())
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

/// Find the account whose Orchard full viewing key is the one inside the given unified
/// viewing key encoding. Only the Orchard component is compared, so an account imported from
/// a complete key still matches a listing that publishes the Orchard part alone.
pub fn find_account_by_ufvk(
    db: &Db,
    network: Network,
    ufvk_str: &str,
) -> Result<Option<AccountUuid>> {
    let wanted = UnifiedFullViewingKey::decode(&network, ufvk_str)
        .map_err(|e| anyhow!("decoding viewing key: {e}"))?
        .orchard()
        .cloned()
        .ok_or_else(|| anyhow!("the viewing key has no Orchard component"))?;
    for id in db.get_account_ids().map_err(|e| anyhow!("{e}"))? {
        if let Ok(ufvk) = account_ufvk(db, id) {
            if ufvk.orchard() == Some(&wanted) {
                return Ok(Some(id));
            }
        }
    }
    Ok(None)
}

/// The largest spendable Ironwood note of an account that is not in `exclude`, if any.
pub fn largest_spendable_note(
    db: &Db,
    account: AccountUuid,
    target_height: TargetHeight,
    exclude: &[NoteRef],
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
        .map(|n| {
            (
                NoteRef {
                    txid: *n.txid(),
                    index: u32::from(n.output_index()),
                },
                n.note().value().inner(),
            )
        })
        .filter(|(r, _)| !exclude.contains(r))
        .max_by_key(|(_, v)| *v)
        .map(|(r, _)| r);
    Ok(best)
}

// ---- Note reservations ----------------------------------------------------------------

const RESERVED_FILE: &str = "reserved-notes.json";

#[derive(Serialize, Deserialize, Default)]
struct Reservations {
    notes: Vec<Reservation>,
}

#[derive(Serialize, Deserialize)]
struct Reservation {
    note: String,
    expiry_height: u32,
}

fn read_reservations(wallet: &Wallet) -> Result<Reservations> {
    let path = wallet.dir.join(RESERVED_FILE);
    if !path.exists() {
        return Ok(Reservations::default());
    }
    let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

fn write_reservations(wallet: &Wallet, r: &Reservations) -> Result<()> {
    let path = wallet.dir.join(RESERVED_FILE);
    fs::write(&path, serde_json::to_string_pretty(r)?)
        .with_context(|| format!("writing {}", path.display()))
}

/// Record that a note is committed to a trade that can be mined until `expiry_height`, so
/// a later `buy` does not pick the same note while that trade is in flight.
pub fn reserve_note(wallet: &Wallet, note: NoteRef, expiry_height: u32) -> Result<()> {
    let mut r = read_reservations(wallet)?;
    let key = note.to_string();
    r.notes.retain(|x| x.note != key);
    r.notes.push(Reservation {
        note: key,
        expiry_height,
    });
    write_reservations(wallet, &r)
}

/// Notes reserved by trades that can still be mined at `tip`. Expired reservations are
/// dropped from the file.
pub fn reserved_notes(wallet: &Wallet, tip: u32) -> Result<Vec<NoteRef>> {
    let mut r = read_reservations(wallet)?;
    let before = r.notes.len();
    r.notes.retain(|x| x.expiry_height >= tip);
    if r.notes.len() != before {
        write_reservations(wallet, &r)?;
    }
    r.notes.iter().map(|x| x.note.parse()).collect()
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
