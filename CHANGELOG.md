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
- Hardening pass: transparent addresses never reach the light server (`Shielded` channel, tested); `cancel` ends a listing for good, `publish --remove` delists, `consolidate` merges small notes; the service defers offers whose anchor or expiry it cannot judge yet; `rotate` syncs first and keeps an offer only while the next block can still mine it; the inbox ignores symlinks; warnings for an index-0 item account, a multi-item consignment and a resale without `--receive-account`; wording on what the listings file and the light server learn.
- `init`, `accounts` and `address` on both binaries: wallets are created by the tools themselves (mnemonic, age identity, database, accounts), so zcash-devtool is no longer needed. Without `--identity` the mnemonic is protected by a passphrase instead of a key file. Logs are quiet unless `ZSETTLE_LOG` is set.
- Third review pass: `publish` verifies the file before signing it again unless `--resign-unverified` is passed, and checks every entry; a spend with unknown expiry counts for 40 blocks after it was first seen, as in the wallet library; an envelope the wallet cannot judge yet is deferred instead of rejected; quarantined files never replace earlier ones; a file dated in the future no longer waits forever; `--expiry-delta` is capped at 40; one server round trip per consensus check; a zero spend randomizer is covered by a test.
- `rotate` retires the paying notes of offers that did not settle; `new-item-account` gives an item its own consignment.
- Second review pass: anchor must be a recent wallet root, dust outputs do not count as payment, envelope branch id checked, zero randomizers refused, account index checked against the account key, seller-side listings verified before re-signing, issued-at height in the signed listings, lineage-broken status in verify, examples for a wallet-free run.
