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
};

use anyhow::{anyhow, Context, Result};
use bip0039::{English, Mnemonic};
use rand_core::OsRng;
use secrecy::{ExposeSecret, SecretVec, Zeroize};
use serde::Deserialize;
use zcash_client_backend::data_api::{
    locking::LockFilter, wallet::TargetHeight, Account as _, AccountBirthday, AccountPurpose,
    InputSource, NullifierQuery, WalletRead, WalletWrite,
};
use zcash_client_backend::wallet::{Note, ReceivedNote};
use zcash_client_sqlite::{util::SystemClock, AccountUuid, ReceivedNoteId, WalletDb};
use zcash_keys::keys::{UnifiedFullViewingKey, UnifiedSpendingKey};
use zcash_protocol::{consensus::Network, ShieldedPool, TxId};

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

/// Look up a specific Ironwood note by transaction id and output index.
pub fn ironwood_note(db: &Db, r: NoteRef, target_height: TargetHeight) -> Result<IronwoodNote> {
    let received: ReceivedNote<ReceivedNoteId, Note> = db
        .get_spendable_note(
            &r.txid,
            ShieldedPool::Ironwood,
            r.index,
            target_height,
            LockFilter::Unfiltered,
        )
        .map_err(|e| anyhow!("{e}"))?
        .ok_or_else(|| {
            anyhow!(
                "note {}:{} not found or not spendable at this height",
                r.txid,
                r.index
            )
        })?;
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
    Ok(IronwoodNote {
        note,
        position: received.note_commitment_tree_position(),
    })
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
