# Resale and item history

## Receive the item where you can resell it

By default `buy` sends the item to the wallet's own `item` account (index 1, made by `init`).
Publishing that account's viewing key later reveals nothing about the account that paid.
`--receive-account` picks another account:

```
zsettle buy ... --receive-account <BUYER_ITEM_ACCOUNT_UUID> ...
```

If the wallet has no item account, the item goes to the paying account and `buy` prints a
note about it.

## Sell it again

Once the trade is mined, the buyer is a seller. Run `zsettle-seller` on the buyer wallet. The
item note is `<trade txid>:<item_action_index>`, where `item_action_index` is printed by `buy`.

```
zsettle-seller publish --wallet ~/.zsettle/buyer --identity ~/.zsettle/buyer.key \
  --item-account <BUYER_ITEM_ACCOUNT_UUID> --item-account-index 1 --payout-account <BUYER_ACCOUNT_UUID> \
  --listings mine.json --item-note <TRADE_TXID>:<ITEM_ACTION_INDEX> --name "Item #3" --price 0.0001
zsettle-seller serve --wallet ~/.zsettle/buyer --identity ~/.zsettle/buyer.key \
  --item-account <BUYER_ITEM_ACCOUNT_UUID> --item-account-index 1 --payout-account <BUYER_ACCOUNT_UUID> \
  --listings mine.json --inbox inbox --outbox outbox --listen 0.0.0.0:8900
```

The buyer wallet's `env.sh` names only the buyer account, so here the seller flags are typed
out. The next buyer goes through the same steps as before, with `mine.json`.

## Check an item's history

```
zsettle verify --listings listings.json --item <TXID>:<INDEX>
zsettle verify --listings listings.json --listings mine.json --item <TXID>:<INDEX>
```

`verify` needs no wallet and no spending key, only the listings files. Pass `--listings` once
for each holder the item went through. `--item` is the genesis note, or any later note that
one of the listings decrypts.

The output is a JSON report:

- `name`: the item name from the genesis memo (when it can be read);
- `genesis`: the genesis note;
- `hops`: every step, with txid, height, note commitment, value and holder;
- `status.state`: `held` (by the named listings file), `transferred_to_unknown_holder` (sold
  to someone whose listings were not given), or `lineage_broken` (the note was spent in a way
  a trade never does);
- `scanned_to`: the last height scanned.

`--private` does not ask the server which block holds the genesis transaction (that request
tells the server which item you look at). The scan then starts at the listings' birthdays,
which still shows the server which consignment you care about, and the report has no item
name. `--to-height` stops the scan at a given height.

How the trace works is in [Item history](../how/history.md).
