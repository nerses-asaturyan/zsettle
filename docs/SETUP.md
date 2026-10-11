# Setup

Mainnet, Ironwood pool, published crates. Testnet has been on NU7 since 2026-10-04 and needs pre-release crates, so everything here runs on mainnet with dust amounts.

## Build

```
cargo build --release
```

Binaries: `target/release/zsettle` (buyer and verifier) and `target/release/zsettle-seller` (seller service). First build downloads the Sapling parameters for the bundled prover; the Orchard circuit keys are built at run time.

## Wallets

```
zsettle-seller init --wallet ~/.zsettle/seller --identity ~/.zsettle/seller/key.txt
zsettle        init --wallet ~/.zsettle/buyer  --identity ~/.zsettle/buyer/key.txt
```

Each command makes a 24-word mnemonic, encrypts it to a new age identity written to `key.txt`, creates the database, and derives two accounts: index 0 pays and receives payments (`payout` in the seller wallet, `buyer` in the buyer wallet), index 1 holds items (`item`). Items live in their own ZIP 32 account so that publishing its viewing key reveals nothing about the rest of the wallet. The output lists the account UUIDs the other commands take and a fresh address to fund. Keep `key.txt`: it is the only way to decrypt the mnemonic, and the mnemonic is the only backup of the funds.

```
zsettle accounts --wallet ~/.zsettle/buyer
zsettle address  --wallet ~/.zsettle/buyer --account <UUID>
```

Prefer a passphrase to a key file? Omit `--identity`: `init` asks for a passphrase twice and protects the mnemonic with it (age's scrypt recipient), and every later command that needs the mnemonic asks for it, or reads `ZSETTLE_PASSPHRASE` for scripts. There is then no file to lose, only a passphrase to remember. zcash-devtool cannot open passphrase-protected wallets.

`accounts` lists the accounts again; `address` prints a fresh Orchard-only receiving address (a new one each call). `zsettle-seller` has the same two commands, and `new-item-account` adds another item account when an item should have a consignment of its own.

The wallet layout is the one of [zcash-devtool](https://github.com/zcash/zcash-devtool) (`data.sqlite`, `keys.toml` with an age-encrypted mnemonic), so devtool can open these wallets too, and wallets made with devtool work here. The default server of every command is `https://zec.rocks:443`; `--server` overrides it. The first connection to it occasionally stalls; a retry connects in under a second.

## Syncing

```
zsettle sync --wallet ~/.zsettle/buyer
zsettle-seller sync --wallet ~/.zsettle/seller
```

This is the `zcash_client_backend` synchronizer with an in-memory block cache: it refreshes the commitment-tree roots and the chain tip, downloads the block ranges the wallet still needs in batches of 500, scans them and drops them. Nothing besides the wallet database is written. The seller service runs the same sync before every pass, so a seller never signs against a stale view of the chain.

## Funding

Send ZEC to the address `init` printed, or to a fresh one from `zsettle address`. A trade costs the price plus the ZIP 317 fee: 15,000 zat with a change output, 10,000 when the paying note is exact. A mint costs 10,000 zat plus 5,000 for every input beyond the first. Prices must be above 5,000 zat, since a smaller payment is dust that wallets refuse to spend. Everything in [TRADE.md](TRADE.md) was funded with 161,000 zat.

## Files the tools write

- `<wallet>/reserved-notes.json`: notes committed to unsettled trades, with their expiry heights; `buy` skips them until they expire.
- The buyer's full PCZT and the final PCZT written by `finish`: contain the buyer's viewing key and witnesses, keep them private.
- The seller's inbox (`done/`, `rejected/` with reason files) and outbox (`<name>.<tag>.signed.pczt`).

## Privacy of the light server connection

Every sync, broadcast and `verify` talks to a lightwalletd server over TLS. The server learns the IP behind each call, the block ranges a wallet scans, the listing birthday an imported key starts at, the genesis transaction `verify` asks for (not with `--private`), and the transactions sent. It never receives the wallet's transparent addresses: the wallet library would send them on every sync, and zsettle answers that request inside the process. Use Tor or your own lightwalletd if the rest matters to you.

## Network upgrade

NU7 is scheduled for mainnet in early November 2026. This build signs for NU6.3; before building, minting or broadcasting, the tools compare the server's consensus branch with their own and stop with a message if they differ. Update the dependencies to the NU7 release line once it is published.
