# Ownership proofs

An ownership proof lets a holder show anyone that they control an item note, without spending
it and without showing their ZEC balance or payout address. Code:
`crates/zsettle-core/src/ownership.rs`.

The proof is **non-interactive**: the holder makes it once and shares it, like an address,
and it expires by itself after a while. No one has to send the holder anything first.

Typical uses: post "I own this item" where people can check it, or show a door, a shop or a
friend that you hold an item before they deal with you.

## How it works

1. The **holder** runs `zsettle-seller prove` with the item note. The tool syncs, checks that
   the note is an unspent, mined note of the item account, and signs a proof that names the
   last block the wallet scanned: its height and its **hash**.
2. The holder shares the proof: a file, a URL, a message or a QR code.
3. Anyone runs `zsettle check-proof` with it.

```sh
# holder
zsettle-seller prove --item-note <txid>:<index> --label "@alice" --out proof.json

# anyone
zsettle check-proof --proof proof.json
```

### Why naming a block makes it fresh

A block's hash cannot be known before the block is mined. A proof that signs a block's hash
was therefore made **after** that block. The checker accepts a proof only while that block is
recent: by default, at most 100 blocks old (about 40 minutes on the testnet, about two hours
on mainnet; `check-proof --max-age` changes it). An old proof expires by itself.

This is the Fiat-Shamir idea applied to freshness: instead of the checker sending a random
challenge, the chain supplies a public value nobody could have chosen in advance. Hashing the
statement alone would not be enough: the statement never changes, so such a proof would be
valid forever.

### Live check (optional)

Freshness from a block is a window, not an instant: within the window, the same proof can be
shown more than once. When that matters, for example at a door, the checker gives the holder a
fresh code first (`prove --challenge <code>`, then `check-proof --challenge <code>`). The proof
then answers only that one check. A shared proof carries no code; a code added or changed
later breaks the signature.

## The proof

```json
{
  "version": 2,
  "network": "main",
  "item_ufvk": "uview1...",
  "item_birthday": 3514000,
  "note": "<txid>:<index>",
  "height": 3520000,
  "block_hash": "<64 hex digits>",
  "label": "@alice",
  "signature": "<128 hex characters>"
}
```

- `height` and `block_hash` name the recent block the proof was made after.
- `label` is optional: a name or handle the holder chose (at most 80 characters).
- `challenge` is present only in a proof made for a live check.

The signed message is the text `zsettle-ownership-v2` and a newline, followed by the JSON of
the other fields in a fixed order (`version`, `network`, `item_ufvk`, `item_birthday`, `note`,
`height`, `block_hash`, `label`, `challenge`). It is signed the same way as a
[listings file](listings.md): a RedPallas signature by the item account's spend authorizing
key, which verifies under the spend validating key inside `item_ufvk`. The different prefix
means a listings signature can never pass as an ownership proof, or a proof as a listings file,
and a proof in the old format cannot pass as a new one. Proofs in the old format (version 1,
which named no block) are refused with a request for a new proof.

## What the checker does

`check-proof`:

1. verifies the signature, and that the proof is for the expected network;
2. if the checker gave a code (`--challenge`), checks that the proof answers it;
3. asks its server for the block at `height` and checks that its hash is `block_hash`, so the
   block is on this chain, and that it is at most `--max-age` blocks below the tip;
4. scans the chain from the note's transaction with the viewing key in the proof: the note
   must decrypt with that key and must still be unspent.

It prints:

```json
{
  "valid": true,
  "held": true,
  "note": "<txid>:<index>",
  "name": "Item #1",
  "label": "@alice",
  "live": false,
  "block_height": 3520000,
  "block_hash": "<64 hex digits>",
  "age_blocks": 12,
  "max_age_blocks": 100,
  "scanned_to": 3520012
}
```

`valid` says the signature, network, code (if any) and block check out. `held` says the note
is still unspent and owned by the proof's key. `live` says the proof answered the checker's own
code. When either `valid` or `held` is false, a `reason` field says why and the command exits
with status 1. `name` is read from the note's transaction memo and may be `null`.

In the app, "Prove I own it" makes a shared proof by default, with an optional name and an
optional live-check code, and shows it as text, as a QR code and with a Share button. The
checker's sheet takes pasted text everywhere and reads the QR code with the camera in browsers
that have a barcode detector (Chrome, Edge, Samsung Internet).

## What it reveals

The proof carries the **item account's viewing key**. With it the checker can see every item
that account has held, now and in the past, and when each one moved. If you keep one item per
account (`new-item-account`), that is the history of this one item.

It reveals nothing about your ZEC: payments and change live in the payout account, whose key
is not in the proof. The proof carries no payout address. This holds when the item account is
separate from your payout account, which is the default (`new-item-account`); a proof made from
your payout account would expose that account's history.

The block it names is the last one your wallet scanned, about the current tip, which shows
roughly how up to date your wallet was when you made the proof. The label is whatever you
chose to write.

Like `verify`, `check-proof` asks the lightwalletd server for the note's transaction, so the
server learns which item is being checked; `check-proof --private` scans from the item
account's birthday instead and does not name the item. Either way it asks for the block the
proof names, a recent height that many wallets ask for.

## Limits

- **A shared proof is a bearer statement.** Whoever has it can show it again until it
  expires. It proves "the holder of this key holds this item", not "the person in front of you
  is that holder". A label helps: a copy still names the holder who signed it, so it cannot be
  passed off as someone else's.
- **A proof can be relayed.** Even a live check, with a fresh code, does not stop someone
  standing between you and the holder from passing your code to the real holder and forwarding
  the genuine proof back: the proof is bound to the code, not to who is asking or to the
  channel. Comparing the viewing key out of band, over a channel you already trust, is what
  closes this.
- **The window is a few blocks at best.** Freshness from a block cannot be finer than one block
  time, and holder and checker may see slightly different tips. Use a live check when the
  window must be zero.
- **A proof is good only while the item stays put.** Once the note is spent, by a sale, a
  `cancel` or any other move, `check-proof` reports `held: false`. A holder who moved the item
  to a new note of the same account must make a new proof naming the new note.
- A proof shows control of the item account's key. It does not say who the person is. If that
  matters, check the viewing key against one you already trust, such as the key in their
  signed listings file.
- The checker's view is only as fresh as its server: `scanned_to` is the last block it saw.
