# Files and data directories

## Wallet directory

Made by `zsettle init` or `zsettle-seller init`, in the zcash-devtool layout. On Unix the
directory is owner-only.

| File | What it holds |
|---|---|
| `keys.toml` | `mnemonic` (the 24 words, age-encrypted and ASCII-armored), `network` (`main` or `test`) and `birthday`. Owner-only. |
| `data.sqlite` | The wallet database (`zcash_client_sqlite`): accounts, viewing keys, notes, commitment trees, history. Holds no spending key. |
| `env.sh`, `env.ps1` | Environment variables for the other commands (see [CLI guide](../cli/index.md)). |
| `reserved-notes.json` | Notes committed to offers: `{"notes": [{"note": "<txid>:<index>", "expiry_height": <h>}]}`. `buy` skips them; `rotate` retires them. Owner-only. |
| `listings.json` | The seller's listings file, when `ZSETTLE_LISTINGS` from `zsettle-seller init` is used. |

The **identity file** (`--identity`) is a standard age X25519 identity, with a
`# public key: ...` comment line. It is created owner-only. Keep it outside the wallet
directory.

## Trade files

| File | Written by | Private? |
|---|---|---|
| `<out>` (for example `trade.pczt`) | `buy --out` | **Yes.** Full PCZT with the buyer's viewing key and witnesses. Owner-only. |
| `<out>.seller.pczt`, or `--seller-copy` | `buy` | No. The redacted copy for the seller. |
| `<out>.reply` | `buy --seller-url` | The reply URL, one line. Owner-only. |
| `<out>` (for example `trade.tx`) | `finish --out` | The raw transaction. |
| `<out>.pczt` | `finish` | **Yes.** The final PCZT, with the buyer's viewing key and witnesses. Owner-only. |

PCZT files use the `pczt` crate's own encoding.

## Seller service directories

| Path | What it holds |
|---|---|
| `<inbox>/*.pczt` | Envelopes waiting to be checked. Posted offers are named `<tag>.pczt`. |
| `<inbox>/done/` | Envelopes that were signed. Deleted after a day. |
| `<inbox>/rejected/<name>.reason.txt` | Why an envelope was refused (the envelope itself is deleted). At most 10,000 kept; deleted after a day. |
| `<outbox>/<name>.<tag>.signed.pczt` | Signed replies. Deleted after a day. |

`serve` creates these directories when they are missing. Writes go through a temporary file and
a rename, so a reader never sees half a file.

## zsettle-app, local mode

`~/.zsettle-app/` (or `--data-dir`), owner-only, with one directory per profile:

```
~/.zsettle-app/<profile>/
  app.json        settings and history the app keeps: accounts, passkey id and salt, item names,
                  activity log, shops opened, shop listen address, last sync
  page.token      the page key (32 hex characters), owner-only
  wallet/         the wallet directory (see above), made by zsettle-seller init
  listings.json   this profile's listings file
  trades/         trade-<unix time>-<8 hex>.pczt and .tx of each purchase; proof-*.json and
                  faucet-*.tx, removed after an hour
  inbox/ outbox/  the shop's directories
```

`app.json` holds nothing secret: the mnemonic stays encrypted in the wallet directory, and a
passkey entry is only its id and salt.

## zsettle-app, hosted mode

The data directory (`/var/lib/zsettle/data` with the setup script):

```
hub.json          the house shop's port; addresses that got test coins; faucet payments under way
house/            the house shop's profile, laid out as in local mode
```

A hosted server keeps no visitor wallets. Browser shops' mailboxes are kept in memory only.
Folders and files left by an older hosted mode (one folder per visitor wallet, `prf.salt`) are
not loaded.

`hub.json` and the house's `app.json` are written owner-only to a new file and then renamed
into place. If either cannot be read (damaged JSON), `zsettle-app` refuses to start: restore it
from a backup, or move it away to start empty. A missing file just starts empty. The same
holds for a profile's `app.json` in local mode.

The setup script keeps the house passphrase in `/var/lib/zsettle/house.pass` (owner-only),
outside the data directory, and the wallet engine in `/usr/local/share/zsettle-web/www`.

## The wallet in the browser

On a hosted server the wallet lives in the visitor's browser, in the IndexedDB database
`zsettle-wallet`, store `wallets`, one record per network (`wallet-test`, `wallet-main`). A
record holds the network, the birthday, how the wallet is locked (a passkey's credential id
and PRF salt, or a passphrase's PBKDF2 salt), and three AES-GCM boxes: the 24 words, the
wallet database, and the app's own data (activity, item details, shops opened, the shop's
listings, last sync).

## Listings file

See [Listings and signatures](../how/listings.md).
