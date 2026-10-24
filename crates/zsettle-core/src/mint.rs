//! Minting items and listing them.
//!
//! An item is minted by paying a small shielded amount from the seller's payout account to
//! the item account, with a memo naming the item. The resulting note is the item's genesis
//! note; its commitment is the item's identity.

use std::{num::NonZeroUsize, str::FromStr};

use anyhow::{anyhow, Result};
use secrecy::SecretVec;
use zcash_address::ZcashAddress;
use zcash_client_backend::data_api::{
    wallet::{
        create_proposed_transactions,
        input_selection::{GreedyInputSelector, SpendPolicy},
        propose_transfer, ConfirmationsPolicy, SpendingKeys,
    },
    WalletRead,
};
use zcash_client_backend::fees::{
    standard::MultiOutputChangeStrategy, DustOutputPolicy, SplitPolicy, StandardFeeRule,
};
use zcash_client_backend::wallet::OvkPolicy;
use zcash_client_sqlite::AccountUuid;
use zcash_keys::address::{Address, UnifiedAddress};
#[cfg(feature = "native")]
use zcash_proofs::prover::LocalTxProver;
use zcash_protocol::{
    consensus::{Network, NetworkUpgrade, Parameters},
    memo::{Memo, MemoBytes},
    value::Zatoshis,
    ShieldedPool, TxId,
};
use zip321::{Payment, TransactionRequest};

use crate::wallet::{next_orchard_address, Db, NoteRef, Wallet, WalletClock};

/// An unspent note of the item account.
#[derive(Clone, Debug)]
pub struct ItemNote {
    /// Where the note lives.
    pub note: NoteRef,
    /// Value in zatoshis.
    pub value_zat: u64,
    /// Height the note was mined at, if known.
    pub mined_height: Option<u32>,
    /// The memo, when it is text.
    pub memo: Option<String>,
    /// Received at the account's internal (change) address. The wallet spends such a note
    /// after 3 confirmations instead of the 10 it asks of a note from another wallet
    /// (ZIP 315's recommended policy).
    pub change: bool,
}

/// SQL condition that a spend recorded in transaction `tx` still counts: the transaction is
/// mined, never expires, or is unexpired at `target` (an SQL parameter). A spend whose expiry
/// the wallet does not know, as happens for one seen only in a compact block, counts for the
/// default expiry delta after it was first observed, the way the wallet library itself bounds
/// it. Without that bound a spend undone by a reorg would hide the note for good.
fn live_spend(network: &Network, tx: &str, target: &str) -> String {
    // The default delta changes at NU7 (40 blocks before, 120 from), and the library bounds an
    // unknown expiry by the delta in force where the spend was first observed.
    let delta = match network.activation_height(NetworkUpgrade::Nu7) {
        Some(nu7) => {
            let nu7 = u32::from(nu7);
            format!(
                "(CASE WHEN {tx}.min_observed_height >= {nu7} THEN {} ELSE {} END)",
                crate::standard_expiry_delta(network, nu7),
                crate::standard_expiry_delta(network, nu7.saturating_sub(1)),
            )
        }
        None => crate::DEFAULT_MAX_EXPIRY_DELTA.to_string(),
    };
    format!(
        "({tx}.mined_height IS NOT NULL
          OR {tx}.expiry_height = 0
          OR {tx}.expiry_height >= {target}
          OR ({tx}.expiry_height IS NULL
              AND {tx}.min_observed_height + {delta} >= {target}))"
    )
}

/// Every unspent Ironwood note of an account, dust included, oldest first and unmined notes
/// last. The wallet's own note selection ignores notes at or below the ZIP 317 marginal fee,
/// which is exactly where item notes live, so this reads the wallet's tables directly.
pub fn unspent_notes(wallet: &Wallet, db: &Db, account: AccountUuid) -> Result<Vec<ItemNote>> {
    // The next block to be scanned: the height a spend must still be valid at.
    let target = crate::wallet::max_scanned_height(db)?.unwrap_or(0) + 1;
    let conn = open_read_only(wallet)?;
    let mut stmt = conn
        .prepare(&format!(
            "SELECT t.txid, n.action_index, n.value, t.mined_height, n.memo,
                    n.recipient_key_scope
             FROM ironwood_received_notes n
             JOIN transactions t ON t.id_tx = n.transaction_id
             JOIN accounts a ON a.id = n.account_id
             WHERE a.uuid = ?1
               AND {live_tx}
               AND NOT EXISTS (
                 SELECT 1 FROM ironwood_received_note_spends s
                 JOIN transactions st ON st.id_tx = s.transaction_id
                 WHERE s.ironwood_received_note_id = n.id
                   AND {live})
             ORDER BY t.mined_height IS NULL, t.mined_height, n.action_index",
            // A note of an unmined transaction counts only while that transaction can still
            // be mined: the change of a payment that expired or was rejected is not money.
            live_tx = live_spend(&wallet.network, "t", "?2"),
            live = live_spend(&wallet.network, "st", "?2")
        ))
        .map_err(|e| anyhow!("{e}"))?;
    let rows = stmt
        .query_map(
            rusqlite::params![account.expose_uuid().as_bytes().as_slice(), target],
            |row| {
                let txid: Vec<u8> = row.get(0)?;
                let index: u32 = row.get(1)?;
                let value: i64 = row.get(2)?;
                let mined: Option<u32> = row.get(3)?;
                let memo: Option<Vec<u8>> = row.get(4)?;
                let scope: Option<i64> = row.get(5)?;
                Ok((txid, index, value, mined, memo, scope))
            },
        )
        .map_err(|e| anyhow!("{e}"))?;
    let mut out = Vec::new();
    for row in rows {
        let (txid, index, value, mined_height, memo, scope) = row.map_err(|e| anyhow!("{e}"))?;
        let memo = memo
            .and_then(|bytes| MemoBytes::from_bytes(&bytes).ok())
            .and_then(|m| Memo::try_from(m).ok())
            .and_then(|m| match m {
                Memo::Text(t) => Some(t.to_string()),
                _ => None,
            });
        let txid: [u8; 32] = txid
            .try_into()
            .map_err(|_| anyhow!("bad txid in the wallet database"))?;
        out.push(ItemNote {
            note: NoteRef {
                txid: TxId::from_bytes(txid),
                index,
            },
            value_zat: u64::try_from(value).unwrap_or(0),
            mined_height,
            memo,
            // 1 is the internal scope in the wallet database.
            change: scope == Some(1),
        });
    }
    Ok(out)
}

/// Whether a note of this wallet is unspent, spent, or unknown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoteState {
    /// The wallet has never seen this note.
    Unknown,
    /// Received and not spent; `mined_height` is `None` while the creating transaction is unmined.
    Unspent {
        /// Height of the transaction that created the note.
        mined_height: Option<u32>,
    },
    /// Spent by the given transaction (mined, or created by this wallet and not yet expired).
    Spent {
        /// Transaction id of the spend.
        txid: String,
    },
}

fn open_read_only(wallet: &Wallet) -> Result<rusqlite::Connection> {
    wallet.connection(true)
}

/// The wallet's knowledge of one Ironwood note, read from its tables. Unlike
/// [`crate::wallet::find_ironwood_note`], this distinguishes a note the wallet has recorded
/// a spend for from one it merely cannot spend right now (unmined, reorged, not yet
/// scanned).
pub fn note_state(wallet: &Wallet, db: &Db, note: NoteRef) -> Result<NoteState> {
    use rusqlite::OptionalExtension;
    let target = crate::wallet::max_scanned_height(db)?.unwrap_or(0) + 1;
    let conn = open_read_only(wallet)?;
    let row = conn
        .query_row(
            &format!(
                "SELECT t.mined_height, (
                SELECT st.txid FROM ironwood_received_note_spends s
                JOIN transactions st ON st.id_tx = s.transaction_id
                WHERE s.ironwood_received_note_id = n.id
                  AND {live}
                LIMIT 1)
             FROM ironwood_received_notes n
             JOIN transactions t ON t.id_tx = n.transaction_id
             WHERE t.txid = ?1 AND n.action_index = ?2",
                live = live_spend(&wallet.network, "st", "?3")
            ),
            rusqlite::params![note.txid.as_ref().to_vec(), note.index, target],
            |row| {
                let mined: Option<u32> = row.get(0)?;
                let spent: Option<Vec<u8>> = row.get(1)?;
                Ok((mined, spent))
            },
        )
        .optional()
        .map_err(|e| anyhow!("{e}"))?;
    Ok(match row {
        None => NoteState::Unknown,
        Some((mined_height, None)) => NoteState::Unspent { mined_height },
        Some((_, Some(spender))) => {
            let bytes: [u8; 32] = spender
                .try_into()
                .map_err(|_| anyhow!("bad txid in the wallet database"))?;
            NoteState::Spent {
                txid: TxId::from_bytes(bytes).to_string(),
            }
        }
    })
}

/// A fresh receiving address of the item account, as a unified address string with only
/// an Orchard receiver. Each mint gets its own diversifier.
pub fn item_address(db: &mut Db, network: Network, item_account: AccountUuid) -> Result<String> {
    let addr = next_orchard_address(db, item_account)?;
    let ua = UnifiedAddress::from_receivers(Some(addr), None, None, None, None)
        .ok_or_else(|| anyhow!("cannot build item address"))?;
    Ok(ua.encode(&network))
}

/// What to mint and which account pays for it.
#[derive(Clone, Debug)]
pub struct MintRequest {
    /// Account that pays (the seller's payout account).
    pub from_account: AccountUuid,
    /// ZIP 32 index of that account, to derive its spending key.
    pub from_account_index: u32,
    /// The item account that receives the genesis note.
    pub item_account: AccountUuid,
    /// Value of the item note in zatoshis.
    pub value_zat: u64,
    /// Item names, one note each, written into the memos; one transaction for all.
    pub names: Vec<String>,
}

/// A plain shielded payment from an account of this wallet.
#[derive(Clone, Debug)]
pub struct PayRequest {
    /// Account that pays.
    pub from_account: AccountUuid,
    /// ZIP 32 index of that account, to derive its spending key.
    pub from_account_index: u32,
    /// Recipient, as a unified address string.
    pub to: String,
    /// Amount in zatoshis.
    pub value_zat: u64,
    /// Optional text memo.
    pub memo: Option<String>,
    /// Confirmations every spent note needs; `None` is ZIP 315's (3 for change, 10 for money
    /// from others).
    pub min_confirmations: Option<u32>,
}

/// Mint items: one note per name, each at a fresh address of the item account with the name
/// as the memo, all in one transaction paid by the payout account. Returns the raw
/// transaction bytes, its id and the index of each item note's action in the order of the
/// names, so the notes can be listed as soon as the transaction is mined; the caller
/// broadcasts.
pub fn mint(
    wallet: &Wallet,
    db: &mut Db,
    seed: &SecretVec<u8>,
    req: &MintRequest,
) -> Result<(Vec<u8>, String, Vec<Option<u32>>)> {
    let MintRequest {
        from_account,
        from_account_index,
        item_account,
        value_zat,
        names,
    } = req.clone();
    if names.is_empty() {
        return Err(anyhow!("nothing to mint: give at least one --name"));
    }
    if value_zat < crate::item::MIN_ITEM_VALUE {
        return Err(anyhow!(
            "item value {value_zat} zat is below the minimum of {} (a smaller note could never be traded)",
            crate::item::MIN_ITEM_VALUE
        ));
    }
    let mut payments = Vec::with_capacity(names.len());
    for name in &names {
        let to = item_address(db, wallet.network, item_account)?;
        let memo =
            Memo::from_str(&format!("zsettle item: {name}")).map_err(|e| anyhow!("memo: {e}"))?;
        payments.push(
            Payment::new(
                ZcashAddress::from_str(&to).map_err(|e| anyhow!("item address: {e}"))?,
                Some(Zatoshis::from_u64(value_zat).map_err(|_| anyhow!("value"))?),
                Some(MemoBytes::from(&memo)),
                None,
                None,
                vec![],
            )
            .map_err(|e| anyhow!("invalid payment: {e:?}"))?,
        );
    }
    let request = TransactionRequest::new(payments).map_err(|e| anyhow!("payment request: {e}"))?;
    let (bytes, txid) = pay_request(
        wallet,
        db,
        seed,
        from_account,
        from_account_index,
        request,
        None,
    )?;
    let indices = item_output_indices(wallet, db, &bytes, item_account, &names)?;
    Ok((bytes, txid, indices))
}

/// Which action of a transaction this wallet built carries the note for each name, matched by
/// memo; `None` when a note does not decrypt (it should not happen).
fn item_output_indices(
    wallet: &Wallet,
    db: &Db,
    bytes: &[u8],
    account: AccountUuid,
    names: &[String],
) -> Result<Vec<Option<u32>>> {
    use orchard::keys::{PreparedIncomingViewingKey, Scope};
    use orchard::note_encryption::IronwoodDomain;
    use zcash_note_encryption::try_note_decryption;
    use zcash_protocol::consensus::{BlockHeight, BranchId};
    let height = crate::wallet::max_scanned_height(db)?.unwrap_or(0) + 1;
    let tx = zcash_primitives::transaction::Transaction::read(
        bytes,
        BranchId::for_height(&wallet.network, BlockHeight::from(height)),
    )
    .map_err(|e| anyhow!("re-reading the transaction: {e}"))?;
    let fvk = crate::wallet::account_orchard_fvk(db, account)?;
    let ivk = PreparedIncomingViewingKey::new(&fvk.to_ivk(Scope::External));
    let mut found: Vec<(u32, String)> = Vec::new();
    if let Some(bundle) = tx.ironwood_bundle() {
        for (i, action) in bundle.actions().iter().enumerate() {
            let domain = IronwoodDomain::for_action(action);
            if let Some((_, _, memo)) = try_note_decryption(&domain, &ivk, action) {
                let text = MemoBytes::from_bytes(&memo)
                    .ok()
                    .and_then(|m| Memo::try_from(m).ok())
                    .and_then(|m| match m {
                        Memo::Text(t) => Some(t.to_string()),
                        _ => None,
                    })
                    .unwrap_or_default();
                found.push((i as u32, text));
            }
        }
    }
    Ok(names
        .iter()
        .map(|name| {
            let memo = format!("zsettle item: {name}");
            let at = found.iter().position(|(_, t)| *t == memo)?;
            Some(found.remove(at).0)
        })
        .collect())
}

/// Pay a unified address from an account of this wallet. Returns the raw transaction bytes
/// and its id; the caller broadcasts.
pub fn pay(
    wallet: &Wallet,
    db: &mut Db,
    seed: &SecretVec<u8>,
    req: &PayRequest,
) -> Result<(Vec<u8>, String)> {
    let PayRequest {
        from_account,
        from_account_index,
        to,
        value_zat,
        memo,
        min_confirmations,
    } = req.clone();
    let memo = memo
        .map(|m| Memo::from_str(&m).map_err(|e| anyhow!("memo: {e}")))
        .transpose()?;
    // Shielded only: a transparent or Sapling payment would show its amount on chain.
    let shielded = matches!(
        Address::decode(&wallet.network, &to),
        Some(Address::Unified(ua)) if ua.orchard().is_some()
    );
    if !shielded {
        return Err(anyhow!(
            "zsettle pays Orchard receivers only: give a unified address of this network with an Orchard receiver"
        ));
    }
    let payment = Payment::new(
        ZcashAddress::from_str(&to).map_err(|e| anyhow!("address: {e}"))?,
        Some(Zatoshis::from_u64(value_zat).map_err(|_| anyhow!("value"))?),
        memo.as_ref().map(MemoBytes::from),
        None,
        None,
        vec![],
    )
    .map_err(|e| anyhow!("invalid payment: {e:?}"))?;
    let request =
        TransactionRequest::new(vec![payment]).map_err(|e| anyhow!("payment request: {e}"))?;
    pay_request(
        wallet,
        db,
        seed,
        from_account,
        from_account_index,
        request,
        min_confirmations,
    )
}

/// Pay a whole request from an account of this wallet (the library proposes, proves and
/// stores the transaction). Returns the raw bytes and the id; the caller broadcasts.
fn pay_request(
    wallet: &Wallet,
    db: &mut Db,
    seed: &SecretVec<u8>,
    from_account: AccountUuid,
    from_account_index: u32,
    request: TransactionRequest,
    min_confirmations: Option<u32>,
) -> Result<(Vec<u8>, String)> {
    #[cfg(feature = "native")]
    let prover = LocalTxProver::bundled();
    #[cfg(not(feature = "native"))]
    let prover = no_sapling::NoSapling;
    let change_strategy = MultiOutputChangeStrategy::new(
        StandardFeeRule::Zip317,
        None,
        ShieldedPool::Ironwood,
        DustOutputPolicy::default(),
        SplitPolicy::with_min_output_value(
            NonZeroUsize::new(1).expect("nonzero"),
            Zatoshis::from_u64(10_000).expect("ok"),
        ),
    );
    let input_selector = GreedyInputSelector::new();
    let proposal =
        propose_transfer::<_, _, _, _, zcash_client_sqlite::wallet::commitment_tree::Error>(
            db,
            &wallet.network,
            from_account,
            &input_selector,
            &change_strategy,
            request,
            min_confirmations.map_or_else(
                ConfirmationsPolicy::default,
                crate::wallet::confirmations_policy,
            ),
            &SpendPolicy::default(),
            None,
            None,
        )
        .map_err(|e| anyhow!("proposing: {e}"))?;

    let usk = wallet.spending_key(seed, from_account_index)?;
    let txids = create_proposed_transactions::<
        _,
        _,
        zcash_client_backend::data_api::wallet::input_selection::GreedyInputSelectorError,
        _,
        zcash_primitives::transaction::fees::zip317::FeeError,
        _,
    >(
        db,
        &wallet.network,
        &WalletClock,
        &mut rand::rng(),
        &prover,
        &prover,
        &SpendingKeys::from_unified_spending_key(usk),
        OvkPolicy::Sender,
        &proposal,
        None,
    )
    .map_err(|e| anyhow!("creating transaction: {e}"))?;
    let txid = *txids.first();
    let tx = db
        .get_transaction(txid)
        .map_err(|e| anyhow!("{e}"))?
        .ok_or_else(|| anyhow!("transaction {txid} not stored"))?;
    // The library records an output to another account of the same wallet as a plain sent
    // output until a block brings it back. Decrypting the transaction with the wallet's own
    // keys records the new notes now, so new items show (as arriving) before the block.
    if let Err(e) = zcash_client_backend::data_api::wallet::decrypt_and_store_transaction(
        &wallet.network,
        db,
        &tx,
        None,
    ) {
        eprintln!("warning: recording the new notes: {e}");
    }
    let mut bytes = Vec::new();
    tx.write(&mut bytes)?;
    Ok((bytes, txid.to_string()))
}

/// The browser build pays from Ironwood only: the wallet library still wants Sapling provers,
/// and this one is never called.
#[cfg(not(feature = "native"))]
mod no_sapling {
    use sapling_crypto::{
        bundle::GrothProofBytes,
        circuit,
        keys::EphemeralSecretKey,
        prover::{OutputProver, SpendProver},
        value::{NoteValue, ValueCommitTrapdoor},
        Diversifier, MerklePath, PaymentAddress, ProofGenerationKey, Rseed,
    };

    pub struct NoSapling;

    const NEVER: &str = "this build makes no Sapling proofs";

    impl SpendProver for NoSapling {
        type Proof = ();

        fn prepare_circuit(
            _: ProofGenerationKey,
            _: Diversifier,
            _: Rseed,
            _: NoteValue,
            _: jubjub::Fr,
            _: ValueCommitTrapdoor,
            _: bls12_381::Scalar,
            _: MerklePath,
        ) -> Option<circuit::Spend> {
            None
        }

        fn create_proof<R: rand_core::Rng>(&self, _: circuit::Spend, _: &mut R) {
            unreachable!("{NEVER}")
        }

        fn encode_proof(_: ()) -> GrothProofBytes {
            unreachable!("{NEVER}")
        }
    }

    impl OutputProver for NoSapling {
        type Proof = ();

        fn prepare_circuit(
            _: &EphemeralSecretKey,
            _: PaymentAddress,
            _: jubjub::Fr,
            _: NoteValue,
            _: ValueCommitTrapdoor,
        ) -> circuit::Output {
            unreachable!("{NEVER}")
        }

        fn create_proof<R: rand_core::Rng>(&self, _: circuit::Output, _: &mut R) {
            unreachable!("{NEVER}")
        }

        fn encode_proof(_: ()) -> GrothProofBytes {
            unreachable!("{NEVER}")
        }
    }
}
