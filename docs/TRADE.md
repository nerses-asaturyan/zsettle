# Running a trade

Two machines (or two wallet directories on one machine). Mainnet, Ironwood pool.

## Minting items

```
zsettle-seller mint --wallet ~/.zsettle/seller --item-account <ITEM_ACCOUNT_UUID> --item-account-index 1   --payout-account <PAYOUT_ACCOUNT_UUID> --identity ~/.zsettle/seller/key.txt --name "Item #3" --value 1000
zsettle-seller items --wallet ~/.zsettle/seller --item-account <ITEM_ACCOUNT_UUID> --item-account-index 1 --payout-account <PAYOUT_ACCOUNT_UUID>
```

`mint` pays the item account from the payout account with the name in the memo and broadcasts; the resulting note is the item's genesis note. `items` lists the item account's unspent notes as `<txid>:<index>`, the reference used by `publish`.

## The market flow

| Step | Who | Command | Produces |
|---|---|---|---|
| 1 | seller | `zsettle-seller publish` | `listings.json`: item viewing key, payout address, items with prices |
| 2 | buyer | `zsettle list`, `zsettle import-listing` | the item account imported as a view-only account; then sync |
| 3 | buyer | `zsettle buy --listings … --item …` | `trade.pczt` (private), `trade.seller.pczt` (send to the seller's inbox) |
| 4 | seller | `zsettle-seller serve` | checks every inbox file, signs the item action, writes `*.signed.pczt` to the outbox, delists the item |
| 5 | buyer | `zsettle finish --broadcast` | the settled transaction, sent through lightwalletd |

### 1. Seller publishes

```
zsettle-seller publish \
  --wallet ~/.zsettle/seller --item-account <ITEM_ACCOUNT_UUID> --item-account-index 1 \
  --payout-account <PAYOUT_ACCOUNT_UUID> \
  --listings listings.json --item-note <TXID>:<INDEX> --name "Item #2" --price 5000 --max-expiry-delta 10
```

The item account is a separate ZIP 32 account of the seller's wallet, so publishing its viewing key reveals nothing about the seller's other funds. Items must carry at least 1,000 zat; a zero-value note is not bound to the tree by the circuit.

### 2. Buyer imports the listing

```
zsettle list --listings listings.json
zsettle import-listing --wallet ~/.zsettle/buyer --listings listings.json
zcash-devtool wallet -w ~/.zsettle/buyer sync -s zecrocks
```

### 3. Buyer builds and proves

```
zsettle buy --wallet ~/.zsettle/buyer --buyer-account <BUYER_ACCOUNT_UUID> \
  --listings listings.json --item <TXID>:<INDEX> \
  --out trade.pczt --seller-copy inbox/trade.pczt
```

The item note and the buyer's largest spendable note are looked up in the buyer's wallet database, their witnesses are taken at one anchor, a two-spend Ironwood bundle is built (item note to the buyer, price to the seller, change to the buyer) and proven with the two full viewing keys. The seller's copy keeps only what fixes the sighash plus the seller's own note data on the item action; the buyer's viewing key, witnesses, note randomness and randomizers are removed. Price, payout address and expiry come from the listing; `--seller`, `--price`, `--expiry-delta`, `--buyer-note` and `--item-account` override them.

### 4. Seller service signs

```
zsettle-seller serve \
  --wallet ~/.zsettle/seller --identity ~/.zsettle/seller/key.txt \
  --item-account <ITEM_ACCOUNT_UUID> --item-account-index 1 --payout-account <PAYOUT_ACCOUNT_UUID> \
  --listings listings.json --inbox inbox --outbox outbox \
  --sync-cmd "zcash-devtool wallet -w ~/.zsettle/seller sync -s zecrocks"
```

For every `*.pczt` in the inbox the service finds which listed item the trade spends, then checks: expiry within the listing's limit (one block of slack, since the buyer measures from the next block); exactly one action spends that item note and it carries a randomizer; no other action spends any note of the seller's wallet; the outputs that decrypt under the seller's incoming viewing key add up to at least the price. The signer then verifies the nullifier and `rk` of the item action against the seller's key and signs that action only. Accepted files move to `inbox/done`, refused ones to `inbox/rejected` with a reason file. `--once` processes the inbox and exits; `zsettle-seller sign` does the same for a single file, with `--dry-run` to check only.

### 5. Buyer finishes and broadcasts

```
zsettle finish --wallet ~/.zsettle/buyer --identity ~/.zsettle/buyer/key.txt --account-index 0 \
  --full trade.pczt --signed outbox/trade.signed.pczt --out trade.final.tx --broadcast
```

Combines the two PCZTs, signs the payment action, finalizes spends, extracts the transaction (proof and signatures are verified here) and sends it (`--server` overrides `https://zec.rocks:443`). `zsettle broadcast --tx trade.final.tx` resends a raw transaction.

## Verifying an item without any key

```
zsettle verify --listings listings.json --item <TXID>:<INDEX>
```

Streams compact blocks from the listing's birthday, trial-decrypts Ironwood actions with the listing's viewing key to find the genesis note, derives its nullifier, and follows the spend: the action that spends the item is the trade, and that action's output commitment is the item's new note. Output is a JSON report with the item name (from the genesis memo), every hop (txid, height, commitment) and a status: `CurrentlyHeld` by the listing's account, or `TransferredToUnknownHolder` after a sale. Tracing further needs the new holder's consignment. Under a second per item on mainnet today.

## Mainnet trades so far

| Txid | Block | What moved |
|---|---|---|
| `e76531b9e51b8a7cbc88188c71fd2df63f8224b0afe1ef2564fb52df91966ae2` | 3514109 | item `71167c88…b2274c:1` to the buyer, 5,000 zat to the seller, fee 15,000 |
| `2e0f4af17eddeb80d3ca570dcb4ac5c29a454926e27c79cd0944d232a899167c` | 3514119 | item `da9e8f95…1927e7:1` to the buyer, 5,000 zat to the seller, signed by the seller service |

## Notes

- Expiry defaults to 10 blocks. A longer expiry gives the buyer a free option on the seller's signature; the seller can cancel only by spending the item note itself.
- The seller service reads nullifiers and heights from its wallet database, so keep the wallet synced (`--sync-cmd`).
- Anyone holding the item viewing key learns when the item sells. That is inherent to consignment-style items.
