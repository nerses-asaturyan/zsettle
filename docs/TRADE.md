# Running a trade

Two machines (or two wallet directories on one machine). Mainnet, Ironwood pool. Wallet setup is in [SETUP.md](SETUP.md); every command below takes `--server` to use another lightwalletd (default `https://zec.rocks:443`).

## Minting items

An item account can hold many items or exactly one. Whoever holds a listing sees every note of its account, so give an item its own account when that matters:

```
zsettle-seller new-item-account --wallet ~/.zsettle/seller --identity ~/.zsettle/seller/key.txt --name "Item #4"
```

It prints the account UUID and ZIP 32 index to use as `--item-account` and `--item-account-index` below.

```
zsettle-seller mint --wallet ~/.zsettle/seller --item-account <ITEM_ACCOUNT_UUID> --item-account-index 1 \
  --payout-account <PAYOUT_ACCOUNT_UUID> --identity ~/.zsettle/seller/key.txt --name "Item #3" --value 1000
zsettle-seller sync --wallet ~/.zsettle/seller
zsettle-seller items --wallet ~/.zsettle/seller --item-account <ITEM_ACCOUNT_UUID>
```

`mint` pays the item account at a fresh address from the payout account with the name in the memo and broadcasts; the resulting note is the item's genesis note. `items` lists the item account's unspent notes as `<txid>:<index>`, the reference `publish` takes.

## The market flow

| Step | Who | Command | Produces |
|---|---|---|---|
| 1 | seller | `zsettle-seller publish` | `listings.json`, signed: item viewing key (Orchard only), market payout address, items with prices |
| 2 | buyer | `zsettle list`, `zsettle import-listing`, `zsettle sync` | the signature checked, the item account imported as a view-only account and scanned |
| 3 | buyer | `zsettle buy --listings … --item …` | `trade.pczt` (private), `trade.pczt` for the seller's inbox |
| 4 | seller | `zsettle-seller serve` | syncs, checks every inbox file, verifies its proof, signs the item action, writes `<name>.<tag>.signed.pczt` to the outbox |
| 5 | buyer | `zsettle finish --broadcast` | the settled transaction, sent through lightwalletd |
| 6 | seller service | (next pass) | sees the item note spent and removes it from the listings, re-signing the file |

### 1. Seller publishes

```
zsettle-seller publish \
  --wallet ~/.zsettle/seller --identity ~/.zsettle/seller/key.txt \
  --item-account <ITEM_ACCOUNT_UUID> --item-account-index 1 --payout-account <PAYOUT_ACCOUNT_UUID> \
  --listings listings.json --item-note <TXID>:<INDEX> --name "Item #3" --price 5001 --max-expiry-delta 40
```

The item account is a separate ZIP 32 account of the seller's wallet, and only its Orchard viewing key is published, so the file reveals nothing about the seller's other funds. The payout address is derived for the market, not the wallet's default address. Prices must be above 5,000 zat (smaller payments are dust no wallet spends). The file is signed with the item account's spend key; buyers verify it against the viewing key inside the file. How the file reaches buyers is up to the seller: a web page, a message, a repository.

### 2. Buyer imports the listing

```
zsettle list --listings listings.json
zsettle import-listing --wallet ~/.zsettle/buyer --listings listings.json
zsettle sync --wallet ~/.zsettle/buyer
```

`list` says whether the signature checks out; `import-listing`, `buy` and `verify` refuse a file that is unsigned or altered.

### 3. Buyer builds and proves

```
zsettle buy --wallet ~/.zsettle/buyer --buyer-account <BUYER_ACCOUNT_UUID> \
  --listings listings.json --item <TXID>:<INDEX> \
  --out trade.pczt --seller-copy inbox/trade.pczt
```

The buyer first asks the server which consensus branch is live and stops if this build does not sign for it (`--offline` skips the check). The item note and the buyer's largest spendable note that is not reserved by an unsettled trade are looked up in the wallet, their witnesses are taken at one anchor, a two-spend Ironwood bundle is built (item note to a fresh address of the buyer with the item name as memo, price to the seller, change to the buyer) and proven with the two full viewing keys. The seller's copy keeps only what fixes the sighash, the proof and the seller's own note data on the item action; the buyer's viewing key, witnesses, note randomness, randomizers and value trapdoors are removed. The paying note is recorded in the wallet directory as reserved until the trade's expiry height. Price, payout address and expiry come from the listing; `--seller`, `--price`, `--expiry-delta`, `--buyer-note` and `--item-account` override them. `--receive-account` sends the item to another account of the buyer's wallet (see Resale). The output names the reply to expect in the seller's outbox (`expect_signed`).

### 4. Seller service signs

```
zsettle-seller serve \
  --wallet ~/.zsettle/seller --identity ~/.zsettle/seller/key.txt \
  --item-account <ITEM_ACCOUNT_UUID> --item-account-index 1 --payout-account <PAYOUT_ACCOUNT_UUID> \
  --listings listings.json --inbox inbox --outbox outbox
```

Every pass (default every 5 seconds):

1. **Sync.** The seller wallet is brought up to date with the built-in synchronizer. A pass whose sync fails signs nothing; it is logged and retried with exponential backoff (up to five minutes). `--sync-cmd` runs a shell command instead, `--no-sync` skips the step.
2. **Reconcile.** Each listed item is checked against the wallet's tables: an item whose note the wallet has recorded as spent is removed (`sold`, with the spending txid) and the file is re-signed. A note the wallet cannot see right now stays listed (`not_in_wallet`).
3. **Inbox.** For every `*.pczt` that has not changed for two seconds and is under 1 MB, the service finds which listed item the trade spends, then checks: the bundle's anchor is a tree root the seller's own wallet has for one of the last 100 blocks; the expiry is set, after the chain tip and within the listing's limit (one block of slack, since the buyer measures from the next block); exactly one action spends that item note and it carries a randomizer; no other action spends any note of the seller's wallet; the outputs that decrypt under the seller's incoming viewing key add up to at least the price. The proof is verified last, against the bundle's public inputs, and only once per envelope, so an offer that fails the cheap checks costs the service nothing. The signer verifies the nullifier and `rk` of the item action against the seller's key and signs that action only. The reply is written to `outbox/<name>.<tag>.signed.pczt`, where the tag is derived from the envelope's content, and the envelope moves to `inbox/done`. A refused envelope is deleted (the buyer keeps the original) and the reason kept in `inbox/rejected/<name>.reason.txt`; a later file with the same name gets a numbered reason file. Filed envelopes in `inbox/done` are deleted after a day. The service judges an envelope in three stages: the public checks, then the proof, then the checks that use its own keys, so a refusal before the proof reveals nothing about the seller's notes. An envelope that spends none of the items this wallet can spend, while some listed item is unscanned or too recent, is deferred: logged and left in the inbox for a later pass, until its expiry passes. An envelope a pass fails on is deferred the same way.

Every acceptable offer is signed, whether or not another offer for the same item is outstanding: all of them spend the same nullifier, so the chain mines at most one and the seller is paid once. Every event is one JSON line on stdout (`start`, `signed`, `rejected`, `deferred`, `sold`, `not_in_wallet`, `already_signed`, `error`). The service is safe to kill and restart at any point: replies and listings are written atomically, the envelope is moved last, and an envelope whose reply already exists is filed without signing again. `--once` runs a single pass and exits (non-zero if the pass failed); `zsettle-seller sign` does the same for one file, with `--dry-run` to check only.

### Over the network

The inbox and outbox can be directories the parties share, or the service can listen:

```
zsettle-seller serve --wallet S --item-account I --payout-account P --identity ~/seller.key --listings listings.json --inbox inbox --outbox outbox --listen 0.0.0.0:8900
zsettle buy    --wallet B --buyer-account A --listings listings.json --item <txid>:<idx> --out trade.pczt --seller-url http://seller.example:8900
zsettle finish --wallet B --identity ~/buyer.key --full trade.pczt --signed http://seller.example:8900/reply/<tag> --out trade.tx --broadcast
```

The service also answers `GET /listings` with the signed listings file, so buyers can fetch it from the same address.

`buy` posts the redacted copy to `/offer` (the answer names its content tag, which `buy` checks) and prints the reply URL; `finish` polls that URL until the signed copy is there (202 while pending, 410 with the reason if refused). Each buyer can fetch only the reply to its own envelope; nothing else is served. The endpoint reads a request head of at most 8 KiB with a ten-second timeout, refuses bodies above 1 MiB before reading them, serves at most 32 connections at once and answers 503 once 1,000 offers wait in the inbox. Plain HTTP carries the redacted envelope and the signed copy, which are safe to see, but it lets an observer link an IP to an offer: put a TLS reverse proxy or a Tor hidden service in front of it for the internet; a buyer reaches a hidden service with `HTTPS_PROXY=socks5://127.0.0.1:9050` (or `ALL_PROXY`) set for `buy` and `finish`.

### 5. Buyer finishes and broadcasts

```
zsettle finish --wallet ~/.zsettle/buyer --identity ~/.zsettle/buyer/key.txt --account-index 0 \
  --full trade.pczt --signed outbox/trade.<tag>.signed.pczt --out trade.final.tx --broadcast
```

Combines the two PCZTs (fields that differ fail the merge, so the seller cannot alter the trade), signs the payment action, finalizes spends, extracts the transaction (proof and signatures are verified here), checks the server's consensus branch and sends it. `zsettle broadcast --tx trade.final.tx` resends a raw transaction.

## Taking an item off the market

```
zsettle-seller publish --wallet S --item-account I --payout-account P --identity S/key.txt --listings listings.json --remove <txid>:<idx>
zsettle-seller cancel  --wallet S --item-account I --payout-account P --identity S/key.txt --listings listings.json --item-note <txid>:<idx>
```

`publish --remove` only delists: an offer the service already signed stays valid until its expiry (at most 40 blocks). `cancel` ends it for good by spending the item note to a fresh address of the item account; an item note alone is below the fee, so one payout-account note is spent with it (exactly the fee, or with change above dust). The listings file loses the entry and is signed again; once the transaction is mined, `items` shows the item's new note, which `publish --item-note` can list again. The service defers an offer whose anchor or expiry it cannot judge yet because the buyer is a few blocks ahead, instead of refusing it for good.

## Paying with several small notes

A trade is paid from one note. If the balance is spread over several, merge them first:

```
zsettle consolidate --wallet B --account A --identity B/key.txt
```

It spends every spendable note of the account that is not reserved for an offer into one note at the account's change address, for the ZIP 317 fee of that many actions. `--dry-run` shows the plan.

## After an offer that did not settle

The seller keeps your envelope, and with it the nullifier of the note you paid with. If the trade was mined that nullifier is public anyway; if it was not (you lost the race, the offer expired, you gave up), retire the note:

```
zsettle rotate --wallet ~/.zsettle/buyer --account <BUYER_ACCOUNT_UUID> --identity ~/.zsettle/buyer/key.txt
```

It walks the reservations `buy` recorded (which stay until then, expired ones included, so that `buy` never pays with a note the seller already knows), drops the ones whose notes are already spent, keeps the ones whose offers can still be mined, and moves every other note to the account's change address in an ordinary two-action transaction. `--dry-run` only reports.

## Resale

A buyer receives the item into the wallet's own item account by default (the one `init` makes), so that publishing that account's viewing key later reveals nothing about the account that paid; `--receive-account` chooses another:

```
zsettle buy … --receive-account <BUYER_ITEM_ACCOUNT_UUID> …
```

After the trade is mined, the buyer is a seller: `zsettle-seller publish --wallet ~/.zsettle/buyer --identity ~/.zsettle/buyer/key.txt --item-account <BUYER_ITEM_ACCOUNT_UUID> --item-account-index 1 --payout-account <BUYER_ACCOUNT_UUID> --listings mine.json --item-note <TRADE_TXID>:<ITEM_ACTION_INDEX> …` (the item action index is printed by `buy`), then `zsettle-seller serve` with the same arguments. The next buyer proceeds exactly as above with `mine.json`.

## Verifying an item with viewing keys only

```
zsettle verify --listings listings.json --item <TXID>:<INDEX>
zsettle verify --listings listings.json --listings mine.json --item <TXID>:<INDEX>
```

Asks the server which block holds the genesis transaction, streams compact blocks from there, trial-decrypts Ironwood actions with the listings' viewing keys to find the genesis note, derives its nullifier, and follows the spend: the action that spends the item is the trade, and that action's output commitment is the item's new note. When another supplied listing decrypts that note, the trace continues with it. `--private` skips asking the server about the genesis transaction (which tells it which item you look at); the trace then starts at the listings' birthdays, which still shows the server which consignment you are interested in, and carries no name. Output is a JSON report with the item name (from the genesis memo), every hop (txid, height, commitment, holder) and a status: `held` by a named listing, or `transferred_to_unknown_holder` after a sale to someone whose listings were not supplied. About two seconds per item on mainnet today, most of it connection setup.

## Mainnet trades so far

| Txid | Block | What moved |
|---|---|---|
| `e76531b9e51b8a7cbc88188c71fd2df63f8224b0afe1ef2564fb52df91966ae2` | 3514109 | item `71167c88…b2274c:1` to the buyer, 5,000 zat to the seller, fee 15,000 |
| `2e0f4af17eddeb80d3ca570dcb4ac5c29a454926e27c79cd0944d232a899167c` | 3514119 | item `da9e8f95…1927e7:1` to the buyer, 5,000 zat to the seller, signed by the seller service |
| `e70f48902faaf55bf3cda050f08a9ee8f3a1b02abb780c46ba3e8d6064618877` | 3514159 | item `6f1c8e26…bca26a:1` to the buyer's own item account (`--receive-account`), 5,000 zat to the seller |
| `af09c2cf525d2bb6e553b61a1d3cc2674ebdf09726be1e3959ee36f5143f5804` | 3514174 | resale: the buyer listed the item from its item account, the original seller bought it back for 7,000 zat with an exact note (two actions, fee 10,000); the buyer's wallet ran `serve` and signed |

The two listings files are in `examples/` (`market.json`, `market2.json`); `zsettle verify --listings examples/market.json --listings examples/market2.json --item af09c2cf525d2bb6e553b61a1d3cc2674ebdf09726be1e3959ee36f5143f5804:0` needs no wallet and no funds. `zsettle verify --listings market/listings.json --listings market2/listings.json --item 6f1c8e26…:1` follows item #3 from its genesis (first seller's key) into the buyer's item account (second key) in under a second. It reports the resale as a transfer to an unknown holder: that transaction was built before the action-pairing rule was enforced (see [PROTOCOL.md](../spec/PROTOCOL.md), Items), so the new item note ended up in a different action from the item spend and the lineage, as the protocol defines it, stops there.

| 70b4a0e550ced3ddc6039248ed860ae383e111e70d7ee01d2ba6b06a50e2ffb3 | 3514587 | mint on consignment with the final code: the buyer wallet paid a 17,999-zat note into the seller's item account through the imported viewing key (`zsettle-seller mint --item-account <item-view> --value 17999`), exactly its two notes minus the fee |
| b378dfbc39c7ad454e0c1eab0b95114b2b1ff5a995dc35f54e728f88687bea9c | 3514597 | final-code trade over HTTP: the seller wallet bought item #3 (`c1c554f2…06f6:2`) from the buyer wallet's listing, paying 7,999 zat with that note as an exact two-action payment (fee 10,000); `serve --listen`, `buy --seller-url`, `finish --signed <reply url> --broadcast`. The item output went to a fresh address under the buyer's outgoing key, the trade carries the default expiry and fee, and `verify` with both consignments shows the whole lineage from the genesis mint through three holders |

The four earlier trades predate the privacy review: their item outputs were encrypted to the seller's published outgoing key and sent to the buyer's default address, and their expiry was ten blocks. The Orchard key in `examples/market.json` is that seller item account's key, so anyone with the file can read those four item notes and the demo buyer's default address. Both wallets are throwaway demo wallets. Trades built since do none of this, which is why the old transactions are listed here rather than quietly dropped.

## Notes

- Expiry defaults to 40 blocks, the same as every librustzcash wallet transaction, so a trade carries no distinguishing expiry; `buy` never uses more, and `publish` warns about a smaller limit.
- Stop `serve` before running `publish` on the same listings file; both rewrite it and the wallet database.
- The seller learns the nullifier of the paying note from the envelope. If an offer does not settle, spend that note to yourself before reusing it, or the seller can recognise its later spend. After expiry the seller's signature is worthless; the seller can cancel earlier by spending the item note.
- Anyone holding the item viewing key learns when the item sells. That is inherent to consignment-style items.
- The buyer's full PCZT (`--out`) and the final PCZT written by `finish` contain the buyer's full viewing key and witnesses: keep them private. The seller's copy and the signed reply do not.
- The light server sees which IP syncs an imported key, broadcasts a trade or fetches a genesis transaction. Use Tor or your own lightwalletd if that matters.
