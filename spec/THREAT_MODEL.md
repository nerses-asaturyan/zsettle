# Threat model

| Threat | Outcome | Mitigation |
|---|---|---|
| Seller sells the item elsewhere after signing | Trade tx invalid (duplicate nullifier); buyer's ZEC untouched | consensus |
| Buyer withholds broadcast after seller signs | Seller's signature is a free option until expiry | `max_expiry_delta` <= 10 blocks; the item is relisted automatically after expiry; the seller may cancel earlier by self-spending the item (one fee) |
| Buyer alters the payment after seller signs | Impossible: ZIP 244 sighash commits every action's cv, nullifier, rk, cmx and ciphertext | protocol |
| Unredacted PCZT sent to seller | Seller learns buyer's full viewing key | prove, then redact; the seller copy carries no buyer key material |
| Seller asked to sign an extra spend | Theft of other seller notes | policy: exactly one seller-controlled spend, and it must be the listed item |
| Two buyers (or one buyer twice) send trades for one item | Two signatures outstanding; only one can confirm, but the seller has promised the item twice | one offer at a time: an item under offer is refused until its expiry height has been scanned |
| Seller signs against a stale chain view | Expiry check and nullifier set out of date | the service syncs before every pass and refuses to run a pass whose sync failed |
| Service crash or restart mid-trade | Duplicate signature, lost envelope, torn listings file | envelopes are moved only after the signed copy and the listings are written; outputs and listings are written atomically; an envelope whose signed copy already exists is filed as done without re-signing |
| Light server down or flaky | Service stops | failed passes are logged and retried with exponential backoff up to five minutes; nothing is signed in a failed pass |
| Window-shoppers hold the item FVK | They learn when and that the item sold | inherent to consignment; documented |
| Fee or change manipulation | Seller underpaid | policy decrypts the payment outputs under the seller's incoming viewing key and checks `>= price` |
| Expiry far in the future | Long free option | policy rejects expiry beyond `max_expiry_delta` (+1 block of slack) |
| Resale exposes the buyer's funds | Publishing a consignment reveals every note of that account | items are received into a dedicated item account (`buy --receive-account`), never the paying account |

## What stays visible
- The listed price and payout address, by the seller's choice.
- To anyone holding a consignment: the item notes of that account, their values, and the block in which each one was spent.
- To the seller, as the sender of the item output: the buyer's receiving address (a diversified address).
- Nothing links the buyer's paying note, the seller's other notes or either party's balance to the trade.
