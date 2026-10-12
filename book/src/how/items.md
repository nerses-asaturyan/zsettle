# Items as notes

An item is an ordinary Ironwood note in a dedicated ZIP 32 account of the seller's wallet: the
*item account*. There is no token standard and no registry. The note itself is the item.

## Minting

`zsettle-seller mint` (code: `mint` in `crates/zsettle-core/src/mint.rs`) makes one
transaction, paid by the payout account, with one output per item:

- each output goes to a fresh diversified address of the item account;
- its value is `--value` (default 1,000 zatoshis, which is also the minimum);
- its memo is `zsettle item: <name>`.

Each new note is that item's **genesis note**. Its reference `<txid>:<action index>` names the
item from then on.

## Why an item note must carry value

A zero-value note is not bound to the commitment tree by the Action circuit, so nothing would
tie its successor to it. The minimum of 1,000 zatoshis is checked at mint time
(`crates/zsettle-core/src/item.rs`) and again when a trade is built.

## What makes an item unique

- A note can be spent only once: spending it reveals its nullifier, and consensus rejects a
  second spend of the same nullifier.
- In a trade, the item's new note is the output of the same action that spends the old one.
  Its `rho` is therefore the old note's nullifier. Following "the action that spends this note,
  and its output" from the genesis note gives the item's whole history, see
  [Item history](history.md).

The value stays the same along the chain. A spend whose successor has a different value is
not a trade, and `verify` reports it as `lineage_broken`.

## What the memo holds

Only the name: `zsettle item: <name>`. In a trade, the buyer's new item note also gets the memo
`zsettle item: <name>` when the listing names the item. The memo is encrypted like any memo;
only holders of the right viewing key can read it.

## The consignment

To buy an item, the buyer needs to see the item note and build a spend of it. The seller
gives out the item account's Orchard full viewing key, in the listings file. This is called
the *consignment*. With it, a holder sees:

- every note of that account, with its value and name;
- when each note is spent.

Nothing else: the Sapling and transparent parts of the account are never published, and no
output of a trade is encrypted to this key. For an item that should not be seen together with
others, give it an account of its own with `zsettle-seller new-item-account`.

## Cancelling

`zsettle-seller cancel` spends the item note to a fresh address of the same item account,
together with one payout note that pays the fee. Every signed offer for the old note becomes
invalid. The item continues as the new note.
