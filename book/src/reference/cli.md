# CLI commands

This page lists the commands and flags of the `main` branch, taken from the clap definitions
in `crates/zsettle/src/main.rs`, `crates/zsettle-seller/src/main.rs` and
`crates/zsettle-app/src/main.rs`. An environment variable in brackets fills the flag when it is
not given.

Common values:

- `--server <url>`: lightwalletd server. Default `https://zec.rocks:443`.
- `--identity <file>`: age identity file that decrypts the mnemonic. Leave it out for a
  passphrase-protected wallet; the passphrase is asked for, or read from `ZSETTLE_PASSPHRASE`.
- Amounts: zatoshis, or ZEC with a decimal point (`0.001`, `0.001 ZEC`).
- Notes: `<txid>:<index>`.

## zsettle

| Command | What it does |
|---|---|
| `init` | Create a wallet with accounts `buyer` (index 0) and `item` (index 1). |
| `accounts` | List the wallet's accounts. |
| `address` | Print a fresh Orchard-only address of an account. |
| `sync` | Sync the wallet. |
| `list` | Show a listings file and whether its signature checks out. |
| `import-listing` | Import a listing's item viewing key as a view-only account. |
| `buy` | Build, prove and redact a trade; optionally post the offer. |
| `finish` | Combine with the seller's signed copy, sign, extract, optionally broadcast. |
| `broadcast` | Send a raw transaction file. |
| `verify` | Trace an item's history with viewing keys only. |
| `check-proof` | Check an ownership proof against your challenge and the chain. |
| `rotate` | Retire the paying notes of offers that did not settle. |
| `consolidate` | Merge an account's spendable notes into one. |
| `pay` | Send ZEC to any unified address with an Orchard receiver. |
| `notes` | List an account's unspent Ironwood notes. |

### zsettle init

| Flag | Default | Meaning |
|---|---|---|
| `--wallet <dir>` | required | Directory to create. |
| `--identity <file>` [`ZSETTLE_IDENTITY`] | | age identity file, generated when missing. Omit for a passphrase. |
| `--network <main\|test>` | `main` | Network. |
| `--server <url>` | default server | Used for the birthday. |

### zsettle accounts

`--wallet <dir>` [`ZSETTLE_WALLET`].

### zsettle address

`--wallet <dir>` [`ZSETTLE_WALLET`], `--account <uuid>` [`ZSETTLE_BUYER_ACCOUNT`].

### zsettle sync

`--wallet <dir>` [`ZSETTLE_WALLET`], `--server <url>`.

### zsettle list

`--listings <path or url>` [`ZSETTLE_LISTINGS`].

### zsettle import-listing

| Flag | Default | Meaning |
|---|---|---|
| `--wallet <dir>` [`ZSETTLE_WALLET`] | required | Buyer wallet. |
| `--listings <path or url>` [`ZSETTLE_LISTINGS`] | required | Must be signed. |
| `--name <name>` | `item-view` | Name for the view-only account. |
| `--server <url>` | default server | Used for the account birthday. |

### zsettle buy

| Flag | Default | Meaning |
|---|---|---|
| `--wallet <dir>` [`ZSETTLE_WALLET`] | required | Buyer wallet. |
| `--buyer-account <uuid>` [`ZSETTLE_BUYER_ACCOUNT`] | required | Spending account. |
| `--listings <path or url>` [`ZSETTLE_LISTINGS`] | | Fills in seller address, price and expiry. Must be signed. |
| `--item <note>` | required | The item note. |
| `--item-account <uuid>` | from the listings | The imported view-only account that holds the item. |
| `--receive-account <uuid>` | the wallet's `item` account | Account that receives the item. |
| `--buyer-note <note>` | largest unreserved spendable note | Note that pays. |
| `--seller <address>` | from the listings | Seller's unified address. |
| `--price <amount>` | from the listings | Price to pay. |
| `--expiry-delta <blocks>` | from the listings, at most 40 | Blocks until expiry; 1 to 40. |
| `--min-confirmations <n>` | `3` | Confirmations required on the inputs. |
| `--memo <text>` | empty | Memo of the payment output. |
| `--server <url>` | default server | Asked which consensus branch is live. |
| `--offline` | off | Skip the branch check. |
| `--out <file>` | required | Full PCZT. Keep it private. |
| `--seller-copy <file>` | `<out>.seller.pczt` | Redacted copy for the seller. |
| `--seller-url <url>` | | Also post the copy to `<url>/offer` and save the reply URL in `<out>.reply`. |

### zsettle finish

| Flag | Default | Meaning |
|---|---|---|
| `--wallet <dir>` [`ZSETTLE_WALLET`] | required | Buyer wallet. |
| `--identity <file>` [`ZSETTLE_IDENTITY`] | | Key file, or omit for a passphrase. |
| `--account-index <n>` | `0` | ZIP 32 index of the paying account. |
| `--full <file>` | required | The full PCZT from `buy`. |
| `--signed <file or url>` | the URL in `<full>.reply` | The seller's signed copy. A URL is polled. |
| `--wait <seconds>` | `300` | How long to poll a reply URL. |
| `--out <file>` | required | Raw transaction. The final PCZT goes to `<out>.pczt`. |
| `--broadcast` | off | Send the transaction after extraction. |
| `--server <url>` | default server | |

### zsettle broadcast

`--tx <file>` (required), `--network <main|test>` (default `main`), `--server <url>`.

### zsettle verify

| Flag | Default | Meaning |
|---|---|---|
| `--listings <path or url>` [`ZSETTLE_LISTINGS`] | required, repeatable | One per holder. Must be signed. |
| `--item <note>` | required | Genesis note, or any later note a listing decrypts. |
| `--server <url>` | default server | |
| `--to-height <height>` | chain tip | Stop scanning here. |
| `--private` | off | Do not ask the server where the genesis transaction is; scan from the birthdays; no name. |

### zsettle check-proof

Checks a proof made by `zsettle-seller prove`. See [Ownership proofs](../how/ownership.md).

| Flag | Default | Meaning |
|---|---|---|
| `--proof <path or url>` | required | The proof file, or an `http://` or `https://` URL. At most 16 KiB. |
| `--challenge <text>` | required | The challenge you gave the holder. |
| `--network <main\|test>` | `main` | Network the proof must be for. |
| `--server <url>` | default server | |

It prints JSON with `valid`, `held`, `note`, `name`, `challenge`, `signed_at_height`,
`scanned_to` and, when the check fails, `reason`. The exit status is 0 only when the proof is
valid and the note is held; otherwise 1.

### zsettle rotate

| Flag | Default | Meaning |
|---|---|---|
| `--wallet <dir>` [`ZSETTLE_WALLET`] | required | |
| `--account <uuid>` [`ZSETTLE_BUYER_ACCOUNT`] | required | Paying account the reservations belong to. |
| `--account-index <n>` | `0` | Its ZIP 32 index. |
| `--identity <file>` [`ZSETTLE_IDENTITY`] | | |
| `--min-confirmations <n>` | `3` | |
| `--server <url>` | default server | |
| `--dry-run` | off | Report only. |
| `--no-sync` | off | Skip the sync first. |

### zsettle consolidate

Same flags as `rotate`: `--wallet`, `--account`, `--account-index`, `--identity`,
`--min-confirmations`, `--server`, `--dry-run`, `--no-sync`.

### zsettle pay

| Flag | Default | Meaning |
|---|---|---|
| `--wallet <dir>` [`ZSETTLE_WALLET`] | required | |
| `--account <uuid>` [`ZSETTLE_BUYER_ACCOUNT`] | required | Paying account. |
| `--account-index <n>` | `0` | Its ZIP 32 index. |
| `--identity <file>` [`ZSETTLE_IDENTITY`] | | |
| `--to <address>` | required | Unified address with an Orchard receiver. |
| `--amount <amount>` | required | |
| `--memo <text>` | | Optional memo. |
| `--server <url>` | default server | |

### zsettle notes

`--wallet <dir>` [`ZSETTLE_WALLET`], `--account <uuid>` [`ZSETTLE_BUYER_ACCOUNT`].

## zsettle-seller

Several commands share the **wallet flags**:

| Flag | Default | Meaning |
|---|---|---|
| `--wallet <dir>` [`ZSETTLE_WALLET`] | required | Seller wallet. |
| `--item-account <uuid>` [`ZSETTLE_ITEM_ACCOUNT`] | required | Item account. |
| `--item-account-index <n>` [`ZSETTLE_ITEM_ACCOUNT_INDEX`] | `1` | Its ZIP 32 index (derives the signing key). |
| `--payout-account <uuid>` [`ZSETTLE_PAYOUT_ACCOUNT`] | required | Account that receives payments. |

| Command | What it does |
|---|---|
| `init` | Create a wallet with accounts `payout` (index 0) and `item` (index 1). |
| `accounts` | List the wallet's accounts. |
| `address` | Print a fresh Orchard-only address of an account. |
| `pay` | Send ZEC from the payout account to any address. |
| `sync` | Sync the wallet. |
| `new-item-account` | Create another item account. |
| `mint` | Mint items. |
| `items` | List the item account's unspent notes. |
| `publish` | Create or update the signed listings file. |
| `cancel` | Take an item off the market for good. |
| `prove` | Sign an ownership proof for an item note, bound to a checker's challenge. |
| `sign` | Check and sign one trade PCZT. |
| `serve` | Run the seller service. |

### zsettle-seller init

Same flags as `zsettle init`: `--wallet`, `--identity` [`ZSETTLE_IDENTITY`], `--network`
(default `main`), `--server`.

### zsettle-seller accounts

`--wallet <dir>` [`ZSETTLE_WALLET`].

### zsettle-seller address

`--wallet <dir>` [`ZSETTLE_WALLET`], `--account <uuid>` (required, no variable).

### zsettle-seller pay

| Flag | Default | Meaning |
|---|---|---|
| `--wallet <dir>` [`ZSETTLE_WALLET`] | required | |
| `--account <uuid>` [`ZSETTLE_PAYOUT_ACCOUNT`] | required | Paying account. |
| `--account-index <n>` | `0` | |
| `--identity <file>` [`ZSETTLE_IDENTITY`] | | |
| `--to <address>` | required | |
| `--amount <amount>` | required | |
| `--memo <text>` | | |
| `--server <url>` | default server | |

### zsettle-seller sync

`--wallet <dir>` [`ZSETTLE_WALLET`], `--server <url>`.

### zsettle-seller new-item-account

`--wallet <dir>` [`ZSETTLE_WALLET`], `--identity <file>` [`ZSETTLE_IDENTITY`], `--name <name>`
(required), `--server <url>` (for the birthday).

### zsettle-seller mint

| Flag | Default | Meaning |
|---|---|---|
| wallet flags | | |
| `--identity <file>` [`ZSETTLE_IDENTITY`] | | |
| `--payout-account-index <n>` | `0` | ZIP 32 index of the payout account. |
| `--name <name>` | required, repeatable | Item name, goes into the memo. One item per `--name`, all in one transaction. |
| `--value <amount>` | `1000` zat | Value of each item note. |
| `--server <url>` | default server | |

### zsettle-seller items

`--wallet <dir>` [`ZSETTLE_WALLET`], `--item-account <uuid>` [`ZSETTLE_ITEM_ACCOUNT`].

### zsettle-seller publish

| Flag | Default | Meaning |
|---|---|---|
| wallet flags | | |
| `--identity <file>` [`ZSETTLE_IDENTITY`] | | |
| `--listings <file>` [`ZSETTLE_LISTINGS`] | required | File to create or update. |
| `--item-note <note>` | | Item to list. Without it the file is checked and signed again. |
| `--name <name>` | empty | Display name. |
| `--price <amount>` | required with `--item-note` | More than 5,000 zatoshis. |
| `--max-expiry-delta <blocks>` | `40` | Most blocks between the tip and an offer's expiry. |
| `--remove <note>` | | Delist this item (no spend). |
| `--resign-unverified` | off | Sign a file whose signature does not verify. |

### zsettle-seller cancel

| Flag | Default | Meaning |
|---|---|---|
| wallet flags | | |
| `--identity <file>` [`ZSETTLE_IDENTITY`] | | |
| `--item-note <note>` | required | The listed item note. |
| `--payout-account-index <n>` | `0` | Signs the fee-paying spend. |
| `--listings <file>` [`ZSETTLE_LISTINGS`] | | File to drop the item from. |
| `--min-confirmations <n>` | `3` | |
| `--server <url>` | default server | |
| `--no-sync` | off | Skip the sync first. |

### zsettle-seller prove

Prints a signed [ownership proof](../how/ownership.md) for `zsettle check-proof`. The proof
gives the checker the item account's viewing key: they can see every item that account has
held. It does not need `--payout-account`.

| Flag | Default | Meaning |
|---|---|---|
| `--wallet <dir>` [`ZSETTLE_WALLET`] | required | |
| `--item-account <uuid>` [`ZSETTLE_ITEM_ACCOUNT`] | required | |
| `--item-account-index <n>` [`ZSETTLE_ITEM_ACCOUNT_INDEX`] | `1` | Derives the signing key. |
| `--identity <file>` [`ZSETTLE_IDENTITY`] | | |
| `--item-note <note>` | required | An unspent, mined note of the item account. |
| `--challenge <text>` | required | The checker's text: 1 to 200 characters, no line breaks. |
| `--out <file>` | | Also write the proof here. |
| `--server <url>` | default server | |
| `--no-sync` | off | Skip the sync first. |

### zsettle-seller sign

| Flag | Default | Meaning |
|---|---|---|
| wallet flags | | |
| `--identity <file>` [`ZSETTLE_IDENTITY`] | | |
| `--listings <file>` [`ZSETTLE_LISTINGS`] | required | |
| `--input <file>` | required | Incoming redacted PCZT. |
| `--out <file>` | required | Signed PCZT. |
| `--dry-run` | off | Check only. |
| `--server <url>` | default server | For the sync first. |
| `--no-sync` | off | |

### zsettle-seller serve

| Flag | Default | Meaning |
|---|---|---|
| wallet flags | | |
| `--identity <file>` [`ZSETTLE_IDENTITY`] | | |
| `--listings <file>` [`ZSETTLE_LISTINGS`] | required | Kept in sync with the chain. |
| `--inbox <dir>` | required | Watched for `*.pczt`. |
| `--outbox <dir>` | required | Replies `<name>.<tag>.signed.pczt`. |
| `--poll-secs <n>` | `5` | Seconds between passes. |
| `--server <url>` | default server | For the built-in sync. |
| `--no-sync` | off | Do not sync. |
| `--sync-cmd <command>` | | Shell command instead of the built-in sync. |
| `--listen <address>` | | Also serve HTTP here, for example `127.0.0.1:8900`. |
| `--once` | off | One pass, then exit. |

## zsettle-app

| Flag | Default | Meaning |
|---|---|---|
| `--profile <name>` | `default` | Local mode profile. |
| `--data-dir <dir>` | `~/.zsettle-app` | Where profiles live. |
| `--port <port>` | `7070` | Port on 127.0.0.1. |
| `--bin-dir <dir>` | next to `zsettle-app` | Where `zsettle` and `zsettle-seller` are. |
| `--new-token` | off | Local mode: new page key. |
| `--public <origin>` | | Hosted mode. |
| `--network <main\|test>` | `test` | Hosted mode: network of new wallets. A `main` build refuses `test`. |
| `--house-passphrase-file <file>` | | Hosted mode: run the house shop. |
| `--allow-hosted-mainnet` | off | Hosted mode on mainnet. |
