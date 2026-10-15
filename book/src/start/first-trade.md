# A first trade on the command line

This walk-through uses two wallet directories, `S` (seller) and `B` (buyer), in two
terminals. It works on mainnet with tiny amounts. It takes about ten minutes, most of it
waiting for confirmations.

## Once: create the wallets

Keep the key files outside the wallet directories. Leave out `--identity` to protect the
wallet with a passphrase instead.

```
zsettle-seller init --wallet S --identity ~/seller.key   # prints the payout and item accounts and an address to fund; writes S/env.sh
zsettle init        --wallet B --identity ~/buyer.key    # prints the buyer and item accounts and an address to fund; writes B/env.sh
```

Fund both wallets at the `fund` address that `init` printed:

- the seller needs about 0.0005 ZEC for mint fees;
- the buyer needs the price plus 0.00015 ZEC per trade.

A note received from another wallet is spendable after ten confirmations (about 13 minutes
on mainnet); the wallet's own change after three. See the [FAQ](../faq.md) for why.

## Seller terminal

```
source S/env.sh                                          # PowerShell: . S\env.ps1
zsettle-seller sync
zsettle-seller mint --name "Item #1" --name "Item #2"    # one transaction; prints each note as <txid>:<idx>
# wait for that block (about 75 seconds), then
zsettle-seller sync
zsettle-seller publish --item-note <txid>:<idx> --name "Item #1" --price 0.001   # writes S/listings.json; one call per item
zsettle-seller serve --inbox inbox --outbox outbox --listen 0.0.0.0:8900         # serves the file at http://seller:8900/listings
```

## Buyer terminal

```
source B/env.sh
zsettle list           --listings http://seller:8900/listings                    # a copy of the file works too
zsettle import-listing --listings http://seller:8900/listings
zsettle sync
zsettle buy            --listings http://seller:8900/listings --item <txid>:<idx> --out trade.pczt --seller-url http://seller:8900
zsettle finish         --full trade.pczt --out trade.tx --broadcast              # polls the reply URL that buy saved in trade.pczt.reply
```

## Anyone

```
zsettle verify --listings http://seller:8900/listings --item <txid>:<idx>
```

It prints a JSON report. `status.state` is `held` while the holder's key opens the last note.

## Without a network between the two

The parties can share a folder instead of using HTTP. The buyer writes the offer into the
seller's inbox and reads the reply from the outbox:

```
zsettle buy    ... --out trade.pczt --seller-copy inbox/trade.pczt
zsettle finish --full trade.pczt --signed outbox/trade.<tag>.signed.pczt --out trade.tx --broadcast
```

`buy` prints the exact reply file name to expect (`expect_signed`).

## Notes on this walk-through

- `init` prints the account UUIDs (`accounts` lists them again) and writes `env.sh` and
  `env.ps1` into the wallet directory. Once you source it, no `--wallet`, `--identity`,
  account or `--listings` flag is needed, which is why the commands above are so short.
- Every `--listings` takes a file path or the URL a running service serves it at.
- Amounts are zatoshis, or ZEC with a decimal point (`--price 0.001`).
- Commands that act print one JSON line. `list`, `notes` and `items` print tables.
- `zsettle-seller pay` and `zsettle pay` send ZEC out to any address, a phone wallet
  included. No other tool is needed.

The [CLI guide](../cli/index.md) explains each step in detail.
