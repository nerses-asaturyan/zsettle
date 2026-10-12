# zsettle

Atomic item-for-ZEC settlement on Zcash. One shielded transaction, two parties, no custodian.

## The problem

Zcash has no smart contracts. Today every trade of a Zcash-native collectible for ZEC needs a middleman: the buyer pays first and trusts the venue to deliver, or the venue holds the money. On the largest marketplace, ownership is a row in the operator's database.

## What zsettle does

An item is a shielded note. A trade is one Ironwood transaction with two actions:

- action A spends the seller's item note to a new item note owned by the buyer;
- action B spends the buyer's ZEC note to the seller at the listed price.

The buyer builds and proves the whole transaction holding only viewing keys. The seller verifies the proof and signs action A only. The buyer signs action B only. Nothing is valid until both have signed. If the seller spends the item elsewhere first, the duplicate nullifier invalidates the whole trade and the buyer's ZEC never moves. Price is public. Buyer and seller stay shielded.

## Pieces

| Crate | Role |
|---|---|
| `zsettle-core` | library: items, signed listings, two-party bundle, PCZT roles, seller policy, proof check, wallet sync, provenance verifier, lightwalletd client |
| `zsettle` | buyer and verifier CLI: `init`, `accounts`, `address`, `pay`, `list`, `sync`, `import-listing`, `buy`, `finish`, `broadcast`, `rotate`, `consolidate`, `notes`, `verify` |
| `zsettle-app` | local web page over the two binaries: passkey wallet, market, buy and sell |
| `zsettle-seller` | seller service: `init`, `accounts`, `address`, `pay`, `sync`, `new-item-account`, `mint`, `items`, `publish`, `cancel`, `sign`, `serve` (syncs, reconciles listings with the chain, verifies proofs, signs only the item action) |

## Install

Download the archive for your system from the [releases page](https://github.com/nerses-asaturyan/zsettle/releases): Linux x86_64, macOS (Apple silicon or Intel) or Windows x86_64. It holds the two binaries, this README, `docs/`, `spec/` and `examples/`, so the one-minute check below runs from it.

```
tar xzf zsettle-linux-x86_64.tar.gz    # Windows: unzip zsettle-windows-x86_64.zip
./zsettle --version
```

To build instead: Rust 1.93 and `cargo build --release --locked` ([docs/SETUP.md](docs/SETUP.md)).

## The app

`zsettle-app` is the same tool in a browser, on a phone or a desktop: a wallet unlocked by a passkey (or a passphrase), a market where you open a shop by its address, buying with a review step and live progress, a seller tab to mint items, set prices and open your shop, item histories from the chain, a QR code to receive, and backup words. Every transaction links to the ZecBlock explorer.

```
zsettle-app                              # prints a http://localhost:7070/#token=... link to open
zsettle-app --profile bob --port 7071    # a second profile, to play buyer and seller on one machine
```

It listens on this machine only, keeps the wallet in `~/.zsettle-app/<profile>`, and runs the `zsettle` and `zsettle-seller` binaries next to it for every action, so it adds no cryptography of its own. The passkey's PRF output is the wallet's passphrase: the browser derives it on each unlock and nothing stores it.

### Public demo

`zsettle-app --public https://<domain>` serves many people from one server on the testnet: each visitor makes a wallet of their own, gets free test coins once, buys from any open shop, and can open a shop of their own; a house shop keeps items in stock. [docs/HOSTING.md](docs/HOSTING.md) sets it up in one command. The demo server holds the demo wallets, so it is for test coins only.

## One minute, no wallet

```
zsettle list   --listings examples/market.json
zsettle verify --listings examples/market.json --listings examples/market2.json --item af09c2cf525d2bb6e553b61a1d3cc2674ebdf09726be1e3959ee36f5143f5804:0
```

The two signed listings files from the mainnet trades are in `examples/`; the trace reads mainnet compact blocks and needs nothing else.

## Ten minutes, two terminals

```
# once (keep the key files outside the wallet directories; omit --identity for a passphrase instead)
zsettle-seller init --wallet S --identity ~/seller.key     # prints the payout and item accounts and an address to fund; writes S/env.sh
zsettle init        --wallet B --identity ~/buyer.key      # prints the buyer and item accounts and an address to fund; writes B/env.sh
# fund both: the seller needs about 0.0005 ZEC (mint fees), the buyer the price plus 0.00015 ZEC per trade;
# a note received from another wallet is spendable after ten confirmations (about 13 minutes)
# seller terminal
source S/env.sh                                            # PowerShell: . S\env.ps1
zsettle-seller sync
zsettle-seller mint --name "Item #1" --name "Item #2"      # one transaction; prints each note as <txid>:<idx>
# wait for that block (about 75 seconds), then
zsettle-seller sync
zsettle-seller publish --item-note <txid>:<idx> --name "Item #1" --price 0.001   # writes S/listings.json; one call per item
zsettle-seller serve --inbox inbox --outbox outbox --listen 0.0.0.0:8900         # serves the file at http://seller:8900/listings
# buyer terminal
source B/env.sh
zsettle list           --listings http://seller:8900/listings                    # a copy of the file works too
zsettle import-listing --listings http://seller:8900/listings
zsettle sync
zsettle buy            --listings http://seller:8900/listings --item <txid>:<idx> --out trade.pczt --seller-url http://seller:8900
zsettle finish         --full trade.pczt --out trade.tx --broadcast              # polls the reply URL buy saved in trade.pczt.reply
# anyone
zsettle verify --listings http://seller:8900/listings --item <txid>:<idx>        # JSON; status.state is "held" while the holder's key opens the last note
```

Without a network between them, the parties share a folder instead: `buy --seller-copy inbox/trade.pczt` and `finish --signed outbox/trade.<tag>.signed.pczt`.

`S` and `B` are wallet directories. `init` prints the account UUIDs (`accounts` lists them again) and writes `env.sh` and `env.ps1` into the wallet directory; once sourced, no `--wallet`, `--identity`, account or `--listings` flag is needed, and every command above is written that way (`--help` shows each flag and the variable behind it). Every `--listings` takes a file or the URL a running service serves it at. No other tool is needed: `zsettle-seller pay` and `zsettle pay` send ZEC out to any address, a phone wallet included. Amounts are zatoshis or ZEC with a decimal point (`--price 0.001`); `list`, `notes` and `items` show both. Commands that act print one JSON line; `list`, `notes` and `items` print tables; `zsettle notes` shows what a wallet holds.

See [docs/SETUP.md](docs/SETUP.md) for wallets, [docs/TRADE.md](docs/TRADE.md) for the full flow including resale, [spec/PROTOCOL.md](spec/PROTOCOL.md) and [spec/THREAT_MODEL.md](spec/THREAT_MODEL.md) for the design.

## What is private, what is real, what is a mock

| | Status |
|---|---|
| Buyer and seller identities | private: shielded Ironwood notes, fresh diversified addresses for every item and for the market payout |
| Trade amounts | private; only the listed price is public, by choice. The seller's copy of a trade carries no value trapdoors, so the seller cannot solve for the buyer's values |
| On chain | a trade is a two- or three-action Ironwood transaction with the ecosystem's default expiry and fee; unmarked to anyone without the listings file. Whoever holds that file can derive the listed items' nullifiers and so spot every trade of that account |
| What a listings holder learns | the item notes of the listed account and when each is spent, plus the height the file was last signed at; no trade output is encrypted to the published key; one account per item limits this to one item |
| Settlement | real: five mainnet trades. The latest, `b378dfbc…ea9c` (block 3514597), ran the shipped code end to end: offer over HTTP, exact two-action payment, item to a fresh address under the buyer's outgoing key. The four earlier ones, `e76531b9…6ae2` (block 3514109), `2e0f4af1…9167c` (3514119), `e70f4890…8877` (3514159) and a resale `af09c2cf…5804` (3514174), predate the privacy pass (see docs/TRADE.md) |
| Items | real shielded notes minted by this tool; no claim about any third-party collection |
| Provenance | real: `zsettle verify` traces an item through mainnet compact blocks with the listings' viewing keys only, across resales |
| Listings | signed by the item account's spend key; buyers verify against the published viewing key |
| Mock | nothing |

## Status

- Two-party trade construction, proving, redaction, seller policy, proof verification, signing, extraction and broadcast: working on mainnet with the shipped code (trade `b378dfbc…ea9c`, offered over HTTP).
- Listings, seller service loop, minting, built-in wallet sync, provenance verification with viewing keys only: working on mainnet.
- Seller service: syncs before every pass, removes sold items by itself, signs every acceptable offer (nothing to lock, nothing to grief), survives restarts, hostile inboxes and server outages.
- Resale: items can be received into a dedicated item account and relisted; provenance follows them across holders.
- Light server: the wallet's transparent addresses are never sent to it (the library's UTXO refresh is answered inside the process); what it does learn is in the threat model.
- Network upgrades: build, mint and broadcast stop with a message when the server's consensus branch is not the one this build signs for.
- Wallets: created by the tools themselves (`init`), in the zcash-devtool layout; no other software is needed. `pay` sends ZEC out.
- Binaries: every `v*` tag publishes a GitHub Release with archives for Linux, macOS (arm64 and x86_64) and Windows holding the binaries, the docs and the examples; the same workflow runs on demand from Actions for testers.
- Tests: the proven settlement core, action pairing, redaction and proof checks, twelve seller-policy cases, the rotation transfer, listings signatures.
- Reviewed for privacy and soundness in several independent passes; the findings and what was done about each are in the changelog, and what remains by design is in the threat model.

## License

MIT or Apache-2.0, at your option.
