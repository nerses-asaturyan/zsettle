# Development setup

Mainnet, Ironwood pool, published crates. Testnet is on NU7 since 2026-10-04 and needs pre-release crates, so we develop on mainnet with dust.

## Tooling

```
git clone https://github.com/zcash/zcash-devtool ~/code/tools/zcash-devtool
cd ~/code/tools/zcash-devtool && cargo build --release
```

Pinned by devtool at this commit: zcash_client_backend 0.24, zcash_client_sqlite 0.22, pczt 0.9.1, orchard 0.15, zcash_primitives 0.30. The zsettle crates stay on the same line.

Light server: `zec.rocks:443` (built into devtool as `-s zecrocks`). Use `--connection direct`; it is the default. The first connection attempt can stall; a retry connects in under a second.

## Wallets

Two wallets outside the repo, in `~/.zsettle/`:

```
D=~/code/tools/zcash-devtool/target/release/zcash-devtool
$D wallet -w ~/.zsettle/buyer  init --name buyer  -i ~/.zsettle/buyer/key.txt  -n main -s zecrocks
$D wallet -w ~/.zsettle/seller init --name seller -i ~/.zsettle/seller/key.txt -n main -s zecrocks
$D wallet -w ~/.zsettle/seller generate-account --name item -i ~/.zsettle/seller/key.txt
```

The seller wallet has two accounts: `seller` (index 0, payout) and `item` (index 1, holds item notes). Item notes are minted by sending from account 0 to the item account's address.

Hand the item account's viewing key to the buyer (this is the "consignment" step of the protocol):

```
$D wallet -w ~/.zsettle/seller list-accounts        # copy the UFVK of the `item` account
$D wallet -w ~/.zsettle/buyer  import-ufvk --name item-view <UFVK> <birthday>
```

Sync and inspect:

```
$D wallet -w ~/.zsettle/buyer  sync -s zecrocks
$D wallet -w ~/.zsettle/seller sync -s zecrocks
$D wallet -w ~/.zsettle/buyer  balance
$D wallet -w ~/.zsettle/buyer  list-addresses [ACCOUNT_UUID]
```

## Devtool pieces zsettle builds on

- `pczt sign` signs every action whose key lives in the local wallet. A seller wallet that holds only the item account signs only the item action; `zsettle-seller` adds the policy checks in front of it.
- `pczt redact -k <key>` strips fields before the hand-off.
- `pczt extract` runs the spend finalizer and transaction extractor.
- `pczt create` is single-account; the two-account trade bundle is zsettle's own code.
