# Threat model

| Threat | Outcome | Mitigation |
|---|---|---|
| Seller sells the item elsewhere after signing | Trade tx invalid (duplicate nullifier); buyer's ZEC untouched | consensus |
| Buyer withholds broadcast after seller signs | Seller's signature is a free option until expiry | expiry bounded by `max_expiry_delta` (default 40 blocks, the ecosystem default); the seller may cancel earlier by self-spending the item (one fee) |
| Buyer submits a transaction with no expiry, or one already expired | Perpetual option, or a signature on a dead transaction | policy refuses expiry 0 and expiry at or below the tip |
| Buyer alters the payment after seller signs | Impossible: ZIP 244 sighash commits every action's cv, nullifier, rk, cmx and ciphertext | protocol |
| Buyer forges an envelope without owning any note | Seller signs for nothing; items could be tied up | the anchor must be one of the seller wallet's own recent tree roots and the proof is verified against it, so a proof over a synthetic tree is refused; no offer locks an item in any case |
| Unredacted PCZT sent to seller | Seller learns buyer's full viewing key, witnesses and values | prove, then redact; the seller copy carries no buyer key material and no value trapdoors |
| Seller solves the buyer's values from the value commitments | Seller learns the paying note's value and the change | `rcv` is removed from the seller copy; `cv_net` alone is hiding |
| Seller asked to sign an extra spend | Theft of other seller notes | policy: exactly one seller-controlled spend, and it must be the listed item |
| Many buyers (or one buyer repeatedly) send trades for one item | Several signatures outstanding | all acceptable offers are signed; only one can confirm, the seller is paid once, nothing is locked |
| Tampered listings file | Buyer pays the wrong party or buys the wrong note | the file is signed by the item account's spend key and verified against the published viewing key; unsigned or altered files are refused. The signature proves the file was written by whoever controls these items, not that it is the seller you meant: compare the key fingerprint `list` prints with one the seller gave you out of band |
| Stale listings file served again | Buyer pays yesterday's higher price | the signed body carries the height it was issued at; `buy` warns when it is more than 1,000 blocks old |
| Price paid as dust | Seller receives notes it can never spend at a profit | outputs at or below the ZIP 317 marginal fee do not count as payment |
| Seller's own listings file edited on disk | The service would re-sign and enforce the edit | the service and `publish` refuse a file whose signature does not verify |
| Listings holder reads the buyer's new item note | Buyer's address, value and memo exposed on chain | the item output's outgoing key is the buyer's; the published consignment decrypts no trade output |
| Address reuse links purchases or links the market to a wallet | Buyer's or seller's purchases correlated | every item is received at a fresh diversified address; the payout address is derived for the market, never the default address |
| Published key reveals more accounts | Sapling notes and transparent addresses of the item account exposed | the listing carries the Orchard component only |
| Seller signs against a stale chain view | Expiry check and nullifier set out of date | the service syncs before every pass and refuses to run a pass whose sync failed |
| Service crash or restart mid-trade | Duplicate signature, lost envelope, torn listings file | replies are named by envelope content, outputs and listings are written atomically, the envelope is moved last, and an envelope whose reply already exists is filed without re-signing |
| Hostile inbox (huge files, unreadable files, name collisions) | Memory exhaustion, stalled service, dropped envelopes | envelopes above 1 MB are refused unread; one bad envelope is quarantined without failing the pass; replies are named by content tag |
| Light server down or flaky | Service stops | failed passes are logged and retried with exponential backoff up to five minutes; nothing is signed in a failed pass |
| Reorg or incomplete sync hides an item note | Item wrongly delisted | an item is removed only when the wallet has recorded a spend of its note |
| Network upgrade this build does not know | Transactions silently rejected | the tools compare the server's consensus branch with their own before building or sending and stop with a message |
| On-chain fingerprinting of trades | Trades identifiable without keys | standard expiry delta, standard ZIP 317 fees, standard anchor depth, two or three actions like any wallet transaction |
| Window-shoppers hold the item FVK | They learn when and that the item sold | inherent to consignment; documented |
| Fee or change manipulation | Seller underpaid | policy decrypts the payment outputs under the seller's incoming viewing key and checks `>= price` |
| Envelope for another consensus branch | Signature on a transaction the chain cannot accept | policy compares the envelope's branch id with the one live at the next block |

## What stays visible
- The listed price and payout address, by the seller's choice.
- To anyone holding a consignment: the item notes of that account, their values and names, and the block in which each one was spent.
- To the light server: the IP that syncs an imported viewing key, broadcasts a trade or fetches an item's genesis transaction. Route through Tor or run your own lightwalletd if that matters.
- To the seller: the nullifier of the buyer's paying note, from the envelope. If the offer settles, the chain reveals it anyway. If the offer does not settle (lost race, expiry, abandoned), the seller can later recognise the transaction in which that note is finally spent. `buy` says so; spend the note to yourself before reusing it if that matters. With an exact-value two-action offer the seller also learns that note's value (price plus fee).
- Nothing else links the buyer's paying note, the buyer's address, the seller's other notes or either party's balance to the trade.
