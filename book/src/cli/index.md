# CLI guide

zsettle has two command-line programs:

- `zsettle`: the buyer, and anyone who wants to check an item's history;
- `zsettle-seller`: the seller, including the service that signs offers (`serve`).

Both create and use wallets in the same layout, so one wallet can be used by both programs.
A buyer who wants to resell an item runs `zsettle-seller` on the buyer wallet (see
[Resale](resale.md)).

The pages in this chapter:

- [Wallets](wallets.md): `init`, `accounts`, `address`, `sync`, funding, `pay`.
- [Selling](seller.md): `new-item-account`, `mint`, `items`, `publish`, `serve`, `sign`,
  `cancel`.
- [Buying](buyer.md): `list`, `import-listing`, `buy`, `finish`, `broadcast`, `consolidate`,
  `rotate`, `notes`.
- [Resale and item history](resale.md): `buy --receive-account`, `verify`.

The full list of flags is in [CLI commands](../reference/cli.md).

## Things every command shares

**Environment variables.** `init` writes `env.sh` (and `env.ps1` for PowerShell) into the
wallet directory. After `source <wallet>/env.sh`, these variables fill in the flags:

| Variable | Flag it fills | Set by |
|---|---|---|
| `ZSETTLE_WALLET` | `--wallet` | both `init`s |
| `ZSETTLE_IDENTITY` | `--identity` | both `init`s, when `--identity` was given |
| `ZSETTLE_BUYER_ACCOUNT` | `--buyer-account`, `--account` (buyer commands) | `zsettle init` |
| `ZSETTLE_PAYOUT_ACCOUNT` | `--payout-account`, `--account` of `zsettle-seller pay` | `zsettle-seller init` |
| `ZSETTLE_ITEM_ACCOUNT` | `--item-account` | `zsettle-seller init` |
| `ZSETTLE_ITEM_ACCOUNT_INDEX` | `--item-account-index` | `zsettle-seller init` (value `1`) |
| `ZSETTLE_LISTINGS` | `--listings` | `zsettle-seller init` (`<wallet>/listings.json`) |

Two more variables are read but never written to a file:

- `ZSETTLE_PASSPHRASE`: the passphrase of a passphrase-protected wallet, for scripts. Without
  it the commands ask for the passphrase.
- `ZSETTLE_LOG`: when set, the wallet library prints its progress lines. By default the
  tools are quiet.

**Key file or passphrase.** With `--identity <file>`, the mnemonic is encrypted to an age
identity in that file. Without it, the mnemonic is protected by a passphrase (age's scrypt
recipient). A command that needs the mnemonic takes `--identity` again, or asks for the
passphrase.

**Amounts.** Prices and amounts take zatoshis (`5001`) or ZEC with a decimal point (`0.001`,
`0.001 ZEC`).

**Note references.** Notes are written `<txid>:<index>`, where the index is the action index
in the transaction. `mint`, `items` and `notes` print them in this form.

**Light server.** Every command that talks to the network takes `--server`. On `main` the
default is `https://zec.rocks:443`. Before building, minting or broadcasting, the tools ask the
server which consensus branch is live and stop with a message if this build does not sign for
it.

**Output.** Commands that act print one JSON line on stdout (`verify` prints a pretty JSON
report). `list`, `notes` and `items` print tables. Warnings and notes go to stderr.
