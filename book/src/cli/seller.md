# Selling with zsettle-seller

The examples below spell out every flag. With `env.sh` sourced, the wallet, identity,
account and listings flags can be left out.

## One account per item

Whoever holds a listings file sees every note of its item account, and when each one is
spent. If that matters, give an item its own account:

```
zsettle-seller new-item-account --wallet ~/.zsettle/seller --identity ~/.zsettle/seller.key --name "Item #4"
```

It prints the account UUID and ZIP 32 index. Use them as `--item-account` and
`--item-account-index` below.

## Mint items

```
zsettle-seller mint --wallet ~/.zsettle/seller --item-account <ITEM_ACCOUNT_UUID> --item-account-index 1 \
  --payout-account <PAYOUT_ACCOUNT_UUID> --identity ~/.zsettle/seller.key --name "Item #3" --value 1000
zsettle-seller sync --wallet ~/.zsettle/seller
zsettle-seller items --wallet ~/.zsettle/seller --item-account <ITEM_ACCOUNT_UUID>
```

`mint` pays the item account from the payout account, at a fresh address for each item, with
the memo `zsettle item: <name>`, and broadcasts. The new note is the item's genesis note.
Repeat `--name` to mint several items in one transaction. `--value` is the value of each item
note (default and minimum 1,000 zatoshis). `mint` prints the new notes as `<txid>:<index>`.

`items` lists the unspent notes of the item account as `<txid>:<index>`, with value and
mined height. This is the reference `publish` takes. Wait until the mint is mined and synced
before you publish.

## Publish listings

```
zsettle-seller publish \
  --wallet ~/.zsettle/seller --identity ~/.zsettle/seller.key \
  --item-account <ITEM_ACCOUNT_UUID> --item-account-index 1 --payout-account <PAYOUT_ACCOUNT_UUID> \
  --listings listings.json --item-note <TXID>:<INDEX> --name "Item #3" --price 5001 --max-expiry-delta 40
```

`publish` creates or updates the signed listings file, one item per call:

- The file carries only the item account's Orchard viewing key, so it reveals nothing about
  the seller's other funds.
- The payout address is a new address derived for the market, not the wallet's default
  address. It must belong to `--payout-account`.
- `--price` must be above 5,000 zatoshis.
- `--max-expiry-delta` (default 40) is the most blocks a signed offer may stay valid. Any
  other value prints a warning: it makes every trade of the item recognisable on chain.
- The file is signed with the item account's spend key. Buyers check it against the viewing
  key inside the file.

Without `--item-note`, `publish` checks the existing file and signs it again. It refuses a
file whose signature does not verify, unless you pass `--resign-unverified` (for a file you
edited on purpose). Every entry is still checked: price floor, and that each note belongs to
the item account.

`publish` warns when the item account index is 0 (the paying account) or when the item
account holds more than one note.

How the file reaches buyers is up to you: the service serves it at `GET /listings`, or you can
put it on a web page, in a message or in a repository.

## Run the service

```
zsettle-seller serve \
  --wallet ~/.zsettle/seller --identity ~/.zsettle/seller.key \
  --item-account <ITEM_ACCOUNT_UUID> --item-account-index 1 --payout-account <PAYOUT_ACCOUNT_UUID> \
  --listings listings.json --inbox inbox --outbox outbox --listen 0.0.0.0:8900
```

The service watches the inbox for `*.pczt` envelopes. Every pass (default every 5 seconds,
`--poll-secs`):

1. **Sync.** The wallet is brought up to date. A pass whose sync fails signs nothing; it is
   logged and retried with exponential backoff, up to five minutes. `--sync-cmd <command>` runs
   a shell command instead of the built-in sync; `--no-sync` skips the step.
2. **Reconcile.** An item whose note the wallet has recorded as spent is removed from the
   listings (`sold`, with the spending txid), and the file is signed again. A note the wallet
   cannot see right now stays listed (`not_in_wallet`).
3. **Inbox.** Each envelope that has not changed for two seconds and is at most 1 MiB is
   checked, in arrival order. What is checked, and in which order, is in
   [The two-party PCZT flow](../how/pczt-flow.md#what-the-seller-checks).
   - Accepted: the service signs the item action only, writes the reply to
     `outbox/<name>.<tag>.signed.pczt`, and moves the envelope to `inbox/done/`.
   - Refused: the envelope is deleted (the buyer keeps the original) and the reason is kept
     in `inbox/rejected/<name>.reason.txt`.
   - Deferred: an envelope the wallet cannot decide on yet (the buyer is a few blocks ahead, or a
     listed note is not scanned yet) stays in the inbox for a later pass. A deferral is given
     up after ten blocks, and at most 100 envelopes stay deferred in one pass.

Filed envelopes, signed replies and reason files are deleted after a day.

Every acceptable offer is signed, even when another offer for the same item is still open.
All of them spend the same nullifier, so the chain mines at most one and the seller is paid
once. Nothing is locked, so nobody can keep an item off the market by sending offers.

Every event is one JSON line on stdout: `start`, `listening`, `signed`, `rejected`,
`deferred`, `sold`, `not_in_wallet`, `already_signed`, `dropped`, `error`.

The service is safe to stop and restart at any time. Replies and listings are written
atomically, the envelope is moved last, and an envelope whose reply already exists is filed
without signing again. `--once` runs one pass and exits (non-zero if the pass failed).

Stop `serve` before you run `publish` on the same listings file: both rewrite it and the
wallet database.

### Over HTTP

With `--listen <address>`, the service also answers HTTP:

- `GET /listings`: the signed listings file;
- `POST /offer`: the envelope as the body; the answer names its content tag;
- `GET /reply/<tag>`: the signed copy once it exists (202 while pending, 410 with the reason
  if refused).

Each buyer can fetch only the reply to its own envelope. Details and limits are in the
[HTTP API reference](../reference/http-api.md). Plain HTTP carries nothing secret (the
redacted envelope and the signed copy are safe to see), but it lets an observer link an IP to
an offer. For the internet, put a TLS reverse proxy or a Tor onion service in front.

### Without the service

`zsettle-seller sign --input <envelope> --out <signed>` checks and signs one envelope, with
the same checks. `--dry-run` checks only. It syncs first unless `--no-sync` is given.

## Take an item off the market

```
zsettle-seller publish --wallet S --item-account I --payout-account P --identity ~/seller.key --listings listings.json --remove <txid>:<idx>
zsettle-seller cancel  --wallet S --item-account I --payout-account P --identity ~/seller.key --listings listings.json --item-note <txid>:<idx>
```

- `publish --remove` only delists. An offer the service already signed stays valid until its
  expiry (at most 40 blocks with the default limit).
- `cancel` ends it for good. It spends the item note to a fresh address of the item account.
  An item note alone cannot pay the fee, so one payout-account note is spent with it: one
  worth exactly the fee, or one that leaves change above dust. The entry leaves the listings
  file, which is signed again. Once the transaction is mined, `items` shows the item's new
  note, and `publish --item-note` can list it again.
