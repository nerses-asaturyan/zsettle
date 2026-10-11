# zsettle protocol (draft)

## Roles
- **Seller**: holds the item account spending key. Publishes a listing. Signs only the item action.
- **Buyer**: holds ZEC. Receives the item account's full viewing key by consignment, builds, proves, redacts, signs the payment action, extracts and broadcasts.
- **Anyone**: verifies provenance from chain data, no keys needed.

## Trade flow
1. Seller publishes `Listing { item, price_zat, max_expiry_delta, seller_address, endpoint }`.
2. Buyer syncs the Ironwood pool for its own account and the item account (viewing key only).
3. Buyer builds a PCZT: spend(item note, item FVK) -> output(item note, buyer address, same value); spend(ZEC note, buyer FVK) -> output(price, seller address) [+ change]. Both witnesses at one anchor. Expiry = tip + `max_expiry_delta`.
4. Buyer runs IO Finalizer and Prover, then **redacts** (removes buyer FVK, witnesses, rseeds, memos not needed by the seller).
5. Seller runs the policy checks (`zsettle-core::policy`) and signs action A only.
6. Buyer signs action B, combines, finalizes spends, extracts the transaction, broadcasts.
7. Anyone runs `verify`: successor note `rho == nf(previous)`, `cmx` recomputed with the v3 `rcm`, last hop unspent.

## Why it is atomic
Both spends live in one transaction authorized by one binding signature. A transaction with any unsigned action is invalid. A prior spend of the item note makes the trade's nullifier a duplicate, so the whole transaction is rejected by consensus.

## Items
- Ordinary Ironwood v3 notes, value >= 1000 zat (zero-value notes are not Merkle-bound).
- Genesis anchor and manifest follow the nscv byte layout where practical; consignment = per-hop full viewing key.
- Item transfer MUST happen in the same action as the item spend.

## Open items
- Exact redaction set with `zcash_client_backend` 0.24 helpers.
- Expiry handling when the chain tip moves between seller signature and broadcast.
- Change action and fee when the buyer has no exact-value note (3 actions, 15 000 zat).
