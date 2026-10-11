# Changelog

## Unreleased

- Two-party atomic settlement core: one Ironwood transaction with spends under two full viewing keys, proven by the buyer, signed per party, extracted and broadcast.
- Buyer CLI: `list`, `sync`, `import-listing`, `buy`, `finish`, `broadcast`, `verify`.
- Seller service: `sync`, `mint`, `items`, `publish`, `sign`, `serve`.
- Seller policy with tests: expiry bounds (set, after the tip, within the limit), single item spend, no extra seller spends, payment check.
- Seller service hardening: built-in wallet sync before every pass, listings reconciled with the wallet's recorded spends, proof verification before signing, every acceptable offer signed (no lock to grief), replies named by envelope content, size cap and per-envelope quarantine, atomic writes, idempotent restart, retry with backoff.
- Privacy: the new item note is encrypted to the buyer's outgoing key and sent to a fresh diversified address; the payout address is derived for the market; listings publish the Orchard viewing key only; the seller copy carries no value trapdoors; expiry and fees are the ecosystem defaults.
- Signed listings: buyers verify the file against the published viewing key.
- Resale: items can be received into a dedicated item account and relisted; `verify` follows an item across holders when given each holder's listings.
- Consensus guard: build, mint and broadcast stop when the server is on a branch this build does not sign for.
- Provenance verification from compact blocks with viewing keys only, starting at the genesis block; `--private` scans from the birthday instead.
- Second review pass: anchor must be a recent wallet root, dust outputs do not count as payment, envelope branch id checked, zero randomizers refused, account index checked against the account key, seller-side listings verified before re-signing, issued-at height in the signed listings, lineage-broken status in verify, examples for a wallet-free run.
