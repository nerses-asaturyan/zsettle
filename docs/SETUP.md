# Setup

Mainnet, Ironwood pool, published crates, with dust amounts. The same build runs on the testnet; see [TESTNET.md](TESTNET.md).

## Build

```
cargo build --release
```

Binaries: `target/release/zsettle` (buyer and verifier), `target/release/zsettle-seller` (seller service) and `target/release/zsettle-app` (the app: a web page over the same tools, locally or on a server). First build downloads the Sapling parameters for the bundled prover; the Orchard circuit keys are built at run time.

## Wallets

```
zsettle-seller init --wallet ~/.zsettle/seller --identity ~/.zsettle/seller.key
zsettle        init --wallet ~/.zsettle/buyer  --identity ~/.zsettle/buyer.key
```

Each command makes a 24-word mnemonic, encrypts it to a new age identity written to the `--identity` path, creates the database, and derives two accounts: index 0 pays and receives payments (`payout` in the seller wallet, `buyer` in the buyer wallet), index 1 holds items (`item`). Items live in their own ZIP 32 account so that publishing its viewing key reveals nothing about the rest of the wallet. The output lists the account UUIDs the other commands take and a fresh address to fund. Keep the key file, and keep it apart from the wallet directory and its backups: it is the only way to decrypt the mnemonic, and the mnemonic is the only backup of the funds.

```
zsettle accounts --wallet ~/.zsettle/buyer
zsettle address  --wallet ~/.zsettle/buyer --account <UUID>
```

Prefer a passphrase to a key file? Omit `--identity`: `init` asks for a passphrase twice and protects the mnemonic with it (age's scrypt recipient), and every later command that needs the mnemonic asks for it, or reads `ZSETTLE_PASSPHRASE` for scripts. There is then no file to lose, only a passphrase to remember. zcash-devtool cannot open passphrase-protected wallets.

`accounts` lists the accounts again; `address` prints a fresh Orchard-only receiving address (a new one each call). `zsettle-seller` has the same two commands, and `new-item-account` adds another item account when an item should have a consignment of its own.

The wallet layout is the one of [zcash-devtool](https://github.com/zcash/zcash-devtool) (`data.sqlite`, `keys.toml` with an age-encrypted mnemonic), so devtool can open these wallets too, and wallets made with devtool work here. The default server of every command is the zec.rocks server of the wallet's network (`https://zec.rocks:443` on mainnet, `https://testnet.zec.rocks:443` on the testnet); `--server` overrides it. The first connection to it occasionally stalls; a retry connects in under a second.

The README uses the short names `S`, `B` and `~/seller.key`; this document keeps everything under `~/.zsettle`. The rule is the same: the key file lives outside the wallet directory.

## Syncing

```
zsettle sync --wallet ~/.zsettle/buyer
zsettle-seller sync --wallet ~/.zsettle/seller
```

This is the `zcash_client_backend` synchronizer with an in-memory block cache: it refreshes the commitment-tree roots and the chain tip, downloads the block ranges the wallet still needs in batches of 500, scans them and drops them. Nothing besides the wallet database is written. The seller service runs the same sync before every pass, so a seller never signs against a stale view of the chain.

## Funding

Send ZEC to the address `init` printed, or to a fresh one from `zsettle address`. A note received from another wallet becomes spendable after ten confirmations (about 13 minutes on mainnet before NU7, about 4 minutes from NU7); the wallet's own change after three. `mint` and `buy` say so when asked too early. `init` writes `env.sh` (and `env.ps1` for PowerShell) into the wallet directory with `ZSETTLE_WALLET`, `ZSETTLE_IDENTITY`, the account UUIDs and, for a seller, `ZSETTLE_LISTINGS`; after `source <wallet>/env.sh` the commands need none of those flags. Amounts can be given in ZEC (`--amount 0.001`) or zatoshis. A trade costs the price plus the ZIP 317 fee: 15,000 zat with a change output, 10,000 when the paying note is exact. A mint pays 5,000 zat per action, the actions being the larger of the number of inputs and the number of outputs (the items plus change), at least two: two items from one note cost 15,000 zat plus the item values. Prices must be above 5,000 zat, since a smaller payment is dust that wallets refuse to spend. Everything in [TRADE.md](TRADE.md) was funded with 161,000 zat.

## Taking money out

```
zsettle-seller pay --to <unified address> --amount 0.001      # from the payout account, with env.sh sourced
zsettle pay --wallet ~/.zsettle/buyer --account <BUYER_ACCOUNT_UUID> --identity ~/.zsettle/buyer.key --to <unified address> --amount 0.001
```

Sends from an account of a wallet to any address, a phone wallet's included (`--memo` optional); without the environment, give `--wallet`, `--account` and `--identity`. The same command funds a counterparty's wallet.

## Files the tools write

- `<wallet>/reserved-notes.json`: notes committed to unsettled trades, with their expiry heights; `buy` skips them, also after they expire (the seller saw the note's nullifier), until `rotate` retires or releases them.
- The buyer's full PCZT and the final PCZT written by `finish`: contain the buyer's viewing key and witnesses, keep them private.
- The seller's inbox (`done/`, `rejected/` with reason files) and outbox (`<name>.<tag>.signed.pczt`).

## Privacy of the light server connection

Every sync, broadcast and `verify` talks to a lightwalletd server over TLS. The server learns the IP behind each call, the block ranges a wallet scans, the listing birthday an imported key starts at, the genesis transaction `verify` asks for (not with `--private`), and the transactions sent. It never receives the wallet's transparent addresses: the wallet library would send them on every sync, and zsettle answers that request inside the process. Use Tor or your own lightwalletd if the rest matters to you.

## Network upgrade

NU7 activates on mainnet at block 3,543,405 (about 5 November 2026). This build is on the released NU7 crates and signs for whichever branch is live at the next block: NU6.3 before the activation, NU7 from it. Before building, minting or broadcasting, the tools still compare the server's consensus branch with their own and stop with a message if they differ.
