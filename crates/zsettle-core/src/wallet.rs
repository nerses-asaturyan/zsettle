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
use bip0039::{Count, English, Mnemonic};
use rand::rngs::ThreadRng;
use secrecy::{ExposeSecret, SecretVec, Zeroize};
use serde::{Deserialize, Serialize};
use zcash_client_backend::data_api::{
    chain::{error as chain_error, BlockCache, BlockSource},
    locking::LockFilter,
    scanning::ScanRange,
    wallet::TargetHeight,
    Account as _, AccountBirthday, AccountPurpose, AccountSource, InputSource, NullifierQuery,
    WalletCommitmentTrees, WalletRead, WalletWrite,
};
use zcash_client_backend::proto::compact_formats::CompactBlock;
use zcash_client_backend::wallet::Note;
use zcash_client_sqlite::{util::SystemClock, wallet::init::init_wallet_db, AccountUuid, WalletDb};
use zcash_keys::keys::{UnifiedAddressRequest, UnifiedFullViewingKey, UnifiedSpendingKey};
use zcash_protocol::{
    consensus::{BlockHeight, Network},
    ShieldedPool, TxId,
};

use crate::broadcast::{connect, install_crypto_provider};

/// Account identifier of the wallet database, re-exported for the binaries.
pub use zcash_client_sqlite::AccountUuid as WalletAccount;

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
    pub fn upgrade(&self, identity_file: Option<&Path>) -> Result<()> {
        let seed = self.seed(identity_file)?;
        let mut db = self.open_db()?;
        zcash_client_sqlite::wallet::init::init_wallet_db(&mut db, Some(seed))
            .map_err(|e| anyhow!("wallet database migration: {e:?}"))
    }

    /// Decrypt the mnemonic and derive the BIP 39 seed. With an age identity file the
    /// mnemonic must be encrypted to one of its identities; without one it must be
    /// passphrase-protected, and the passphrase comes from `ZSETTLE_PASSPHRASE` or a prompt.
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

/// The wallet passphrase: `ZSETTLE_PASSPHRASE` when set, otherwise asked for on the terminal
/// (twice when `confirm`), otherwise one line of standard input.
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
        None => (
            vec![Box::new(age::scrypt::Recipient::new(passphrase(true)?))],
            "passphrase",
            false,
        ),
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
pub fn create_account(
    wallet: &Wallet,
    db: &mut Db,
    seed: &SecretVec<u8>,
    name: &str,
    server: &str,
) -> Result<(AccountUuid, u32)> {
    let tip = max_scanned_height(db)?.ok_or_else(|| anyhow!("sync the wallet first"))?;
    // Rounded down like `init_wallet`: the published birthday then names no exact height.
    let born = (tip / BIRTHDAY_ROUNDING) * BIRTHDAY_ROUNDING;
    let tree_state = crate::broadcast::fetch_tree_state_blocking(server, born.saturating_sub(1))?;
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
    let _ = wallet;
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
