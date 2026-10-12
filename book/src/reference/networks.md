# Networks and the NU7 testnet branch

## Two branches

| Branch | Network | Crates | Default light server |
|---|---|---|---|
| `main` | mainnet, NU6.3 (Ironwood pool) | published librustzcash releases (`zcash_client_backend` 0.24, `pczt` 0.9, `orchard` 0.15, ...) | `https://zec.rocks:443` for every command |
| `nu7-testnet` | testnet, NU7 | pre-release crates (`zcash_client_backend` 0.25.0-pre.1 and its exact companions) | follows the wallet's network: `https://testnet.zec.rocks:443` on testnet, `https://zec.rocks:443` on mainnet |

The testnet moved to NU7 on 4 October 2026 and needs the pre-release crates. That is why
everything on `main` runs on mainnet with small amounts, and the hosted demo runs a
`nu7-testnet` build.

Release archives are built from `main`. Build the `nu7-testnet` branch from source for the
testnet:

```
git clone -b nu7-testnet https://github.com/nerses-asaturyan/zsettle
cd zsettle
cargo build --release --locked
```

## What differs on nu7-testnet

From that branch's `docs/TESTNET.md`:

- the dependencies (the 0.25 pre-release line, `rand` 0.10, `pasta_curves` 0.6) and the RNG
  plumbing they need;
- the wallet database schema is migrated when a wallet is opened; an `upgrade` command on both
  binaries applies migrations that need the seed (`upgrade --wallet <dir> --identity <key>`);
- `--server` has no fixed default: it follows the wallet's network;
- a `tx` command on the buyer CLI asks the light server whether a transaction is mined, in the
  mempool, or unknown (`zsettle tx --network test --txid <id>`).

**Do not open a mainnet wallet with a `nu7-testnet` build.** It migrates the database to the
newer schema, which the `main` build cannot read. Keep testnet wallets in their own
directories.

On testnet, blocks come about every 25 seconds since NU7, so a 40-block expiry is about 17
minutes. Testnet coins (TAZ) are free and worthless.

## The consensus-branch guard

The consensus branch id is part of the signature hash. Before building, minting or
broadcasting, the tools ask the light server which branch is live and compare it with the
branch this build signs for. When they differ, the tools stop with a message instead of making
a transaction the network would reject. A `main` build pointed at the testnet stops here.

`zsettle-app` checks earlier in hosted mode: a `main` build (where
`zsettle_core::broadcast::TESTNET_BUILD` is `false`) refuses to start with `--network test`,
which is the hosted default, and says to build the hosted testnet demo from `nu7-testnet`.

## Mainnet and NU7

NU7 is expected on mainnet in early November 2026. The `main` build signs for NU6.3. Once a
librustzcash release carries the NU7 mainnet activation height, the plan in the testnet notes
is that `nu7-testnet` becomes `main` by changing the pre-release versions to the release.
