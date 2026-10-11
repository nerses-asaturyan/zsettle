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
| `zsettle-core` | library: items, two-party bundle, PCZT roles, seller policy, keyless verifier |
| `zsettle` | buyer and verifier CLI: `wallet`, `mint`, `list`, `buy`, `finish`, `verify` |
| `zsettle-seller` | seller service: validates envelopes against policy, signs the item action only |

## What is private, what is real, what is a mock

| | Status |
|---|---|
| Buyer and seller identities | private (shielded Ironwood notes) |
| Trade amounts | private; only the listed price is public, by choice |
| Settlement | real: mainnet transactions (txids listed in `demo/` once available) |
| Items | real shielded notes minted by this tool; no claim about any third-party collection |
| Mock | nothing |

## Status

Workspace and the proven settlement core as an integration test (`crates/zsettle-core/tests/two_party_settlement.rs`). See `spec/PROTOCOL.md` for the trade flow and `spec/THREAT_MODEL.md` for what each party can and cannot do.

## License

MIT or Apache-2.0, at your option.
