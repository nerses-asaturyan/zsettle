# Threat model (draft)

| Threat | Outcome | Mitigation |
|---|---|---|
| Seller sells the item elsewhere after signing | Trade tx invalid (duplicate nullifier); buyer's ZEC untouched | consensus |
| Buyer withholds broadcast after seller signs | Seller's signature is a free option until expiry | `max_expiry_delta` <= 10 blocks; seller may cancel by self-spending the item (one fee) |
| Buyer alters the payment after seller signs | Impossible: ZIP 244 sighash commits every action's cv, nullifier, rk, cmx and ciphertext | protocol |
| Unredacted PCZT sent to seller | Seller learns buyer's full viewing key | prove, then redact; seller refuses unredacted envelopes |
| Seller asked to sign an extra spend | Theft of other seller notes | policy: exactly one seller-controlled spend, and it must be the listed item |
| Window-shoppers hold the item FVK | They learn when and that the item sold | inherent to consignment; documented |
| Fee or change manipulation | Seller underpaid | policy decrypts the payment output and checks `>= price` |
| Expiry far in the future | Long free option | policy rejects expiry beyond `max_expiry_delta` |
