# zsettle-web: the wallet in the page

`zsettle-core` compiled to WebAssembly, and the engine that runs it in the app's page. On a
hosted server (`zsettle-app --public`) the app at `/` is this wallet, running in the
visitor's browser:

- The recovery phrase is made in the page and stays there, encrypted with AES-GCM under a key
  from a passkey (its PRF output) or a passphrase (PBKDF2, 600,000 rounds). The wallet
  database (notes and history) is kept in the browser's IndexedDB, encrypted with the same
  key. One wallet, with the same words and lock on both networks; each network keeps its own
  copy of the database.
- Proving, signing and note scanning run in a Web Worker, with a thread pool for the proofs.
- Selling runs in the page too (`src/seller.rs`): minting, listing, delisting, cancelling, and
  a shop that checks each offer with the seller service's checks and signs its half while the
  page is open. The server keeps a mailbox for the shop; every request to it is signed with
  the item key.
- Item histories and ownership-proof checks run in the page (`src/history.rs`), with the same
  tracer as `zsettle verify` and `zsettle check-proof`.
- The server relays public chain data from lightwalletd at `/api/chain/` (the chain tip,
  compact blocks, tree states, subtree roots, transactions, server info, and finished
  transactions to send) and passes offers to shops at `/shop/`. It holds no visitor key.

`www/engine.js` answers the app's `/api/...` calls in the browser; `www/worker.js` runs the
WebAssembly module. A passkey wallet asks for the passkey again before each action that signs
and before showing the backup words.

## Build

Needs a nightly toolchain (pinned in `rust-toolchain.toml`, with `rust-src` for rebuilding
the standard library with atomics), `wasm-pack`, and `clang` for SQLite's C code.

```sh
crates/zsettle-web/build.sh
```

The result is `www/` with `www/pkg/` inside. Serve it with the app (hosted mode needs it):

```sh
zsettle-app --public https://wallet.example --web-wallet crates/zsettle-web/www
```

The app page is at `/`; it loads `/engine.js`, `/worker.js` and `/pkg/...`. The old `/wallet/`
address answers `301` to `/`. These answers carry `Cross-Origin-Opener-Policy: same-origin` and
`Cross-Origin-Embedder-Policy: require-corp`, which shared memory (and so the thread pool)
needs, and a content security policy that allows WebAssembly.

## Why a separate workspace

- Threads need `+atomics` and the standard library rebuilt with them, which is nightly only.
- SQLite cannot be compiled for `wasm32-unknown-unknown` the usual way. `shim/libsqlite3-sys`
  stands in for `libsqlite3-sys` 0.35 and takes SQLite from `sqlite-wasm-rs`.
- Sapling is never used: a stand-in prover fills the wallet library's Sapling slots
  (`zsettle-core` without its `native` feature).

None of this touches the native build of the other crates.

## Limits

- The first trade of a visit builds the proving key (seconds natively, longer on a phone);
  the page starts it in the background once a shop opened in the Market has been scanned, if
  the page is in front and the wallet is idle.
- A payment from the page (Send) is built by the wallet library, which builds its own proving
  key each time.
- The page buys from shops on the same server only: it talks to no other site.
- A shop run from the page sells only while the page is open.
