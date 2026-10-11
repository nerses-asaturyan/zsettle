# zsettle

Atomic item-for-ZEC settlement on Zcash. One shielded transaction, two parties, no custodian.

## The problem

Zcash has no smart contracts. Today every trade of a Zcash-native collectible for ZEC needs a middleman: the buyer pays first and trusts the venue to deliver, or the venue holds the money. On the largest marketplace, ownership is a row in the operator's database.

## What zsettle does

An item is a shielded note. A trade is one Ironwood transaction with two actions:

- action A spends the seller's item note to a new item note owned by the buyer;
- action B spends the buyer's ZEC note to the seller at the listed price.

The buyer builds and proves the whole transaction holding only viewing keys. The seller signs action A only. The buyer signs action B only. Nothing is valid until both have signed. If the seller spends the item elsewhere first, the duplicate nullifier invalidates the whole trade and the buyer's ZEC never moves. Price is public. Buyer and seller stay shielded.

## Pieces

| Crate | Role |
|---|---|
| `zsettle-core` | library: items, listings, two-party bundle, PCZT roles, seller policy, keyless verifier, lightwalletd client |
| `zsettle` | buyer and verifier CLI: `list`, `import-listing`, `buy`, `finish`, `broadcast`, `verify` |
| `zsettle-seller` | seller service: `mint`, `items`, `publish`, `sign`, `serve` (inbox/outbox loop that signs only the item action) |

See [docs/SETUP.md](docs/SETUP.md) for wallets and [docs/TRADE.md](docs/TRADE.md) for the full flow, [spec/PROTOCOL.md](spec/PROTOCOL.md) and [spec/THREAT_MODEL.md](spec/THREAT_MODEL.md) for the design.

## What is private, what is real, what is a mock

| | Status |
|---|---|
| Buyer and seller identities | private (shielded Ironwood notes) |
| Trade amounts | private; only the listed price is public, by choice |
| Settlement | real: mainnet transactions `e76531b9…6ae2` (block 3514109) and `2e0f4af1…9167c` (block 3514119) |
| Items | real shielded notes minted by this tool; no claim about any third-party collection |
| Provenance | real: `zsettle verify` traces an item through mainnet compact blocks with the listing's viewing key only |
| Mock | nothing |

## Status

- Two-party trade construction, proving, redaction, seller policy, signing, extraction and broadcast: working on mainnet.
- Listings, seller service loop, minting, keyless verification: working on mainnet.
- Tests: the proven settlement core as an integration test, six seller-policy tests on synthetic trades.

## License

MIT or Apache-2.0, at your option.
