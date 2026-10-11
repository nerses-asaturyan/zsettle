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
| `zsettle` | buyer and verifier CLI: `init`, `accounts`, `address`, `list`, `sync`, `import-listing`, `buy`, `finish`, `broadcast`, `rotate`, `consolidate`, `notes`, `verify` |
| `zsettle-seller` | seller service: `init`, `accounts`, `address`, `sync`, `new-item-account`, `mint`, `items`, `publish`, `cancel`, `sign`, `serve` (syncs, reconciles listings with the chain, verifies proofs, signs only the item action) |

## One minute, no wallet

```
zsettle list   --listings examples/market2.json
zsettle verify --listings examples/market.json --listings examples/market2.json --item af09c2cf525d2bb6e553b61a1d3cc2674ebdf09726be1e3959ee36f5143f5804:0
```

The two signed listings files from the mainnet trades are in `examples/`; the trace reads mainnet compact blocks and needs nothing else.

## Ten minutes, two terminals

```
# once
zsettle-seller init --wallet S --identity S/key.txt     # prints P (payout) and I (item) and an address to fund
zsettle init        --wallet B --identity B/key.txt     # prints A (buyer) and the item account, and an address to fund
#   (omit --identity on either to protect the mnemonic with a passphrase instead of a key file)
# seller
zsettle-seller mint    --wallet S --item-account I --payout-account P --identity S/key.txt --name "Item #1"
zsettle-seller publish --wallet S --item-account I --payout-account P --identity S/key.txt --listings listings.json --item-note <txid>:<idx> --price 100000
zsettle-seller serve   --wallet S --item-account I --payout-account P --identity S/key.txt --listings listings.json --inbox inbox --outbox outbox   # add --listen 0.0.0.0:8900 to take offers over HTTP
# buyer
zsettle import-listing --wallet B --listings listings.json
zsettle sync           --wallet B
zsettle buy            --wallet B --buyer-account A --listings listings.json --item <txid>:<idx> --out trade.pczt --seller-copy inbox/trade.pczt
zsettle finish         --wallet B --identity B/key.txt --full trade.pczt --signed outbox/trade.<tag>.signed.pczt --out trade.tx --broadcast   # or --signed http://seller:8900/reply/<tag> after buy --seller-url
# anyone
zsettle verify --listings listings.json --item <txid>:<idx>
```

`S`, `B` are wallet directories, `I`, `P`, `A` the account UUIDs `init` prints (`accounts` lists them again). No other tool is needed. Commands that act print one JSON line; `list`, `notes` and `items` print tables; `zsettle notes --wallet B --account A` shows what a wallet holds.

See [docs/SETUP.md](docs/SETUP.md) for wallets, [docs/TRADE.md](docs/TRADE.md) for the full flow including resale, [spec/PROTOCOL.md](spec/PROTOCOL.md) and [spec/THREAT_MODEL.md](spec/THREAT_MODEL.md) for the design.

## What is private, what is real, what is a mock

| | Status |
|---|---|
| Buyer and seller identities | private: shielded Ironwood notes, fresh diversified addresses for every item and for the market payout |
| Trade amounts | private; only the listed price is public, by choice. The seller's copy of a trade carries no value trapdoors, so the seller cannot solve for the buyer's values |
| On chain | a trade is a two- or three-action Ironwood transaction with the ecosystem's default expiry and fee; unmarked to anyone without the listings file. Whoever holds that file can derive the listed items' nullifiers and so spot every trade of that account |
| What a listings holder learns | the item notes of the listed account and when each is spent, plus the height the file was last signed at; no trade output is encrypted to the published key; one account per item limits this to one item |
| Settlement | real: four mainnet trades, `e76531b9…6ae2` (block 3514109), `2e0f4af1…9167c` (3514119), `e70f4890…8877` (3514159) and a resale `af09c2cf…5804` (3514174), made before the privacy pass (see docs/TRADE.md) |
| Items | real shielded notes minted by this tool; no claim about any third-party collection |
| Provenance | real: `zsettle verify` traces an item through mainnet compact blocks with the listings' viewing keys only, across resales |
| Listings | signed by the item account's spend key; buyers verify against the published viewing key |
| Mock | nothing |

## Status

- Two-party trade construction, proving, redaction, seller policy, proof verification, signing, extraction and broadcast: working on mainnet.
- Listings, seller service loop, minting, built-in wallet sync, provenance verification with viewing keys only: working on mainnet.
- Seller service: syncs before every pass, removes sold items by itself, signs every acceptable offer (nothing to lock, nothing to grief), survives restarts, hostile inboxes and server outages.
- Resale: items can be received into a dedicated item account and relisted; provenance follows them across holders.
- Light server: the wallet's transparent addresses are never sent to it (the library's UTXO refresh is answered inside the process); what it does learn is in the threat model.
- Network upgrades: build, mint and broadcast stop with a message when the server's consensus branch is not the one this build signs for.
- Wallets: created by the tools themselves (`init`), in the zcash-devtool layout; no other software is needed.
- Tests: the proven settlement core, action pairing, redaction and proof checks, twelve seller-policy cases, the rotation transfer, listings signatures.
- Reviewed for privacy and soundness in several independent passes; every finding is fixed or documented.

## License

MIT or Apache-2.0, at your option.
