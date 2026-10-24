//! Access to a light-wallet database and its encrypted seed.
//!
//! The on-disk layout is the one `zcash-devtool` creates: `data.sqlite` holds the wallet,
//! `keys.toml` holds the network, birthday and an age-encrypted mnemonic, and an age
//! identity file decrypts that mnemonic.

use std::{
    fs,
    io::Read,
    io::Write,
    num::NonZeroU32,
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::{anyhow, Context, Result};
#[cfg(feature = "native")]
use bip0039::{Count, English, Mnemonic};
use rand::rngs::ThreadRng;
#[cfg(feature = "native")]
use secrecy::Zeroize;
use secrecy::{ExposeSecret, SecretVec};
use serde::{Deserialize, Serialize};
use zcash_client_backend::data_api::wallet::ConfirmationsPolicy;
#[cfg(feature = "native")]
use zcash_client_backend::data_api::{chain::BlockCache, scanning::ScanRange};
use zcash_client_backend::data_api::{
    chain::{error as chain_error, BlockSource},
    locking::LockFilter,
    wallet::TargetHeight,
    Account as _, AccountBirthday, AccountPurpose, AccountSource, InputSource, NullifierQuery,
    WalletCommitmentTrees, WalletRead, WalletWrite,
};
use zcash_client_backend::proto::compact_formats::CompactBlock;
use zcash_client_backend::proto::service::TreeState;
use zcash_client_backend::util::Clock;
use zcash_client_backend::wallet::Note;
#[cfg(feature = "native")]
use zcash_client_sqlite::wallet::init::init_wallet_db;
use zcash_client_sqlite::{AccountUuid, WalletDb};
use zcash_keys::keys::{UnifiedAddressRequest, UnifiedFullViewingKey, UnifiedSpendingKey};
use zcash_protocol::{
    consensus::{BlockHeight, Network},
    ShieldedPool, TxId,
};

#[cfg(feature = "native")]
use crate::broadcast::{connect, install_crypto_provider};

/// Account identifier of the wallet database, re-exported for the binaries.
pub use zcash_client_sqlite::AccountUuid as WalletAccount;

/// The wallet database type used throughout zsettle.
pub type Db = WalletDb<rusqlite::Connection, Network, WalletClock, ThreadRng>;

/// The wall clock the wallet library records times with: the system clock, or the page's
/// clock in the browser, where the standard library has none.
#[derive(Clone, Copy, Debug, Default)]
pub struct WalletClock;

impl Clock for WalletClock {
    fn now(&self) -> std::time::SystemTime {
        #[cfg(not(target_family = "wasm"))]
        {
            std::time::SystemTime::now()
        }
        #[cfg(target_family = "wasm")]
        {
            std::time::UNIX_EPOCH + std::time::Duration::from_millis(js_sys::Date::now() as u64)
        }
    }
}

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

/// A devtool-layout wallet directory, or a wallet held in memory (the browser).
pub struct Wallet {
    /// Directory containing `data.sqlite` and `keys.toml`; for a wallet in memory, its name.
    pub dir: PathBuf,
    /// Whether the database lives in memory, shared by name between this process's
    /// connections, instead of in `data.sqlite`.
    memory: bool,
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
            memory: false,
            network,
            birthday: keys.birthday.unwrap_or(0),
            mnemonic_ciphertext: keys.mnemonic,
        })
    }

    /// A wallet whose database lives in memory under `name`, shared by every connection of
    /// this process that opens the same name; the caller loads and saves its bytes
    /// ([`Wallet::export`], [`Wallet::import`]). The seed is never stored with it.
    pub fn in_memory(name: &str, network: Network, birthday: u32) -> Self {
        Wallet {
            dir: PathBuf::from(name),
            memory: true,
            network,
            birthday,
            mnemonic_ciphertext: None,
        }
    }

    /// Where SQLite finds the database: the file, or the shared in-memory database.
    fn location(&self) -> PathBuf {
        if self.memory {
            PathBuf::from(format!("file:/{}?vfs=memdb", self.dir.display()))
        } else {
            self.dir.join("data.sqlite")
        }
    }

    /// A plain connection to the wallet database, for queries the wallet library does not
    /// offer.
    pub fn connection(&self, read_only: bool) -> Result<rusqlite::Connection> {
        use rusqlite::OpenFlags;
        let mode = if read_only {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        } else {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        };
        rusqlite::Connection::open_with_flags(
            self.location(),
            mode | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_URI,
        )
        .map_err(|e| anyhow!("opening wallet database: {e}"))
    }

    /// A connection that keeps a wallet in memory alive: SQLite frees an in-memory database
    /// when its last connection closes, so the owner holds this one for the wallet's life.
    pub fn keep_alive(&self) -> Result<rusqlite::Connection> {
        rusqlite::Connection::open(self.location())
            .map_err(|e| anyhow!("opening wallet database: {e}"))
    }

    /// The whole database of a wallet in memory, as SQLite file bytes.
    pub fn export(&self) -> Result<Vec<u8>> {
        let conn = self.connection(true)?;
        let data = conn
            .serialize(rusqlite::MAIN_DB)
            .map_err(|e| anyhow!("saving the wallet database: {e}"))?;
        Ok(data.to_vec())
    }

    /// Replace the database of a wallet in memory with SQLite file bytes saved by
    /// [`Wallet::export`]. The caller holds a [`Wallet::keep_alive`] connection first.
    pub fn import(&self, bytes: &[u8]) -> Result<()> {
        if !self.memory {
            return Err(anyhow!("only a wallet in memory is loaded from bytes"));
        }
        let mut src = rusqlite::Connection::open_in_memory()
            .map_err(|e| anyhow!("loading the wallet database: {e}"))?;
        src.deserialize_read_exact(rusqlite::MAIN_DB, bytes, bytes.len(), true)
            .map_err(|e| anyhow!("loading the wallet database: {e}"))?;
        let mut dst = self.keep_alive()?;
        rusqlite::backup::Backup::new(&src, &mut dst)
            .and_then(|b| b.run_to_completion(1000, std::time::Duration::ZERO, None))
            .map_err(|e| anyhow!("loading the wallet database: {e}"))
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
        WalletDb::for_path(self.location(), self.network, WalletClock, rand::rng())
            .context("opening wallet database")
    }

    /// Run the wallet database migrations with the seed available.
    #[cfg(feature = "native")]
    pub fn upgrade(&self, identity_file: Option<&Path>) -> Result<()> {
        let seed = self.seed(identity_file)?;
        let mut db = self.open_db()?;
        zcash_client_sqlite::wallet::init::init_wallet_db(&mut db, Some(seed))
            .map_err(|e| anyhow!("wallet database migration: {e:?}"))
    }

    /// Decrypt the mnemonic and derive the BIP 39 seed. With an age identity file the
    /// mnemonic must be encrypted to one of its identities; without one it must be
    /// passphrase-protected, and the passphrase comes from `ZSETTLE_PASSPHRASE` or a prompt.
    #[cfg(feature = "native")]
    pub fn seed(&self, identity_file: Option<&Path>) -> Result<SecretVec<u8>> {
        let ciphertext = self
            .mnemonic_ciphertext
            .as_deref()
            .ok_or_else(|| anyhow!("wallet has no encrypted mnemonic (view-only wallet?)"))?;
        let identities: Vec<Box<dyn age::Identity + Send + Sync>> = match identity_file {
            Some(path) => age::IdentityFile::from_file(path.to_string_lossy().into_owned())
                .context("reading age identity file")?
                .into_identities()
                .map_err(|e| anyhow!("age identity file: {e}"))?,
            None => vec![Box::new(age::scrypt::Identity::new(passphrase(false)?))],
        };
        let decryptor = age::Decryptor::new(age::armor::ArmoredReader::new(ciphertext.as_bytes()))
            .map_err(|e| anyhow!("age decryptor: {e}"))?;
        let mut buf = vec![];
        let read = decryptor
            .decrypt(identities.iter().map(|i| i.as_ref() as &dyn age::Identity))
            .map_err(|e| match (identity_file, e) {
                (None, age::DecryptError::NoMatchingKeys) => anyhow!(
                    "this wallet's mnemonic is encrypted to an age identity, not a passphrase: pass --identity"
                ),
                (None, age::DecryptError::DecryptionFailed) => anyhow!("wrong passphrase"),
                (Some(_), age::DecryptError::NoMatchingKeys) => anyhow!(
                    "the identity file does not decrypt this wallet's mnemonic (a passphrase-protected wallet? omit --identity)"
                ),
                (_, e) => anyhow!("age decrypt: {e}"),
            })?
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

    /// The wallet's 24 recovery words, decrypted with `passphrase`. The caller shows them to
    /// their owner and drops them; they are zeroized when the value goes out of scope.
    pub fn mnemonic_phrase(&self, passphrase: &str) -> Result<zeroize::Zeroizing<String>> {
        let ciphertext = self
            .mnemonic_ciphertext
            .as_deref()
            .ok_or_else(|| anyhow!("wallet has no encrypted mnemonic (view-only wallet?)"))?;
        let identity =
            age::scrypt::Identity::new(age::secrecy::SecretString::new(passphrase.into()));
        let decryptor = age::Decryptor::new(age::armor::ArmoredReader::new(ciphertext.as_bytes()))
            .map_err(|e| anyhow!("age decryptor: {e}"))?;
        let mut buf = zeroize::Zeroizing::new(Vec::new());
        decryptor
            .decrypt(std::iter::once(&identity as &dyn age::Identity))
            .map_err(|e| match e {
                age::DecryptError::DecryptionFailed => anyhow!("wrong passphrase"),
                age::DecryptError::NoMatchingKeys => {
                    anyhow!("this wallet is protected by an identity file, not a passphrase")
                }
                e => anyhow!("age decrypt: {e}"),
            })?
            .read_to_end(&mut buf)?;
        let phrase = std::str::from_utf8(&buf).context("the mnemonic is not text")?;
        Ok(zeroize::Zeroizing::new(phrase.trim().to_string()))
    }

    /// Check that `passphrase` decrypts this wallet's mnemonic; nothing is kept.
    pub fn check_passphrase(&self, passphrase: &str) -> Result<()> {
        self.mnemonic_phrase(passphrase).map(|_| ())
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

/// Record a transaction this wallet made outside the library's own builder (a trade, a
/// rotation, a cancel) as soon as it is sent. The wallet then knows the spent notes are gone
/// and sees its change before the next block: the balance is right at once, and a payment
/// made meanwhile does not pick a note that is already spent. Until the transaction is mined
/// or expires, the library treats it like one of its own pending transactions.
pub fn record_transaction(
    wallet: &Wallet,
    db: &mut Db,
    tx: &zcash_primitives::transaction::Transaction,
) -> Result<()> {
    zcash_client_backend::data_api::wallet::decrypt_and_store_transaction(
        &wallet.network,
        db,
        tx,
        None,
    )
    .map_err(|e| anyhow!("recording the transaction in the wallet: {e}"))
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

/// The last block the wallet scanned: its height and its hash as block explorers show it. An
/// ownership proof names this block, so it was provably made after the block was mined.
pub fn scanned_tip(db: &Db) -> Result<Option<(u32, String)>> {
    Ok(db
        .block_max_scanned()
        .map_err(|e| anyhow!("{e}"))?
        .map(|m| (u32::from(m.block_height()), m.block_hash().to_string())))
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

/// Nullifiers of the unspent notes of one account. For the listed item account these are
/// public knowledge: the published viewing key derives them.
pub fn account_unspent_nullifiers(
    db: &Db,
    account: AccountUuid,
) -> Result<Vec<orchard::note::Nullifier>> {
    Ok(db
        .get_ironwood_nullifiers(NullifierQuery::Unspent)
        .map_err(|e| anyhow!("{e}"))?
        .into_iter()
        .filter(|(owner, _)| *owner == account)
        .map(|(_, nf)| nf)
        .collect())
}

/// Import a unified full viewing key as a view-only account, scanning from `birthday`.
#[cfg(feature = "native")]
pub fn import_viewing_key(
    db: &mut Db,
    network: Network,
    name: &str,
    ufvk_str: &str,
    birthday: u32,
    server: &str,
) -> Result<AccountUuid> {
    // The tree state one block before the birthday lets the scanner start exactly there.
    let tree_state =
        crate::broadcast::fetch_tree_state_blocking(server, birthday.saturating_sub(1))?;
    import_viewing_key_at(db, network, name, ufvk_str, tree_state)
}

/// [`import_viewing_key`] with the tree state of the block before the birthday supplied.
pub fn import_viewing_key_at(
    db: &mut Db,
    network: Network,
    name: &str,
    ufvk_str: &str,
    tree_state: TreeState,
) -> Result<AccountUuid> {
    let ufvk = UnifiedFullViewingKey::decode(&network, ufvk_str)
        .map_err(|e| anyhow!("decoding viewing key: {e}"))?;
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

/// The wallet passphrase: `ZSETTLE_PASSPHRASE` when set, otherwise asked for on the terminal
/// (twice when `confirm`), otherwise one line of standard input.
/// scrypt work factor (log2 N) for a passphrase that is already a random 256-bit secret.
#[cfg(feature = "native")]
const PASSKEY_WORK_FACTOR: u8 = 10;

/// Whether a passphrase is a secret a passkey gave (`passkey:` and 64 hex digits), which
/// no one can guess.
pub fn is_random_secret(p: &str) -> bool {
    p.strip_prefix("passkey:")
        .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

#[cfg(feature = "native")]
fn passphrase(confirm: bool) -> Result<age::secrecy::SecretString> {
    use std::io::IsTerminal;
    use zeroize::Zeroizing;
    let text: Zeroizing<String> = if let Ok(p) = std::env::var("ZSETTLE_PASSPHRASE") {
        Zeroizing::new(p)
    } else if std::io::stdin().is_terminal() {
        let p = Zeroizing::new(rpassword::prompt_password("Wallet passphrase: ")?);
        if confirm && Zeroizing::new(rpassword::prompt_password("Repeat passphrase: ")?) != p {
            return Err(anyhow!("the passphrases differ"));
        }
        p
    } else {
        let mut line = Zeroizing::new(String::new());
        std::io::stdin().read_line(&mut line)?;
        Zeroizing::new(line.trim_end_matches(['\r', '\n']).to_string())
    };
    if text.is_empty() {
        return Err(anyhow!(
            "empty passphrase (set ZSETTLE_PASSPHRASE, or run on a terminal to be asked)"
        ));
    }
    Ok(age::secrecy::SecretString::new(text.as_str().into()))
}

/// Birthdays are rounded down to a multiple of this many blocks (about a day), so that the
/// tree-state request made to create a wallet or an account, and the birthday a listing
/// later publishes, name no height unique to one wallet.
pub const BIRTHDAY_ROUNDING: u32 = 1_000;

/// A wallet directory just created by [`init_wallet`].
#[derive(Serialize)]
pub struct Created {
    /// The wallet directory.
    pub dir: PathBuf,
    /// `main` or `test`.
    pub network: String,
    /// Height the wallet scans from.
    pub birthday: u32,
    /// `identity-file` or `passphrase`.
    pub protection: &'static str,
    /// Whether the age identity file was generated by this call.
    pub identity_created: bool,
    /// The derived accounts, in the order they were asked for.
    pub accounts: Vec<AccountInfo>,
}

/// One account of a wallet, as `accounts` lists them.
#[derive(Serialize)]
pub struct AccountInfo {
    /// Name given at creation.
    pub name: String,
    /// Account UUID, as the commands take it.
    pub account: String,
    /// ZIP 32 account index; absent for an account imported from a viewing key.
    pub index: Option<u32>,
    /// Height the account scans from.
    pub birthday: u32,
}

#[cfg(feature = "native")]
#[derive(Serialize)]
struct KeysOut<'a> {
    mnemonic: &'a str,
    network: &'a str,
    birthday: u32,
}

/// Create a wallet in the devtool layout: a fresh 24-word mnemonic encrypted to the age
/// identity in `identity` (generated there when the file does not exist) or, without one, to
/// a passphrase; `keys.toml`; the database; and one derived account per name. The birthday is a little below the server's
/// tip, so the first sync is seconds. Refuses to touch a directory that already holds a
/// wallet.
#[cfg(feature = "native")]
pub fn init_wallet(
    dir: &Path,
    network: Network,
    server: &str,
    identity: Option<&Path>,
    account_names: &[&str],
) -> Result<Created> {
    let keys_path = dir.join("keys.toml");
    let db_path = dir.join("data.sqlite");
    if keys_path.exists() || db_path.exists() {
        return Err(anyhow!(
            "{} already holds a wallet; refusing to overwrite it",
            dir.display()
        ));
    }
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    // The database holds viewing keys and the whole history: owner-only on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("restricting {}", dir.display()))?;
    }

    let (recipients, protection, identity_created): (
        Vec<Box<dyn age::Recipient + Send>>,
        &'static str,
        bool,
    ) = match identity {
        Some(identity) if identity.exists() => {
            let file = age::IdentityFile::from_file(identity.to_string_lossy().into_owned())
                .context("reading age identity file")?;
            let recipients = file
                .to_recipients()
                .map_err(|e| anyhow!("age identity file: {e}"))?;
            (recipients, "identity-file", false)
        }
        Some(identity) => {
            let id = age::x25519::Identity::generate();
            let public = id.to_public();
            let secret = id.to_string();
            let text = format!(
                "# public key: {public}\n{}\n",
                age::secrecy::ExposeSecret::expose_secret(&secret)
            );
            write_private(identity, text.as_bytes())?;
            (vec![Box::new(public)], "identity-file", true)
        }
        None => {
            let pass = passphrase(true)?;
            let strong = is_random_secret(age::secrecy::ExposeSecret::expose_secret(&pass));
            let mut r = age::scrypt::Recipient::new(pass);
            // A passphrase made from a passkey is 256 random bits: stretching it adds nothing,
            // so it gets a light work factor and every command opens the wallet at once.
            if strong {
                r.set_work_factor(PASSKEY_WORK_FACTOR);
            }
            (vec![Box::new(r)], "passphrase", false)
        }
    };

    let mnemonic = <Mnemonic<English>>::generate(Count::Words24);
    let mut armored = Vec::new();
    {
        let encryptor = age::Encryptor::with_recipients(
            recipients.iter().map(|r| r.as_ref() as &dyn age::Recipient),
        )
        .map_err(|e| anyhow!("age: {e}"))?;
        let armor =
            age::armor::ArmoredWriter::wrap_output(&mut armored, age::armor::Format::AsciiArmor)?;
        let mut w = encryptor.wrap_output(armor)?;
        w.write_all(mnemonic.phrase().as_bytes())?;
        w.finish()?.finish()?;
    }
    let ciphertext = String::from_utf8(armored).context("age armor")?;

    let info = crate::broadcast::server_info_blocking(server)?;
    let chain = match network {
        Network::MainNetwork => "main",
        Network::TestNetwork => "test",
    };
    if info.chain_name != chain {
        return Err(anyhow!(
            "the server {server} is on chain {:?}, not {chain:?}",
            info.chain_name
        ));
    }
    // Rounded down, so the tree-state request does not name a height unique to this wallet.
    let birthday_height =
        (info.block_height.saturating_sub(100) / BIRTHDAY_ROUNDING) * BIRTHDAY_ROUNDING;
    let tree_state =
        crate::broadcast::fetch_tree_state_blocking(server, birthday_height.saturating_sub(1))?;
    let birthday = AccountBirthday::from_treestate(tree_state, None)
        .map_err(|e| anyhow!("birthday: {e:?}"))?;

    let keys = KeysOut {
        mnemonic: &ciphertext,
        network: chain,
        birthday: birthday_height,
    };
    write_private(&keys_path, toml::to_string(&keys)?.as_bytes())?;

    let wallet = Wallet::open(dir)?;
    let mut db = wallet.db()?;
    init_wallet_db(&mut db, None).map_err(|e| anyhow!("initialising the wallet database: {e}"))?;
    let seed = {
        let mut bytes = mnemonic.to_seed("");
        let seed = SecretVec::new(bytes.to_vec());
        bytes.zeroize();
        seed
    };
    let mut accounts = Vec::new();
    for name in account_names {
        let (id, _usk) = db
            .create_account(name, &seed, &birthday, None)
            .map_err(|e| anyhow!("creating account {name}: {e}"))?;
        accounts.push(account_info(&db, id)?);
    }
    Ok(Created {
        dir: dir.to_path_buf(),
        network: chain.into(),
        birthday: birthday_height,
        protection,
        identity_created,
        accounts,
    })
}

/// Write `env.sh` (export lines for `source`) and `env.ps1` (PowerShell assignments) into the
/// wallet directory, so that later commands need none of the flags these variables stand for.
/// Returns the path of `env.sh` and its text.
pub fn write_env_files(wallet: &Path, vars: &[(&str, String)]) -> Result<(PathBuf, String)> {
    let sh: String = vars
        .iter()
        .map(|(k, v)| format!("export {k}='{}'\n", v.replace('\'', "'\\''")))
        .collect();
    let ps1: String = vars
        .iter()
        .map(|(k, v)| format!("$env:{k} = '{}'\n", v.replace('\'', "''")))
        .collect();
    let sh_path = wallet.join("env.sh");
    fs::write(&sh_path, &sh).with_context(|| format!("writing {}", sh_path.display()))?;
    let ps1_path = wallet.join("env.ps1");
    fs::write(&ps1_path, &ps1).with_context(|| format!("writing {}", ps1_path.display()))?;
    Ok((sh_path, sh))
}

/// Write a file readable by its owner only (on Unix), replacing any previous content. The
/// bytes go to a fresh file next to it that then takes its place, so a reader, a crash or a
/// second writer never sees half a file.
pub fn write_owner_only(path: &Path, bytes: &[u8]) -> Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(
        ".{name}.{}.tmp",
        hex::encode(rand::random::<[u8; 6]>())
    ));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = (|| -> Result<()> {
        let mut f = options
            .open(&tmp)
            .with_context(|| format!("writing {}", path.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// Write a secret file readable by its owner only (on Unix) and never over an existing one.
#[cfg(feature = "native")]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    f.write_all(bytes)?;
    Ok(())
}

/// Name, UUID, ZIP 32 index and birthday of one account.
pub fn account_info(db: &Db, id: AccountUuid) -> Result<AccountInfo> {
    let account = db
        .get_account(id)
        .map_err(|e| anyhow!("{e}"))?
        .ok_or_else(|| anyhow!("account {} not found", id.expose_uuid()))?;
    let index = match account.source() {
        AccountSource::Derived { derivation, .. } => Some(u32::from(derivation.account_index())),
        AccountSource::Imported { .. } => None,
    };
    Ok(AccountInfo {
        name: account.name().unwrap_or("").to_string(),
        account: id.expose_uuid().to_string(),
        index,
        birthday: u32::from(account.birthday_height()),
    })
}

/// Every account of the wallet, in database order.
pub fn list_accounts(db: &Db) -> Result<Vec<AccountInfo>> {
    db.get_account_ids()
        .map_err(|e| anyhow!("{e}"))?
        .into_iter()
        .map(|id| account_info(db, id))
        .collect()
}

/// Create a new ZIP 32 account of this wallet from its seed, with a birthday at the wallet's
/// current scanned height. Returns the account id and its ZIP 32 index. One account per item
/// keeps a consignment to that item alone.
#[cfg(feature = "native")]
pub fn create_account(
    wallet: &Wallet,
    db: &mut Db,
    seed: &SecretVec<u8>,
    name: &str,
    server: &str,
) -> Result<(AccountUuid, u32)> {
    let born = new_account_birthday(db)?;
    let tree_state = crate::broadcast::fetch_tree_state_blocking(server, born.saturating_sub(1))?;
    let _ = wallet;
    create_account_at(db, seed, name, tree_state)
}

/// The birthday a new account of this wallet gets: the scanned height, rounded down like
/// `init_wallet`, so the published birthday names no exact height.
pub fn new_account_birthday(db: &Db) -> Result<u32> {
    let tip = max_scanned_height(db)?.ok_or_else(|| anyhow!("sync the wallet first"))?;
    Ok((tip / BIRTHDAY_ROUNDING) * BIRTHDAY_ROUNDING)
}

/// [`create_account`] with the tree state of the block before the birthday supplied.
pub fn create_account_at(
    db: &mut Db,
    seed: &SecretVec<u8>,
    name: &str,
    tree_state: TreeState,
) -> Result<(AccountUuid, u32)> {
    let birthday = AccountBirthday::from_treestate(tree_state, None)
        .map_err(|e| anyhow!("birthday: {e:?}"))?;
    let (id, _usk) = db
        .create_account(name, seed, &birthday, None)
        .map_err(|e| anyhow!("creating account: {e}"))?;
    let account = db
        .get_account(id)
        .map_err(|e| anyhow!("{e}"))?
        .ok_or_else(|| anyhow!("account {} not found after creation", id.expose_uuid()))?;
    let index = match account.source() {
        AccountSource::Derived { derivation, .. } => u32::from(derivation.account_index()),
        AccountSource::Imported { .. } => return Err(anyhow!("created account has no derivation")),
    };
    Ok((id, index))
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

/// The largest spendable Ironwood note of an account that is not in `exclude`, if any, under
/// ZIP 315's confirmations (3 for the wallet's own change, 10 for money from others).
pub fn largest_spendable_note(
    db: &Db,
    account: AccountUuid,
    target_height: TargetHeight,
    exclude: &[NoteRef],
) -> Result<Option<NoteRef>> {
    largest_note_under(db, account, target_height, exclude, ConfirmationsPolicy::default())
}

/// [`largest_spendable_note`] for notes with at least `confirmations`, whoever sent them.
pub fn largest_confirmed_note(
    db: &Db,
    account: AccountUuid,
    target_height: TargetHeight,
    exclude: &[NoteRef],
    confirmations: u32,
) -> Result<Option<NoteRef>> {
    largest_note_under(db, account, target_height, exclude, confirmations_policy(confirmations))
}

/// The same number of confirmations for every note, whoever sent it.
pub fn confirmations_policy(confirmations: u32) -> ConfirmationsPolicy {
    let min = NonZeroU32::new(confirmations.max(1)).expect("non-zero");
    ConfirmationsPolicy::new_symmetrical(min, false)
}

fn largest_note_under(
    db: &Db,
    account: AccountUuid,
    target_height: TargetHeight,
    exclude: &[NoteRef],
    policy: ConfirmationsPolicy,
) -> Result<Option<NoteRef>> {
    use zcash_client_backend::data_api::{MaxSpendMode, TargetValue};
    let notes = db
        .select_spendable_notes(
            account,
            TargetValue::AllFunds(MaxSpendMode::MaxSpendable),
            &[ShieldedPool::Ironwood],
            target_height,
            policy,
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

/// A wallet in memory keeps its reservations in a table of its own database, so they are
/// saved with it.
const RESERVED_TABLE: &str = "CREATE TABLE IF NOT EXISTS zsettle_reserved_notes (
    note TEXT PRIMARY KEY, expiry_height INTEGER NOT NULL)";

fn read_reservations(wallet: &Wallet) -> Result<Reservations> {
    if wallet.memory {
        let conn = wallet.connection(false)?;
        conn.execute(RESERVED_TABLE, [])
            .map_err(|e| anyhow!("{e}"))?;
        let mut stmt = conn
            .prepare("SELECT note, expiry_height FROM zsettle_reserved_notes ORDER BY rowid")
            .map_err(|e| anyhow!("{e}"))?;
        let notes = stmt
            .query_map([], |r| {
                Ok(Reservation {
                    note: r.get(0)?,
                    expiry_height: r.get(1)?,
                })
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .map_err(|e| anyhow!("{e}"))?;
        return Ok(Reservations { notes });
    }
    let path = wallet.dir.join(RESERVED_FILE);
    if !path.exists() {
        return Ok(Reservations::default());
    }
    let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

fn write_reservations(wallet: &Wallet, r: &Reservations) -> Result<()> {
    if wallet.memory {
        let mut conn = wallet.connection(false)?;
        let tx = conn.transaction().map_err(|e| anyhow!("{e}"))?;
        tx.execute(RESERVED_TABLE, []).map_err(|e| anyhow!("{e}"))?;
        tx.execute("DELETE FROM zsettle_reserved_notes", [])
            .map_err(|e| anyhow!("{e}"))?;
        for x in &r.notes {
            tx.execute(
                "INSERT INTO zsettle_reserved_notes (note, expiry_height) VALUES (?1, ?2)",
                rusqlite::params![x.note, x.expiry_height],
            )
            .map_err(|e| anyhow!("{e}"))?;
        }
        return tx.commit().map_err(|e| anyhow!("{e}"));
    }
    let path = wallet.dir.join(RESERVED_FILE);
    write_owner_only(&path, serde_json::to_string_pretty(r)?.as_bytes())
}

/// Every reservation on file: note and the height until which its trade can be mined.
pub fn reservations(wallet: &Wallet) -> Result<Vec<(NoteRef, u32)>> {
    read_reservations(wallet)?
        .notes
        .iter()
        .map(|x| Ok((x.note.parse()?, x.expiry_height)))
        .collect()
}

/// Drop the reservation of a note (its trade settled, or the note was retired).
pub fn release_note(wallet: &Wallet, note: NoteRef) -> Result<()> {
    let mut r = read_reservations(wallet)?;
    let key = note.to_string();
    r.notes.retain(|x| x.note != key);
    write_reservations(wallet, &r)
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
pub fn reserved_notes(wallet: &Wallet, _tip: u32) -> Result<Vec<NoteRef>> {
    // Expired reservations stay: the seller holds that note's nullifier, so paying with it
    // again would link two offers. `rotate` retires or releases them.
    read_reservations(wallet)?
        .notes
        .iter()
        .map(|x| x.note.parse())
        .collect()
}

// ---- Synchronisation ----------------------------------------------------------------

/// Blocks fetched per batch while syncing; each batch is scanned and dropped before the next.
#[cfg(feature = "native")]
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
    /// Add downloaded blocks, replacing any already held at the same heights.
    pub fn put(&self, compact_blocks: Vec<CompactBlock>) {
        let mut guard = self.lock();
        for b in compact_blocks {
            guard.retain(|x| x.height != b.height);
            guard.push(b);
        }
    }

    /// Drop every block held.
    pub fn clear(&self) {
        self.lock().clear();
    }

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

#[cfg(feature = "native")]
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
        self.put(compact_blocks);
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
#[cfg(feature = "native")]
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

#[cfg(test)]
mod passkey_tests {
    #[test]
    fn only_passkey_secrets_get_the_light_work_factor() {
        assert!(super::is_random_secret(&format!(
            "passkey:{}",
            "ab".repeat(32)
        )));
        assert!(!super::is_random_secret("passkey:short"));
        assert!(!super::is_random_secret(&format!(
            "passkey:{}",
            "zz".repeat(32)
        )));
        assert!(!super::is_random_secret(
            "a long human passphrase of many words"
        ));
    }
}
