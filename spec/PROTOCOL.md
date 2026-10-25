# zsettle protocol

## Roles
- **Seller**: holds the item account spending key. Publishes signed listings. Verifies each offer's proof, checks it against policy, signs only the item action. Signs every acceptable offer; the chain settles one.
- **Buyer**: holds ZEC. Receives the item account's Orchard full viewing key by consignment (inside the listings file), builds, proves, redacts, signs the payment action, extracts and broadcasts.
- **Anyone**: verifies provenance from chain data with consignments only, no spending key.

## Items
- Ordinary Ironwood v3 notes in a dedicated ZIP 32 account, value >= 1,000 zat (a zero-value note is not bound to the commitment tree by the circuit).
- Minted by paying the item account from the seller's payout account at a fresh diversified address; the memo `zsettle item: <name>` names the item. That note is the item's genesis.
- Consignment = the item account's Orchard full viewing key plus the account birthday. It lets a holder see every item note of that account and when each one is spent, and nothing else; for an item that should not be seen together with others, give it an account of its own (`new-item-account`). The rest holds: the Sapling and transparent components of the account are never published, and no output of a trade is encrypted to this key.
- Item transfer MUST happen in the same action as the item spend, so the successor note is always the output of the action that spends the predecessor, and its `rho` is the predecessor's nullifier. The Orchard builder pairs spends and outputs at random, so the trade builder rebuilds the bundle with fresh randomness until the item spend and the item output share an action (one chance in two or three per attempt; proving happens once, afterwards). `verify` relies on this rule and on nothing else: the lineage is the chain of spending actions. The buyer's wallet enforces the pairing while it builds the trade; the seller does not and cannot check it, because the output's `rho` is not public in the redacted envelope. A buyer who builds a trade with a custom tool and skips the pairing still pays the seller the listed price, and only breaks its own item's future provenance in `verify`.

## Listings
A JSON file the seller publishes anywhere: `{ version, network, item_ufvk, item_birthday, payout_address, items: [{ note, name, price_zat, max_expiry_delta, description?, icon? }], issued_at, signature }`.

- `item_ufvk` is a unified viewing key with the Orchard component only.
- `payout_address` is a diversified address derived for the market, never the account's default address.
- `price_zat` is above 5,000: a smaller payment is dust that wallets refuse to spend.
- `description` (one line of plain text, at most 280 characters) and `icon` (one emoji) are optional and signed like the rest.
- `issued_at` is the chain height at which this version was signed; buyers warn on files more than 1,000 blocks old.
- `signature` is a RedPallas signature over the other fields (JSON in fixed field order, prefixed with `zsettle-listings-v1\n`) by the item account's spend authorizing key with randomizer zero, so it verifies under the spend validating key `ak` that opens the published viewing key. Buyers refuse unsigned or altered files. The seller service re-signs the file whenever it changes it.

## Trade flow
1. Seller publishes the signed listings.
2. Buyer verifies the signature, imports `item_ufvk` as a view-only account and syncs both accounts.
3. Buyer asks the light server which consensus branch is live and refuses to build on a branch this build does not sign for.
4. Buyer builds a PCZT: spend(item note, item FVK) -> output(item note, a fresh diversified address of the buyer's receiving account, same value, memo = item name, outgoing key = the buyer's); spend(ZEC note, buyer FVK) -> output(price, payout address) [+ change]. Both witnesses at one anchor. Expiry = the wallet library's standard expiry from the next block, within `max_expiry_delta` (see Expiry); fee = the conventional ZIP 317 amount.
5. Buyer runs the IO finalizer and the prover with the two full viewing keys, then redacts the seller's copy: the buyer's FVK, witnesses, note randomness, randomizers, value-commitment trapdoors and output secrets are removed; the sighash-relevant fields, the proof and the item action's own spend data stay. The buyer records the paying note as reserved; it stays reserved until `rotate` moves or releases it (see Offers).
6. Seller syncs, matches the item by nullifier and runs the public checks (`zsettle-core::policy::check_public`: the envelope's consensus branch is the live one; the bundle's anchor is one of the seller wallet's own tree roots from the last 100 blocks; expiry set, after the tip and within the limit; exactly one spend of the listed note with a non-zero randomizer), then verifies the proof against the bundle's public inputs, then runs the two checks that use its own keys (`check`: no other note of the listed item account spent; payment outputs to the listed payout address, counting only those above the ZIP 317 marginal fee, sum to at least the price), and signs action A only. Every decision depends only on public data and the proof, so a refusal reveals nothing a listings holder could not compute. The anchor rule matters: a proof only shows membership in the tree whose root it was made for, so without it a synthetic tree would pass.
7. Buyer combines (conflicting fields fail the merge), signs action B, finalizes spends, extracts the transaction (proof and both signatures are verified here), checks the server's branch again and broadcasts.
8. Anyone runs `verify` with the consignments: the server says which block holds the genesis transaction, streaming starts there, the genesis note is decrypted, its nullifier derived, the spending action located, its output taken as the successor, repeated while a supplied consignment decrypts the successor.

## Why it is atomic
Both spends live in one transaction authorized by one binding signature. A transaction with any unsigned action is invalid. A prior spend of the item note makes the trade's nullifier a duplicate, so the whole transaction is rejected by consensus.

## Offers
The seller signs every offer that passes. Several signed offers for one item may exist at once; all spend the same nullifier, so at most one is ever mined and the seller is paid exactly once. A buyer who loses the race loses nothing but the time spent proving. Nothing is locked, so nobody can keep an item off the market by sending offers. The seller keeps the envelope of a lost offer, which holds the nullifier of the buyer's paying note; `rotate` moves such notes to the buyer's account's change address once their offers can no longer be mined, so that nullifier never identifies a later payment.

## Expiry
The buyer uses the wallet library's standard expiry, counted from the next block: 40 blocks before NU7 and 120 from NU7 (ZIP 203, ZIP 218), about 50 minutes either way, and never more even if the listing allows more. A listing's `max_expiry_delta` is stated in 75-second reference blocks (default 40) and converted at the tip, so the default limit equals that standard expiry; the seller sees the chain tip, so it allows one block of slack beyond the converted limit. An expiry of zero (never expires) or at or below the tip is refused. After the expiry height the seller's signature is worthless. Because the expiry is the library default, a trade carries no distinguishing expiry; the seller can end the option early by spending the item itself.

## Resale
A buyer who wants to resell receives the item into its own item account (`buy --receive-account`), then publishes listings from that wallet exactly as the first seller did. Provenance across resales is verified by supplying every holder's listings file; the trace stops at the first successor no supplied consignment can open.

## Fees
ZIP 317: 5,000 zat per logical action, minimum two. A trade with change is three actions (15,000 zat); with an exact paying note it is two (10,000 zat). The buyer pays the fee. Change that would be dust is refused rather than created.

## Network upgrades
The consensus branch id is part of the sighash. Before building, minting or broadcasting, the tools compare the branch the server reports with the one this build signs for at the target height and stop with a clear message when they differ, instead of producing a transaction the network would reject.
