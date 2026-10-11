# Changelog

## Unreleased

- Two-party atomic settlement core: one Ironwood transaction with spends under two full viewing keys, proven by the buyer, signed per party, extracted and broadcast.
- Buyer CLI: `list`, `sync`, `import-listing`, `buy`, `finish`, `broadcast`, `verify`.
- Seller service: `sync`, `mint`, `items`, `publish`, `sign`, `serve`.
- Seller policy with tests: expiry cap, single item spend, no extra seller spends, payment check.
- Seller service hardening: built-in wallet sync before every pass, chain reconciliation of the listings (sold items removed, expired offers relisted), one offer per item, atomic writes, idempotent restart, retry with backoff.
- Resale: items can be received into a dedicated item account and relisted; `verify` follows an item across holders when given each holder's listings.
- Keyless provenance verification from compact blocks.
