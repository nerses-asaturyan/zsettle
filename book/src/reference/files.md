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
  trades/         trade-<unix time>.pczt and related files of each purchase
  inbox/ outbox/  the shop's directories
```

`app.json` holds nothing secret: the mnemonic stays encrypted in the wallet directory, and a
passkey entry is only its id and salt.

## zsettle-app, hosted mode

The data directory (`/var/lib/zsettle/data` with the setup script):

```
hub.json          passkey credential id -> wallet code; wallet code -> shop port; wallets that got test coins
prf.salt          the site's passkey PRF salt; keep it, or every passkey stops working
<code>/           one profile per visitor wallet, laid out as in local mode (no page.token)
house/            the house shop's profile
```

`hub.json` and each `app.json` are written owner-only to a new file and then renamed into
place. If either cannot be read (damaged JSON), `zsettle-app` refuses to start: restore it
from a backup, or move it away to start empty. A missing file just starts empty. The same
holds for a profile's `app.json` in local mode.

The setup script keeps the house passphrase in `/var/lib/zsettle/house.pass` (owner-only),
outside the data directory.

## Listings file

See [Listings and signatures](../how/listings.md).
