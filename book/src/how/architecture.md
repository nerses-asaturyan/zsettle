# Architecture

## Crates

The workspace (`Cargo.toml`) has four crates:

| Crate | Path | What it is |
|---|---|---|
| `zsettle-core` | `crates/zsettle-core` | Library: items, signed listings, the two-party bundle, PCZT roles, seller policy, proof check, wallet sync, provenance tracing, lightwalletd client. |
| `zsettle` | `crates/zsettle` | Buyer and verifier CLI. |
| `zsettle-seller` | `crates/zsettle-seller` | Seller CLI and the seller service (`serve`), with its own small HTTP server. |
| `zsettle-app` | `crates/zsettle-app` | Web page and HTTP server. The page (`web/`) is built into the binary. It runs the two CLIs as subprocesses. |

Modules of `zsettle-core` (`crates/zsettle-core/src/`):

| Module | Job |
|---|---|
| `item.rs` | What an item is; minimum item value. |
| `mint.rs` | Minting items, plain payments (`pay`), listing a wallet's notes. |
| `listing.rs` | The listings file: read, write, sign, verify, fingerprint. |
| `trade.rs` | Building and proving the trade, and self-transfers (`rotate`, `consolidate`, `cancel`). |
| `pczt.rs` | PCZT helpers: redact, verify proof, sign, combine, extract. |
| `policy.rs` | The seller's checks on an offer. |
| `verify.rs` | Item history from compact blocks. |
| `wallet.rs` | Wallet directory, keys, accounts, sync, note reservations. |
| `broadcast.rs` | lightwalletd connection, consensus-branch check, broadcast. |

The Zcash stack is the published librustzcash crates (`zcash_client_backend`,
`zcash_client_sqlite`, `pczt`, `orchard` and others) on `main`, and their NU7 pre-release
versions on the `nu7-testnet` branch.

## How they call each other

```
              browser (web/ page)
                     |
                     | HTTP on 127.0.0.1
                     v
               +-------------+
               | zsettle-app |----- starts as subprocesses ----+
               +-------------+                                 |
                 |         |                                   |
        runs     |         |  runs                             |
                 v         v                                   v
        +---------+   +----------------+   long-running   +----------------------+
        | zsettle |   | zsettle-seller |   ------------>  | zsettle-seller serve |
        +---------+   +----------------+                  |  inbox / outbox      |
             \              /                             |  HTTP: /listings,    |
              \            /                              |  /offer, /reply/<tag>|
               v          v                               +----------------------+
           +----------------+                                       |
           |  zsettle-core  |  <------------------------------------+
           +----------------+
                   |
                   | gRPC over TLS
                   v
            lightwalletd server
```

- The CLIs link `zsettle-core` directly.
- `zsettle-app` links `zsettle-core` only to read listings files, check a wallet passphrase
  and decrypt the backup words when asked. Every wallet action is a run of `zsettle` or `zsettle-seller`. The app reads the
  JSON they print. It removes the `ZSETTLE_*` variables from their environment and passes the
  wallet passphrase in `ZSETTLE_PASSPHRASE`, so the user's shell settings cannot point a
  command at another wallet.
- A shop is a long-running `zsettle-seller serve --listen ...` process. The app reads its
  JSON event lines to show the shop's activity. While another wallet action runs, the app
  stops the shop and starts it again afterwards, because both use the same wallet database.
- The buyer's `zsettle buy --seller-url` and `zsettle finish` talk to the shop over HTTP.

The wallet the app creates is made by `zsettle-seller init`, so it has a `payout` account
(index 0) and an `item` account (index 1). The app buys with the payout account and sells from
the item account.

## Local mode

One profile, one wallet, on one machine. The server listens on 127.0.0.1 and serves the page
and a JSON API under `/api/`. Every API call must carry the profile's page key. See
[Security model](../security.md#local-mode).

## Hosted mode

`zsettle-app --public https://<domain>` serves many visitors from one server, behind a TLS
proxy (the setup script uses Caddy).

```
visitor --HTTPS--> Caddy --> zsettle-app (127.0.0.1:8080)
                                |-- /api/...           one profile per signed-in visitor
                                |-- /shop/<code>/...   relay --> that profile's shop on 127.0.0.1:<port>
                                '-- house profile      restocks itself, pays test coins
```

- **Profiles.** Each visitor's wallet is a profile in the data directory, named by a random
  eight-character wallet code. `hub.json` maps passkey credential ids to profiles, keeps each
  profile's shop port, and records who got test coins.
- **Sign-in.** A visitor signs in with a passkey (the WebAuthn PRF output is the wallet
  passphrase, with one PRF salt for the whole site, kept in `prf.salt`) or with the wallet code
  and its passphrase. Sign-in sets a session cookie. The server keeps the passphrase in memory
  while the wallet is unlocked, because it runs the commands.
- **Shops behind a relay.** Each profile's shop listens on its own fixed port from 19100 up,
  on 127.0.0.1. The public address of a shop is `https://<domain>/shop/<code>`. The relay
  passes only three requests: `GET listings`, `POST offer` and `GET reply/<tag>`. While a
  wallet action pauses a shop, the relay still serves the shop's signed listings file, and
  holds an offer or a reply request for up to 25 seconds while it waits for the shop to open
  again.
- **Unused wallets.** A wallet that was never synced, got no test coins and listed nothing is
  removed after three days.
- **State files.** `hub.json` and each `app.json` are written to a new file and renamed into
  place. If one cannot be read, the server stops at start instead of starting empty.
- **Own shops only.** In hosted mode the market and the history only open shops of this
  server, so a visitor cannot make the server fetch or post to any other address.
- **House shop.** With `--house-passphrase-file`, a profile named `house` is created if
  missing, unlocked at start, and checked every 45 seconds: it keeps six items listed at
  0.0005 ZEC and mints a new batch when two or fewer are left. It also pays each new wallet
  300,000 zatoshis of test coins once. Nobody can sign in to the house profile.

Limits and protections of hosted mode are in
[Security model and limits](../security.md#hosted-mode).
