# Setup

Mainnet, Ironwood pool, published crates. Testnet has been on NU7 since 2026-10-04 and needs pre-release crates, so everything here runs on mainnet with dust amounts.

## Build

```
cargo build --release
```

Binaries: `target/release/zsettle` (buyer and verifier) and `target/release/zsettle-seller` (seller service). First build downloads the Sapling parameters for the bundled prover; the Orchard circuit keys are built at run time.

## Wallets

zsettle reads and writes the wallet layout of [zcash-devtool](https://github.com/zcash/zcash-devtool): `data.sqlite`, and `keys.toml` with the network, birthday and an age-encrypted mnemonic. Creating wallets and accounts is the one step that still uses devtool; syncing, minting, trading and verifying are zsettle commands.

```
git clone https://github.com/zcash/zcash-devtool ~/code/tools/zcash-devtool
cd ~/code/tools/zcash-devtool && cargo build --release
D=~/code/tools/zcash-devtool/target/release/zcash-devtool

$D wallet -w ~/.zsettle/seller init --name seller -i ~/.zsettle/seller/key.txt -n main -s zecrocks
$D wallet -w ~/.zsettle/seller generate-account --name item -i ~/.zsettle/seller/key.txt -s zecrocks
$D wallet -w ~/.zsettle/buyer  init --name buyer  -i ~/.zsettle/buyer/key.txt  -n main -s zecrocks
$D wallet -w ~/.zsettle/buyer  generate-account --name item -i ~/.zsettle/buyer/key.txt -s zecrocks
```

Each wallet gets two accounts: index 0 pays and receives payments, index 1 holds items. Items live in their own ZIP 32 account so that publishing its viewing key reveals nothing about the rest of the wallet. The buyer's item account is only needed to resell.

`$D wallet -w <dir> list-accounts` prints the account UUIDs the commands take. `-s zecrocks` is `https://zec.rocks:443`, the default server of every zsettle command; `--server` overrides it. The first connection to it occasionally stalls; a retry connects in under a second.

## Syncing

```
zsettle sync --wallet ~/.zsettle/buyer
zsettle-seller sync --wallet ~/.zsettle/seller
```

This is the `zcash_client_backend` synchronizer with an in-memory block cache: it refreshes the commitment-tree roots and the chain tip, downloads the block ranges the wallet still needs in batches of 500, scans them and drops them. Nothing besides the wallet database is written. The seller service runs the same sync before every pass, so a seller never signs against a stale view of the chain.

## Funding

Send ZEC to the buyer account's unified address (`$D wallet -w ~/.zsettle/buyer list-addresses`). A trade costs the price plus the ZIP 317 fee: 15,000 zat with a change output, 10,000 when the paying note is exact. A mint costs 10,000 zat plus 5,000 for every input beyond the first. Prices must be above 5,000 zat, since a smaller payment is dust that wallets refuse to spend. Everything in [TRADE.md](TRADE.md) was funded with 161,000 zat.

## Files the tools write

- `<wallet>/reserved-notes.json`: notes committed to unsettled trades, with their expiry heights; `buy` skips them until they expire.
- The buyer's full PCZT and the final PCZT written by `finish`: contain the buyer's viewing key and witnesses, keep them private.
- The seller's inbox (`done/`, `rejected/` with reason files) and outbox (`<name>.<tag>.signed.pczt`).

## Privacy of the light server connection

Every sync, broadcast and `verify` talks to a lightwalletd server over TLS. The server learns the IP that syncs an imported key, sends a trade or fetches an item's genesis transaction. Use Tor or your own lightwalletd if that matters to you.

## Network upgrade

NU7 is scheduled for mainnet in early November 2026. This build signs for NU6.3; before building, minting or broadcasting, the tools compare the server's consensus branch with their own and stop with a message if they differ. Update the dependencies to the NU7 release line once it is published.
