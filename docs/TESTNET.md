# Testnet (NU7) build

This branch builds against the NU7 pre-release crate line (`zcash_client_backend` 0.25.0-pre.1 and its exact companions), so zsettle runs on the Zcash testnet, which activated NU7 at height 4,465,026 on 4 October 2026. `main` serves mainnet (NU6.3) with the released crates. The two branches differ only in dependencies, the RNG plumbing those crates require, a schema migration when a wallet is opened, and the default light server, which follows the wallet's network (`https://testnet.zec.rocks:443` for testnet, `https://zec.rocks:443` for mainnet).

Do not open a mainnet wallet with this build: it migrates the wallet database to the newer schema, which the `main` build cannot read. Keep testnet wallets in their own directory.

## Wallets

```
D=~/code/tools/zcash-devtool/target/release/zcash-devtool
$D wallet -w ~/.zsettle-testnet/seller init --name seller -i ~/.zsettle-testnet/seller/key.txt -n test -s zecrocks
$D wallet -w ~/.zsettle-testnet/seller generate-account --name item -i ~/.zsettle-testnet/seller/key.txt -s zecrocks
$D wallet -w ~/.zsettle-testnet/buyer  init --name buyer  -i ~/.zsettle-testnet/buyer/key.txt  -n test -s zecrocks
$D wallet -w ~/.zsettle-testnet/buyer  generate-account --name item -i ~/.zsettle-testnet/buyer/key.txt -s zecrocks
zsettle sync --wallet ~/.zsettle-testnet/buyer
zsettle-seller sync --wallet ~/.zsettle-testnet/seller
```

Devtool only creates the wallet files, which needs no NU7 support. The first `sync` with this build migrates the database and scans from the birthday; a migration that needs the seed is applied with `zsettle upgrade --wallet <dir> --identity <key>`.

## Funds

Testnet coins (TAZ) are free and worthless. The faucet at fauzec.com sends 1 TAZ per address per day to a `utest1…` unified address; take the buyer account's default address from `$D wallet -w ~/.zsettle-testnet/buyer list-addresses`. One TAZ is 100,000,000 zat: thousands of trades at test prices.

## Flow

The same as [TRADE.md](TRADE.md) with the testnet wallet directories. Every command picks `testnet.zec.rocks` from the wallet's network; `--server` still overrides it. Blocks arrive every 25 seconds on testnet since NU7, so a 40-block expiry is about 17 minutes and three confirmations about 75 seconds.

## What changed against `main`

- Dependencies: the 0.25-pre line, `rand` 0.10 (`rand::rng()` wherever a role needs randomness), `pasta_curves` 0.6.
- `Wallet::db` runs the wallet schema migrations; new `upgrade` command on both binaries.
- `pick_server`: the default light server follows the wallet's network.
- `COMPACT_NOTE_SIZE` moved to `orchard::note_encryption`; `UnifiedAddress::from_receivers` and the unified key encoder took new parameters; the signer, prover, IO finalizer and transaction extractor take an RNG.

When a librustzcash release carries the NU7 mainnet activation height, this branch becomes `main` by changing the `-pre` versions to the release.
