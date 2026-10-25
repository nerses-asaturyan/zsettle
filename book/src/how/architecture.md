# Architecture

## Crates

The workspace (`Cargo.toml`) has four crates:

| Crate | Path | What it is |
|---|---|---|
| `zsettle-core` | `crates/zsettle-core` | Library: items, signed listings, the two-party bundle, PCZT roles, seller policy, proof check, wallet sync, provenance tracing, lightwalletd client. |
| `zsettle` | `crates/zsettle` | Buyer and verifier CLI. |
| `zsettle-seller` | `crates/zsettle-seller` | Seller CLI and the seller service (`serve`), with its own small HTTP server. |
| `zsettle-app` | `crates/zsettle-app` | Web page and HTTP server. The page (`web/`) is built into the binary. In local mode it runs the two CLIs as subprocesses. |

A fifth crate, `zsettle-web` (`crates/zsettle-web`), builds on its own (nightly, WebAssembly)
and is left out of the workspace. It is `zsettle-core` compiled for the browser: the wallet,
its sync, the buyer's side of a trade (`lib.rs`), selling and the shop's checks (`seller.rs`),
and item histories and proof checks (`history.rs`). `www/engine.js` and `www/worker.js` run it
in the page. A hosted `zsettle-app` serves it with the page.

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
| `ownership.rs` | Ownership proofs: make, sign and check. |
| `wallet.rs` | Wallet directory, keys, accounts, sync, note reservations. |
| `broadcast.rs` | lightwalletd connection, consensus-branch check, broadcast. |

The Zcash stack is the published librustzcash crates of the NU7 release line
(`zcash_client_backend`, `zcash_client_sqlite`, `pczt`, `orchard` and others).

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
- `zsettle-app` links `zsettle-core` to read and check listings files, check a wallet
  passphrase and decrypt the backup words when asked; in hosted mode also for the chain relay,
  the checks on browser shops' mailbox requests and the house shop's view of the chain tip. Every wallet action is a run of `zsettle` or `zsettle-seller`. The app reads the
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

`zsettle-app --public https://<domain> --web-wallet <dir>` serves many visitors from one
server, behind a TLS proxy (the setup script uses Caddy). It keeps no visitor wallets: the
wallet runs in each visitor's page.

```
visitor's page (app.js + engine.js + WebAssembly wallet)
    |
    | HTTPS
    v
Caddy --> zsettle-app (127.0.0.1:8080)
             |-- /, /engine.js, /pkg/...   the page and the wallet engine
             |-- /api/chain/...            relay --> lightwalletd
             |-- /api/mailbox/<code>/...   a browser shop's owner (signed with its item key)
             |-- /shop/<code>/...          buyers --> a browser shop's mailbox,
             |                                        or the house shop on 127.0.0.1:<port>
             '-- house profile             restocks itself, pays test coins
```

- **The wallet in the page.** On a hosted server the page asks `/api/hello`, loads
  `/engine.js`, and from then on the engine answers the page's wallet calls inside the
  browser, in place of the local app's API. The engine keeps the 24 words and the
  wallet database in IndexedDB, encrypted under a key from a passkey (PRF) or a passphrase
  (PBKDF2), with a copy per network: the same words and lock, that network's own keys and
  data. The WebAssembly wallet runs in a worker; proofs use a
  thread pool.
- **Chain relay.** The page fetches compact blocks, tree states and transactions through
  `/api/chain/...` and sends its transactions there. The server passes each call to its light
  server, for either network.
- **Browser shops.** Minting, listing, delisting and cancelling run in the page. When the
  owner opens the shop, the page keeps the item key in the worker, uploads the signed listings
  to a mailbox on the server, and asks for offers every few seconds. It checks each offer with
  the seller service's checks, signs its half, and posts the reply. The shop's code is the
  fingerprint of its listings key. Every mailbox request is signed with the item key. The shop
  counts as open while its page asked for offers in the last 45 seconds.
- **Shops for buyers.** The public address of a shop is `https://<domain>/shop/<code>`. Only
  three requests pass: `GET listings`, `POST offer` and `GET reply/<tag>`. A browser shop is
  answered from its mailbox. The house shop listens on its own fixed port from 19100 up, on
  127.0.0.1. While a wallet action pauses it, the relay still serves its signed listings file,
  and holds an offer or a reply request for up to 25 seconds while it waits for the shop to
  open again.
- **History and proofs.** Item histories and ownership-proof checks run in the page, with the
  same tracer as `zsettle verify` and `zsettle check-proof`.
- **State files.** `hub.json` (the house shop's port, the addresses that got test coins) and
  the house's `app.json` are written to a new file and renamed into place. If one cannot be
  read, the server stops at start instead of starting empty.
- **Own shops only.** The page buys from shops of this server only, and the server passes
  requests to its own house shop only, so a visitor cannot make the server fetch or post to
  any other address.
- **House shop.** With `--house-passphrase-file`, a profile named `house` is created if
  missing, unlocked at start, and checked every 45 seconds: it keeps up to eight items listed
  from a small catalogue (0.0002 to 0.0015 ZEC) and mints a new batch when four or fewer are
  left. On the testnet it also pays 300,000 zatoshis of test coins once to each address that
  asks (`/api/wallet-faucet`). An item listed for over 2,000 blocks moves to a fresh note (one
  per round, only when a coin can pay the fee) and is listed again under its name, so a new
  buyer's first scan of the shop's item key stays short; offers signed for the old note fail.
  It is the only wallet the server holds.

Limits and protections of hosted mode are in
[Security model and limits](../security.md#hosted-mode).
