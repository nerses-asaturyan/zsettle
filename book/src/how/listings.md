# Listings and signatures

A listings file is a JSON file a seller publishes anywhere. Code:
`crates/zsettle-core/src/listing.rs`.

## Content

```json
{
  "version": 1,
  "network": "main",
  "item_ufvk": "uview1...",
  "item_birthday": 3514000,
  "payout_address": "u1...",
  "items": [
    { "note": "<txid>:<index>", "name": "Item #1", "price_zat": 100000, "max_expiry_delta": 40 }
  ],
  "issued_at": 3514600,
  "signature": "<128 hex characters>"
}
```

| Field | Meaning |
|---|---|
| `version` | Format version. Only `1` is accepted. |
| `network` | `main` or `test`. |
| `item_ufvk` | Unified full viewing key of the item account, with the **Orchard component only**. |
| `item_birthday` | Height to scan from to see the item notes. Current builds round account birthdays down to a multiple of 1,000 blocks. |
| `payout_address` | Unified address that receives payments. Derived for the market, never the account's default address. |
| `items` | One entry per item: note reference, display name, price in zatoshis (above 5,000), and the most blocks a signed offer may stay valid. |
| `issued_at` | Chain height at which this version was signed. |
| `signature` | Hex RedPallas signature over all other fields. |

## How it is signed

The signed message is the text `zsettle-listings-v1` and a newline, followed by the JSON of
the other fields in a fixed order (`version`, `network`, `item_ufvk`, `item_birthday`,
`payout_address`, `items`, `issued_at`).

The signature is a RedPallas spend-authorization signature made with the item account's spend
authorizing key, with randomizer zero. So it verifies under the spend validating key `ak`,
which is the first 32 bytes of the published Orchard viewing key. Anyone who has the file can
check it; no other key is needed.

The domain prefix means a listings signature can never be mistaken for a signature over
anything else.

## How it is checked

`import-listing`, `buy` and `verify` refuse a file that is unsigned or whose signature does not
verify. `list` shows the file either way, and says `NOT TRUSTWORTHY` when the signature fails.
The seller service and `publish` also refuse to re-sign their own file when its signature does
not verify, unless `publish --resign-unverified` is given.

The seller service signs the file again whenever it changes it, for example after a sale.

## What the signature does not prove

It proves the file was written by whoever controls the items in it. It does not prove that
this is the seller you meant. `list` prints a short **key fingerprint**: the first 6 bytes, in
hex, of a BLAKE2b-256 hash (personalisation `zsettle_listkey_`) of `item_ufvk`. Compare it with
a fingerprint the seller gave you some other way.

## Stale files

A buyer could be shown an old file with an old, higher price. `issued_at` is part of the
signed body, and `buy` warns when the file is more than 1,000 blocks old.

## Fetching by URL

Every `--listings` flag of the buyer tool takes a file path or an `http://` or `https://` URL,
for example `http://seller:8900/listings`. A URL is fetched each time, verified, and never
written to disk. Redirects are not followed, and a file larger than 1 MiB is refused. The
proxy rules are in [Privacy](../privacy.md#network-and-proxies).
