# Item history

`zsettle verify` traces an item from chain data, with viewing keys only. It needs no wallet
and no spending key: just the listings files of the holders the item went through. Code:
`crates/zsettle-core/src/verify.rs`.

## The trace

1. **Find the start.** Unless `--private` is given, `verify` asks the light server which block
   holds the transaction of the `--item` note, and starts streaming compact blocks there. With
   `--private`, it starts at the earliest birthday of the given listings.
2. **Find the note.** It trial-decrypts Ironwood actions with each listing's viewing key until
   it finds the starting note.
3. **Follow the spend.** It derives the note's nullifier with the key that decrypted it, and
   streams on until an action spends that nullifier. By the rule that the item spend and the
   item output share an action, that action's output commitment is the item's next note.
4. **Continue.** When another supplied listing decrypts the next note, the trace goes on from
   there. It ends at an unspent note one of the keys owns, or at a note none of the keys can
   open.

The item name is read from the memo of the genesis transaction, with the key that owns the
genesis note. With `--private` the name is not read.

`verify` reads its block stream to the tip even after the trace is done, so the server does not
learn where the item last moved.

## The result

```
{
  "name": "Item #3",
  "genesis": "<txid>:<index>",
  "keys": ["listings.json", "mine.json"],
  "hops": [
    { "txid": "...", "height": 3514109, "cmx": "...", "value_zat": 1000, "holder": "listings.json" }
  ],
  "status": { "state": "held", "by": "mine.json" },
  "scanned_to": 3514700
}
```

`status.state` is one of:

| State | Meaning |
|---|---|
| `held` | The last note is unspent and the listings file named in `by` owns it. |
| `transferred_to_unknown_holder` | The last note was spent into a note none of the given keys can decrypt: sold to someone whose listings were not given, or spent outside the protocol. |
| `lineage_broken` | The last note was spent by a transaction (`txid`) whose next note has a different value. A trade never does that. |

The values in the example above are made up to show the shape; the labels are the
`--listings` arguments as given.

## Limits

- The trace can only follow holders whose listings you supply.
- A trade built before the pairing rule existed may have its item output in a different action.
  Then the trace reports a transfer to an unknown holder at that point. One of the early
  mainnet trades is like this, see [Project status](../reference/status.md).
- The light server sees the requests. See [Privacy](../privacy.md#the-light-server).
