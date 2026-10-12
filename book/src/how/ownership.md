# Ownership proofs

An ownership proof lets a holder show someone that they control an item note right now,
without spending it and without showing their ZEC balance or payout address. Code:
`crates/zsettle-core/src/ownership.rs`.

Typical use: a buyer, a shop or a friend wants to know you really hold an item before they
deal with you.

## How it works

1. The **checker** picks a challenge: any fresh text, for example a random word and the date.
2. The **holder** runs `zsettle-seller prove` with the item note and that challenge. The tool
   checks that the note is an unspent, mined note of the item account, then signs a proof.
3. The holder gives the proof file (or a URL to it) to the checker.
4. The checker runs `zsettle check-proof` with the proof and the same challenge.

```sh
# holder
zsettle-seller prove --item-note <txid>:<index> --challenge "blue-heron 13 Oct" --out proof.json

# checker
zsettle check-proof --proof proof.json --challenge "blue-heron 13 Oct"
```

## The proof

```json
{
  "version": 1,
  "network": "main",
  "item_ufvk": "uview1...",
  "item_birthday": 3514000,
  "note": "<txid>:<index>",
  "challenge": "blue-heron 13 Oct",
  "height": 3520000,
  "signature": "<128 hex characters>"
}
```

`height` is the chain height the holder's wallet had reached when signing.

The signed message is the text `zsettle-ownership-v1` and a newline, followed by the JSON of
the other fields in a fixed order (`version`, `network`, `item_ufvk`, `item_birthday`, `note`,
`challenge`, `height`). It is signed the same way as a [listings file](listings.md): a
RedPallas signature by the item account's spend authorizing key, which verifies under the
spend validating key inside `item_ufvk`. The different prefix means a listings signature can
never pass as an ownership proof, and the other way round.

## What the checker does

`check-proof`:

1. verifies the signature;
2. checks that the proof answers the checker's challenge, and is for the expected network;
3. scans the chain from the note's transaction with the viewing key in the proof: the note
   must decrypt with that key and must still be unspent.

It prints:

```json
{
  "valid": true,
  "held": true,
  "note": "<txid>:<index>",
  "name": "Item #1",
  "challenge": "blue-heron 13 Oct",
  "signed_at_height": 3520000,
  "scanned_to": 3520012
}
```

`valid` says the signature, challenge and network check out. `held` says the note is still
unspent and owned by the proof's key. When either is false, a `reason` field says why and the
command exits with status 1. `name` is read from the note's transaction memo and may be
`null`.

## What it reveals

The proof carries the **item account's viewing key**. With it the checker can see every item
that account has held, now and in the past, and when each one moved. If you keep one item per
account (`new-item-account`), that is the history of this one item.

It reveals nothing about your ZEC: payments and change live in the payout account, whose key
is not in the proof. The proof carries no payout address.

Like `verify`, `check-proof` asks the lightwalletd server for the note's transaction, so the
server learns which item is being checked.

## Limits

- **Use a fresh challenge every time.** The challenge is what stops an old proof from being
  shown again. If the checker reuses a challenge, or lets the holder pick it, a proof made
  earlier (perhaps before the item was sold) would still answer it. `check-proof` does catch a
  note spent since then, but only a fresh challenge shows that the holder can sign *now*.
- **A proof is good only while the item stays put.** Once the note is spent, by a sale, a
  `cancel` or any other move, `check-proof` reports `held: false`. A holder who moved the item
  to a new note of the same account must make a new proof naming the new note.
- A proof shows control of the item account's key. It does not say who the person is. If that
  matters, check the viewing key against one you already trust, such as the key in their
  signed listings file.
- The checker's view is only as fresh as its server: `scanned_to` is the last block it saw.
