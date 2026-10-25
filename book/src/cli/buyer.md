# Buying with zsettle

The examples spell out every flag. With `env.sh` sourced, `--wallet`, `--identity` and
`--buyer-account` / `--account` can be left out. `--listings` names the seller's file, so give
it each time.

## 1. Look at the listings

```
zsettle list --listings listings.json
zsettle list --listings http://seller.example:8900/listings
```

`list` prints the network, the payout address, the key fingerprint, the height the file was
signed at (`issued at`), and whether the signature checks out. Then one line per item: note,
price, expiry limit and name.

The signature proves that the file was written by whoever controls these items. It does not
prove that this is the seller you meant. Compare the key fingerprint with one the seller gave
you some other way.

## 2. Import the viewing key

```
zsettle import-listing --wallet ~/.zsettle/buyer --listings listings.json
zsettle sync --wallet ~/.zsettle/buyer
```

`import-listing` adds the item account's viewing key to your wallet as a view-only account
(named `item-view` unless you pass `--name`), starting at the listing's birthday. The next
`sync` scans it. `import-listing`, `buy` and `verify` refuse a file that is unsigned or
altered, or that is for another network.

## 3. Build and prove the trade

```
zsettle buy --wallet ~/.zsettle/buyer --buyer-account <BUYER_ACCOUNT_UUID> \
  --listings http://seller.example:8900/listings --item <TXID>:<INDEX> \
  --out trade.pczt --seller-url http://seller.example:8900
```

What `buy` does:

1. Asks the light server which consensus branch is live, and stops if this build does not sign
   for it (`--offline` skips this check).
2. Looks up the item note and the paying note: by default the largest spendable note of the
   buyer account that is not reserved by an earlier offer (`--buyer-note` picks one).
3. Builds one Ironwood bundle with two spends at one anchor: the item note to a fresh address
   of the buyer with the item name as memo, the price to the seller's payout address, and
   change to the buyer if there is any. Then proves it.
4. Writes the full PCZT to `--out`. **It holds your viewing key and witnesses: keep it
   private.**
5. Writes the redacted copy for the seller to `<out>.seller.pczt`, or to `--seller-copy`.
6. Records the paying note as reserved, so a later `buy` does not use it again.
7. With `--seller-url`, posts the redacted copy to `<url>/offer`, checks the tag the seller
   answers with, prints the reply URL and saves it in `<out>.reply`. If the shop is down for a
   moment (a connection error, or a `502`, `503` or `504` answer), it posts the same offer
   again after ten seconds, up to four tries in all.

The JSON it prints includes `item_action_index` (where the item lands, needed for resale),
`fee_zat`, `change_zat`, `expiry_height`, `expect_signed` (the reply file name for a shared
folder) and `reply_url`.

Price, payout address and expiry come from the listing. `--seller`, `--price`,
`--expiry-delta`, `--buyer-note` and `--item-account` override them. The expiry is the wallet
library's standard (40 blocks before NU7, 120 from NU7) and never more; a smaller value prints
a warning because it makes the trade stand out on chain. `--receive-account` sends the item to a chosen account of your wallet (see
[Resale](resale.md)). By default the item goes to the wallet's `item` account (the one
`init` makes), or to the paying account if the wallet has none.

`buy` warns when the listings file was signed more than 1,000 blocks ago: check with the
seller that the price is still right.

## 4. Finish and broadcast

```
zsettle finish --wallet ~/.zsettle/buyer --identity ~/.zsettle/buyer.key \
  --full trade.pczt --out trade.final.tx --broadcast
```

`--signed` is the seller's signed copy: a file, or the reply URL. Without `--signed`,
`finish` reads the URL from `<full>.reply` and polls it every three seconds until the seller
has answered, for up to `--wait` seconds (default 300). It stops if the seller refused, and
keeps waiting through short outages of the shop (connection errors, `502` to `504`).

Then `finish`:

1. combines the two PCZTs (a field that differs fails the merge, so the seller cannot change
   the trade);
2. signs the payment action with the account at `--account-index` (default 0);
3. finalizes the spends and extracts the transaction (the proof and both signatures are
   checked here);
4. writes the raw transaction to `--out` and the final PCZT to `<out>.pczt`;
5. with `--broadcast`, checks the server's consensus branch and sends the transaction.

The final PCZT also holds your viewing key and witnesses: keep it private.

`zsettle broadcast --tx trade.final.tx` sends a raw transaction again.

## Pay from several small notes

A trade is paid from one note. If your balance is spread over several notes, merge them first:

```
zsettle consolidate --wallet B --account A --identity ~/buyer.key
```

It spends every spendable note of the account that is not reserved for an offer into one note
at the account's change address, for the ZIP 317 fee of that many actions. Notes whose memo
starts with `zsettle item` are left alone. `--dry-run` shows the plan. Keep items in the item
account, not in the paying account, to be sure.

## After an offer that did not settle

The seller keeps your envelope, and with it the nullifier of the note you paid with. If the
trade was mined, that nullifier is public anyway. If it was not (another buyer was first, the
offer expired, you gave up), retire the note:

```
zsettle rotate --wallet ~/.zsettle/buyer --account <BUYER_ACCOUNT_UUID> --identity ~/.zsettle/buyer.key
```

`rotate` syncs first, then walks the reservations `buy` recorded. It releases notes that are
already spent, keeps notes whose offers can still be mined, and moves every other note to the
account's change address in a plain two-action transaction. After that, the nullifier the
seller saw points only at that transfer. `--dry-run` only reports.

Reservations stay until `rotate` retires them, expired ones included, so `buy` never pays with
a note a seller already knows.

## Look at your notes

```
zsettle notes --wallet ~/.zsettle/buyer --account <UUID>
```

Lists the unspent Ironwood notes of an account, dust included, with value and mined height.
