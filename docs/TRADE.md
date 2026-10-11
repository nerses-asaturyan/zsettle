# Running a trade

Two machines (or two wallet directories on one machine). Mainnet, Ironwood pool. Wallet setup is in [SETUP.md](SETUP.md); every command below takes `--server` to use another lightwalletd (default `https://zec.rocks:443`).

## Minting items

```
zsettle-seller mint --wallet ~/.zsettle/seller --item-account <ITEM_ACCOUNT_UUID> --item-account-index 1 \
  --payout-account <PAYOUT_ACCOUNT_UUID> --identity ~/.zsettle/seller/key.txt --name "Item #3" --value 1000
zsettle-seller sync --wallet ~/.zsettle/seller
zsettle-seller items --wallet ~/.zsettle/seller --item-account <ITEM_ACCOUNT_UUID> --payout-account <PAYOUT_ACCOUNT_UUID>
```

`mint` pays the item account from the payout account with the name in the memo and broadcasts; the resulting note is the item's genesis note. `items` lists the item account's unspent notes as `<txid>:<index>`, the reference `publish` takes.

## The market flow

| Step | Who | Command | Produces |
|---|---|---|---|
| 1 | seller | `zsettle-seller publish` | `listings.json`: item viewing key, payout address, items with prices |
| 2 | buyer | `zsettle list`, `zsettle import-listing`, `zsettle sync` | the item account imported as a view-only account and scanned |
| 3 | buyer | `zsettle buy --listings … --item …` | `trade.pczt` (private), `trade.seller.pczt` (goes to the seller's inbox) |
| 4 | seller | `zsettle-seller serve` | syncs, checks every inbox file, signs the item action, writes `*.signed.pczt` to the outbox, marks the item under offer |
| 5 | buyer | `zsettle finish --broadcast` | the settled transaction, sent through lightwalletd |
| 6 | seller service | (next pass) | sees the item note spent and removes it from the listings |

### 1. Seller publishes

```
zsettle-seller publish \
  --wallet ~/.zsettle/seller --item-account <ITEM_ACCOUNT_UUID> --item-account-index 1 \
  --payout-account <PAYOUT_ACCOUNT_UUID> \
  --listings listings.json --item-note <TXID>:<INDEX> --name "Item #3" --price 5000 --max-expiry-delta 10
```

The item account is a separate ZIP 32 account of the seller's wallet, so publishing its viewing key reveals nothing about the seller's other funds. Items must carry at least 1,000 zat; a zero-value note is not bound to the tree by the circuit. How the file reaches buyers is up to the seller: a web page, a message, a repository.

### 2. Buyer imports the listing

```
zsettle list --listings listings.json
zsettle import-listing --wallet ~/.zsettle/buyer --listings listings.json
zsettle sync --wallet ~/.zsettle/buyer
```

### 3. Buyer builds and proves

```
zsettle buy --wallet ~/.zsettle/buyer --buyer-account <BUYER_ACCOUNT_UUID> \
  --listings listings.json --item <TXID>:<INDEX> \
  --out trade.pczt --seller-copy inbox/trade.pczt
```

The item note and the buyer's largest spendable note are looked up in the buyer's wallet database, their witnesses are taken at one anchor, a two-spend Ironwood bundle is built (item note to the buyer with the item name as memo, price to the seller, change to the buyer) and proven with the two full viewing keys. The seller's copy keeps only what fixes the sighash plus the seller's own note data on the item action; the buyer's viewing key, witnesses, note randomness and randomizers are removed. Price, payout address and expiry come from the listing; `--seller`, `--price`, `--expiry-delta`, `--buyer-note` and `--item-account` override them. An item the listing marks as under offer is refused unless `--force` is given. `--receive-account` sends the item to another account of the buyer's wallet (see Resale).

### 4. Seller service signs

```
zsettle-seller serve \
  --wallet ~/.zsettle/seller --identity ~/.zsettle/seller/key.txt \
  --item-account <ITEM_ACCOUNT_UUID> --item-account-index 1 --payout-account <PAYOUT_ACCOUNT_UUID> \
  --listings listings.json --inbox inbox --outbox outbox
```

Every pass (default every 5 seconds):

1. **Sync.** The seller wallet is brought up to date with the built-in synchronizer. A pass whose sync fails signs nothing; it is logged and retried with exponential backoff (up to five minutes). `--sync-cmd` runs a shell command instead, `--no-sync` skips the step.
2. **Reconcile.** Each listed item is checked against the wallet: an item whose note is spent is removed (`sold` when it was under offer, `spent_elsewhere` otherwise); an item under offer whose expiry height has been scanned with the note still unspent is listed again (`relisted`).
3. **Inbox.** For every `*.pczt` that has not changed for two seconds, the service finds which listed item the trade spends, then checks: the item is not already under offer; expiry within the listing's limit (one block of slack, since the buyer measures from the next block); exactly one action spends that item note and it carries a randomizer; no other action spends any note of the seller's wallet; the outputs that decrypt under the seller's incoming viewing key add up to at least the price. The signer then verifies the nullifier and `rk` of the item action against the seller's key and signs that action only. The signed copy is written to the outbox, the item is marked under offer until the trade's expiry height, and the envelope moves to `inbox/done`. Refused envelopes move to `inbox/rejected` with a `.reason.txt`.

Every event is one JSON line on stdout (`start`, `signed`, `rejected`, `sold`, `relisted`, `spent_elsewhere`, `already_signed`, `error`). The service is safe to kill and restart at any point: the outbox file and the listings are written atomically, the envelope is moved last, and an envelope whose signed copy already exists is filed as done without signing again. `--once` runs a single pass and exits (non-zero if the pass failed); `zsettle-seller sign` does the same for one file, with `--dry-run` to check only.

### 5. Buyer finishes and broadcasts

```
zsettle finish --wallet ~/.zsettle/buyer --identity ~/.zsettle/buyer/key.txt --account-index 0 \
  --full trade.pczt --signed outbox/trade.signed.pczt --out trade.final.tx --broadcast
```

Combines the two PCZTs, signs the payment action, finalizes spends, extracts the transaction (proof and signatures are verified here) and sends it. `zsettle broadcast --tx trade.final.tx` resends a raw transaction.

## Resale

A buyer who may resell receives the item into a dedicated item account of its own wallet, so that publishing that account's viewing key later reveals nothing about the account that paid:

```
zsettle buy … --receive-account <BUYER_ITEM_ACCOUNT_UUID> …
```

After the trade is mined, the buyer is a seller: `zsettle-seller publish --wallet ~/.zsettle/buyer --item-account <BUYER_ITEM_ACCOUNT_UUID> --item-account-index 1 --payout-account <BUYER_ACCOUNT_UUID> --listings mine.json --item-note <TRADE_TXID>:<ITEM_ACTION_INDEX> …` (the item action index is printed by `buy`), then `zsettle-seller serve` with the same arguments. The next buyer proceeds exactly as above with `mine.json`.

## Verifying an item without any key

```
zsettle verify --listings listings.json --item <TXID>:<INDEX>
zsettle verify --listings listings.json --listings mine.json --item <TXID>:<INDEX>
```

Streams compact blocks from the earliest listing's birthday, trial-decrypts Ironwood actions with the listings' viewing keys to find the genesis note, derives its nullifier, and follows the spend: the action that spends the item is the trade, and that action's output commitment is the item's new note. When another supplied listing decrypts that note, the trace continues with it. Output is a JSON report with the item name (from the genesis memo), every hop (txid, height, commitment, holder) and a status: `held` by a named listing, or `transferred_to_unknown_holder` after a sale to someone whose listings were not supplied. Under a second per item on mainnet today.

## Mainnet trades so far

| Txid | Block | What moved |
|---|---|---|
| `e76531b9e51b8a7cbc88188c71fd2df63f8224b0afe1ef2564fb52df91966ae2` | 3514109 | item `71167c88…b2274c:1` to the buyer, 5,000 zat to the seller, fee 15,000 |
| `2e0f4af17eddeb80d3ca570dcb4ac5c29a454926e27c79cd0944d232a899167c` | 3514119 | item `da9e8f95…1927e7:1` to the buyer, 5,000 zat to the seller, signed by the seller service |
| `e70f48902faaf55bf3cda050f08a9ee8f3a1b02abb780c46ba3e8d6064618877` | 3514159 | item `6f1c8e26…bca26a:1` to the buyer's own item account (`--receive-account`), 5,000 zat to the seller; the service's built-in sync, the under-offer mark and the refusal of a second offer all ran here |
| `af09c2cf525d2bb6e553b61a1d3cc2674ebdf09726be1e3959ee36f5143f5804` | 3514174 | resale: the buyer listed the item from its item account, the original seller bought it back for 7,000 zat with an exact note (two actions, fee 10,000); the buyer's wallet ran `serve` and signed |

`zsettle verify --listings market/listings.json --listings market2/listings.json --item 6f1c8e26…:1` follows item #3 from its genesis (first seller's key) into the buyer's item account (second key) in under a second. It reports the resale as a transfer to an unknown holder: that transaction was built before the action-pairing rule was enforced (see [PROTOCOL.md](../spec/PROTOCOL.md), Items), so the new item note ended up in a different action from the item spend and the lineage, as the protocol defines it, stops there. Every trade built since pairs them.

## Notes

- Expiry defaults to 10 blocks. A longer expiry gives the buyer a free option on the seller's signature; after expiry the service relists the item by itself, and the seller can cancel earlier by spending the item note.
- Anyone holding the item viewing key learns when the item sells. That is inherent to consignment-style items.
- Trade files (`*.pczt`, `*.tx`) hold no secrets beyond what the holder already knows, but the buyer's full PCZT contains the buyer's full viewing key and witnesses: keep it private.
