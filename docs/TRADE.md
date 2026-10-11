# Running a trade

Three commands, two machines (or two wallet directories on one machine). Mainnet, Ironwood pool.

## Roles and files

| Who | Command | Reads | Writes |
|---|---|---|---|
| buyer | `zsettle buy` | buyer wallet (own account + the item account's imported viewing key) | `trade.pczt` (private, keep), `trade.seller.pczt` (redacted, send to seller) |
| seller | `zsettle-seller sign` | seller wallet (item account + payout account) | `trade.seller.signed.pczt` (send back) |
| buyer | `zsettle finish` | buyer wallet, both PCZTs | `trade.final.tx` (raw transaction), `trade.final.pczt`; optionally broadcasts |

## 1. Buyer builds and proves

```
zsettle buy \
  --wallet ~/.zsettle/buyer \
  --buyer-account <BUYER_ACCOUNT_UUID> \
  --item-account <IMPORTED_ITEM_ACCOUNT_UUID> \
  --item-note <TXID>:<INDEX> \
  --buyer-note <TXID>:<INDEX> \
  --seller <SELLER_UNIFIED_ADDRESS> \
  --price 5000 --expiry-delta 10 --min-confirmations 3 \
  --out trade.pczt --seller-copy trade.seller.pczt
```

What happens: both notes are looked up in the buyer's wallet database, their witnesses are taken at one anchor, a two-spend Ironwood bundle is built (item note to the buyer, price to the seller, change to the buyer), proven with the two full viewing keys, and redacted for the seller. The seller's copy keeps only what fixes the sighash plus the seller's own note data on the item action; the buyer's viewing key, witnesses, note randomness and randomizers are removed.

## 2. Seller checks and signs

```
zsettle-seller sign \
  --wallet ~/.zsettle/seller --identity ~/.zsettle/seller/key.txt \
  --item-account <ITEM_ACCOUNT_UUID> --item-account-index 1 \
  --payout-account <PAYOUT_ACCOUNT_UUID> \
  --item-note <TXID>:<INDEX> --price 5000 --max-expiry-delta 10 \
  --input trade.seller.pczt --out trade.seller.signed.pczt
```

Checks before signing: expiry within the limit (one block of slack for the buyer measuring from the next block), exactly one action spends the listed item note, that action carries a randomizer, and the outputs that decrypt under the seller's incoming viewing key add up to at least the price. The signer itself then verifies the nullifier and `rk` of the item action against the seller's key. Add `--dry-run` to check without signing.

## 3. Buyer finishes and broadcasts

```
zsettle finish \
  --wallet ~/.zsettle/buyer --identity ~/.zsettle/buyer/key.txt --account-index 0 \
  --full trade.pczt --signed trade.seller.signed.pczt \
  --out trade.final.tx --broadcast
```

Combines the two PCZTs, signs the payment action, finalizes spends, extracts the transaction (proof and signatures are verified here) and sends it through lightwalletd (`--server` to override `https://zec.rocks:443`). `zsettle broadcast --tx trade.final.tx` resends a raw transaction.

## First mainnet trade

`e76531b9e51b8a7cbc88188c71fd2df63f8224b0afe1ef2564fb52df91966ae2`: item note `71167c88…b2274c:1` (1,000 zat) moved from the seller's item account to the buyer; 5,000 zat moved from the buyer to the seller; fee 15,000 zat; three actions.

## Notes

- Item notes must carry at least 1,000 zat. A zero-value note is not bound to the tree by the circuit.
- Expiry defaults to 10 blocks. A longer expiry gives the buyer a free option on the seller's signature.
- The item account is a separate ZIP 32 account in the seller's wallet (index 1 here) so that handing out its viewing key reveals nothing about the seller's other funds.
