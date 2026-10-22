# What zsettle is

## The problem

Zcash has no smart contracts. So when someone trades a Zcash-native collectible for ZEC
today, a middleman is needed. Either the buyer pays first and trusts the venue to deliver,
or the venue holds the money for a while. On many marketplaces, who owns an item is just a
row in the operator's database.

## The idea

zsettle puts both sides of a trade into one shielded transaction:

- the item moves from the seller to the buyer, and
- the price moves from the buyer to the seller.

Zcash consensus accepts a transaction only when every part of it is valid and signed. So the
item cannot move without the payment, and the payment cannot move without the item. There is
no escrow and no custodian.

An item is a real shielded note in the seller's wallet, not a database entry. Anyone who has
the seller's published viewing key can follow the item's history on the chain.

## One trade in plain words

1. **The seller lists the item.** The seller publishes a small signed file, the *listings*.
   It holds a viewing key for the seller's item account, a payout address, and each item with
   its price.
2. **The buyer builds the whole trade.** The buyer's tool imports the viewing key, finds the
   item note, and builds one transaction with both halves: the item to the buyer and the
   price to the seller. It also makes the zero-knowledge proof. Building and proving need
   only viewing keys.
3. **The seller checks and signs its half.** The buyer sends a copy with its own secrets
   removed. The seller's service checks the price, the expiry and the proof, and signs only
   the half that spends the item.
4. **The buyer signs and broadcasts.** The buyer adds the seller's signature, signs the
   payment half, and sends the transaction to the network.

If the seller spends the item somewhere else first, the trade becomes invalid (the item's
nullifier is already used) and the buyer's ZEC never moves. If the buyer never broadcasts,
the seller's signature expires after a few blocks.

## What is public and what is not

The listed price is public, because the seller publishes it. Buyer and seller addresses and
balances stay shielded. [Privacy](privacy.md) has the full picture, including what a holder
of the listings file can learn.

## The pieces

| Program | Role |
|---|---|
| `zsettle` | buyer and verifier command line |
| `zsettle-seller` | seller command line, and the seller service (`serve`) that signs offers |
| `zsettle-app` | a web page: wallet, market, buying and selling; in local mode over the two programs, in hosted mode with the wallet running in the browser (`zsettle-web`) |
| `zsettle-core` | the library under all three |

See [Architecture](how/architecture.md) for how they fit together.
