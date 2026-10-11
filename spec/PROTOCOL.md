# zsettle protocol

## Roles
- **Seller**: holds the item account spending key. Publishes listings. Signs only the item action, one outstanding offer per item.
- **Buyer**: holds ZEC. Receives the item account's full viewing key by consignment (inside the listings file), builds, proves, redacts, signs the payment action, extracts and broadcasts.
- **Anyone**: verifies provenance from chain data with consignments only, no spending key.

## Items
- Ordinary Ironwood v3 notes in a dedicated ZIP 32 account, value >= 1,000 zat (a zero-value note is not bound to the commitment tree by the circuit).
- Minted by paying the item account from the seller's payout account; the memo `zsettle item: <name>` names the item. That note is the item's genesis.
- Consignment = the item account's full viewing key plus the account birthday. It lets a holder of it see every item note of that account and when each one is spent, nothing else.
- Item transfer MUST happen in the same action as the item spend, so the successor note is always the output of the action that spends the predecessor, and its `rho` is the predecessor's nullifier. The Orchard builder pairs spends and outputs at random, so the trade builder rebuilds the bundle with fresh randomness until the item spend and the item output share an action (one chance in two or three per attempt; proving happens once, afterwards). `verify` relies on this rule and on nothing else: the lineage is the chain of spending actions.

## Listings
A JSON file the seller publishes anywhere: `{ version, network, item_ufvk, item_birthday, payout_address, items: [{ note, name, price_zat, max_expiry_delta, pending? }] }`. `pending = { expiry_height, trade }` marks an item under offer: the seller has signed a trade for it that can still be mined up to `expiry_height`. The service clears the mark once that height is scanned with the note unspent, and removes the item once the note is spent.

## Trade flow
1. Seller publishes the listings.
2. Buyer imports `item_ufvk` as a view-only account and syncs both accounts.
3. Buyer builds a PCZT: spend(item note, item FVK) -> output(item note, buyer address, same value); spend(ZEC note, buyer FVK) -> output(price, payout address) [+ change]. Both witnesses at one anchor. Expiry = next block + `max_expiry_delta`.
4. Buyer runs the IO finalizer and the prover with the two full viewing keys, then redacts the seller's copy: the buyer's FVK, witnesses, note randomness, randomizers and output secrets are removed; the sighash-relevant fields and the item action's own spend data stay.
5. Seller syncs, runs the policy checks (`zsettle-core::policy`) and signs action A only. The item is marked under offer.
6. Buyer combines, signs action B, finalizes spends, extracts the transaction (proof and both signatures are verified here) and broadcasts.
7. Anyone runs `verify` with the consignments: genesis note found by trial decryption, its nullifier derived, the spending action located, its output taken as the successor, repeated while a supplied consignment decrypts the successor.

## Why it is atomic
Both spends live in one transaction authorized by one binding signature. A transaction with any unsigned action is invalid. A prior spend of the item note makes the trade's nullifier a duplicate, so the whole transaction is rejected by consensus.

## Expiry
The buyer counts from the next block; the seller sees the chain tip, so the seller allows one block of slack beyond `max_expiry_delta`. After the expiry height the seller's signature is worthless and the item goes back on sale without any action from the seller.

## Resale
A buyer who wants to resell receives the item into its own item account (`buy --receive-account`), then publishes listings from that wallet exactly as the first seller did. Provenance across resales is verified by supplying every holder's listings file; the trace stops at the first successor no supplied consignment can open.

## Fees
ZIP 317: 5,000 zat per logical action, minimum two. A trade with change is three actions (15,000 zat); with an exact paying note it is two (10,000 zat). The buyer pays the fee.
