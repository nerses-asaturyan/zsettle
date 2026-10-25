# Project status and mainnet runs

## What works

- Two-party trade construction, proving, redaction, seller policy, proof verification,
  signing, extraction and broadcast: working on mainnet with the shipped code (trade
  `b378dfbc…ea9c`, offered over HTTP).
- Listings, the seller service loop, minting, the built-in wallet sync, and history tracing
  with viewing keys only: working on mainnet.
- Seller service: syncs before every pass, removes sold items by itself, signs every acceptable
  offer (nothing to lock, nothing to grief), survives restarts, hostile inboxes and server
  outages.
- Resale: items can be received into a dedicated item account and listed again; history
  tracing follows them across holders.
- Light server: the wallet's transparent addresses are never sent to it (the library's UTXO
  refresh is answered inside the process).
- Network upgrades: build, mint and broadcast stop with a message when the server's consensus
  branch is not the one this build signs for.
- Wallets are created by the tools themselves (`init`), in the zcash-devtool layout. No other
  software is needed. `pay` sends ZEC out.
- Binaries: every `v*` tag publishes a GitHub Release with archives for Linux, macOS (arm64 and
  x86_64) and Windows, holding the binaries, the docs and the examples. The same workflow runs
  on demand from Actions.
- Tests (`crates/zsettle-core/tests/` and unit tests): the proven settlement core, action
  pairing, redaction and proof checks, the seller-policy cases, the rotation transfer, listings
  signatures.
- The app (`zsettle-app`) in local mode, and hosted mode for the public demo (browser wallets
  on the testnet or mainnet; the house shop and free coins on the testnet).

The code was reviewed for privacy and soundness in several passes. The findings and what was
done about each are in `CHANGELOG.md`. What remains by design is in `spec/THREAT_MODEL.md` and
on the [Privacy](../privacy.md) page.

Nothing in zsettle is a mock. Items are real shielded notes minted by these tools; zsettle
makes no claim about any third-party collection.

## Mainnet trades

| Txid | Block | What moved |
|---|---|---|
| `e76531b9e51b8a7cbc88188c71fd2df63f8224b0afe1ef2564fb52df91966ae2` | 3514109 | item `71167c88…b2274c:1` to the buyer, 5,000 zat to the seller, fee 15,000 |
| `2e0f4af17eddeb80d3ca570dcb4ac5c29a454926e27c79cd0944d232a899167c` | 3514119 | item `da9e8f95…1927e7:1` to the buyer, 5,000 zat to the seller, signed by the seller service |
| `e70f48902faaf55bf3cda050f08a9ee8f3a1b02abb780c46ba3e8d6064618877` | 3514159 | item `6f1c8e26…bca26a:1` to the buyer's own item account (`--receive-account`), 5,000 zat to the seller |
| `af09c2cf525d2bb6e553b61a1d3cc2674ebdf09726be1e3959ee36f5143f5804` | 3514174 | resale: the buyer listed the item from its item account, the original seller bought it back for 7,000 zat with an exact note (two actions, fee 10,000); the buyer's wallet ran `serve` and signed |
| `70b4a0e550ced3ddc6039248ed860ae383e111e70d7ee01d2ba6b06a50e2ffb3` | 3514587 | mint on consignment with the final code: the buyer wallet paid a 17,999-zat note into the seller's item account through the imported viewing key (`zsettle-seller mint --item-account <item-view> --value 17999`), exactly its two notes minus the fee |
| `b378dfbc39c7ad454e0c1eab0b95114b2b1ff5a995dc35f54e728f88687bea9c` | 3514597 | final-code trade over HTTP: the seller wallet bought item #3 (`c1c554f2…06f6:2`) from the buyer wallet's listing, paying 7,999 zat with an exact two-action payment (fee 10,000); `serve --listen`, `buy --seller-url`, `finish --signed <reply url> --broadcast`. The item output went to a fresh address under the buyer's outgoing key, the trade has the default expiry and fee, and `verify` with both consignments shows the whole lineage from the genesis mint through three holders |

The two listings files are in `examples/` (`market.json`, `market2.json`). These commands need
no wallet and no funds:

```
zsettle verify --listings examples/market.json --listings examples/market2.json --item af09c2cf525d2bb6e553b61a1d3cc2674ebdf09726be1e3959ee36f5143f5804:0
zsettle verify --listings examples/market.json --listings examples/market2.json --item 6f1c8e264cf5a89b24c4c20337706bfa76a480eeae0c5117a3667f8987bca26a:1
```

The second follows item #3 from its genesis (first seller's key) into the buyer's item account
(second key). It reports the resale as a transfer to an unknown holder: that transaction was
built before the action-pairing rule was enforced, so the new item note ended up in a
different action from the item spend, and the lineage, as the protocol defines it, stops there.

The four earlier trades came before the privacy review. Their item outputs were encrypted to
the seller's published outgoing key and sent to the buyer's default address, and their expiry
was ten blocks. The Orchard key in `examples/market.json` is that seller item account's key, so
anyone with the file can read those four item notes and the demo buyer's default address. Both
wallets are throwaway demo wallets. Trades built since do none of this.

## Testnet run

`docs/TESTNET.md` records a testnet run on 11 October 2026: a
payment, two mints, a trade signed by the seller service, and a resale, all from one faucet
claim.
