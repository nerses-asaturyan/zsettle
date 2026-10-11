# Testnet (NU7) build

This branch builds against the NU7 pre-release crate line (`zcash_client_backend` 0.25.0-pre.1 and its exact companions), so zsettle runs on the Zcash testnet, which activated NU7 at height 4,465,026 on 4 October 2026. `main` serves mainnet (NU6.3) with the released crates. The two branches differ only in dependencies, the RNG plumbing those crates require, a schema migration when a wallet is opened, the default light server, which follows the wallet's network (`https://testnet.zec.rocks:443` for testnet, `https://zec.rocks:443` for mainnet), and two helper commands (`pay`, `tx`).

Do not open a mainnet wallet with this build: it migrates the wallet database to the newer schema, which the `main` build cannot read. Keep testnet wallets in their own directory.

## Wallets

```
D=~/code/tools/zcash-devtool/target/release/zcash-devtool
$D wallet -w ~/.zsettle-testnet/seller init --name seller -i ~/.zsettle-testnet/seller/key.txt -n test -s zecrocks
$D wallet -w ~/.zsettle-testnet/seller generate-account --name item -i ~/.zsettle-testnet/seller/key.txt -s zecrocks
$D wallet -w ~/.zsettle-testnet/buyer  init --name buyer  -i ~/.zsettle-testnet/buyer/key.txt  -n test -s zecrocks
$D wallet -w ~/.zsettle-testnet/buyer  generate-account --name item -i ~/.zsettle-testnet/buyer/key.txt -s zecrocks
zsettle sync --wallet ~/.zsettle-testnet/buyer
zsettle-seller sync --wallet ~/.zsettle-testnet/seller
```

Devtool only creates the wallet files, which needs no NU7 support. The first `sync` with this build migrates the database and scans from the birthday; a migration that needs the seed is applied with `zsettle upgrade --wallet <dir> --identity <key>`.

## Funds

Testnet coins (TAZ) are free and worthless. Since the NU7 activation the one faucet verified to pay is Valar's: `https://faucet.testnet.valargroup.dev`, 0.125 TAZ per address and per IP every 24 hours, claimed on the page or with one request:

```
curl -H 'Content-Type: application/json' -d '{"address":"utest1…"}' https://faucet.testnet.valargroup.dev/api/claim
```

Take the buyer account's default address from `$D wallet -w ~/.zsettle-testnet/buyer list-addresses`. One claim (12,500,000 zat) funds both wallets: the buyer pays the seller with `zsettle pay --wallet ~/.zsettle-testnet/buyer --account <BUYER_UUID> --identity ~/.zsettle-testnet/buyer/key.txt --to <SELLER_ADDRESS> --amount 4000000`. A note received from another party is spendable after 10 confirmations (about four minutes on testnet), the wallet's own change after 3.

`zsettle tx --network test --txid <id>` asks the light server whether a transaction is mined, in the mempool or unknown.

## Flow

The same as [TRADE.md](TRADE.md) with the testnet wallet directories. Every command picks `testnet.zec.rocks` from the wallet's network; `--server` still overrides it. Blocks arrive every 25 seconds on testnet since NU7, so a 40-block expiry is about 17 minutes and three confirmations about 75 seconds.

## What changed against `main`

- Dependencies: the 0.25-pre line, `rand` 0.10 (`rand::rng()` wherever a role needs randomness), `pasta_curves` 0.6.
- `Wallet::db` runs the wallet schema migrations; new `upgrade` command on both binaries.
- `pick_server`: the default light server follows the wallet's network.
- `pay` (plain shielded payment, shares the mint path) and `tx` (transaction status) on the buyer CLI.
- `COMPACT_NOTE_SIZE` moved to `orchard::note_encryption`; `UnifiedAddress::from_receivers` and the unified key encoder took new parameters; the signer, prover, IO finalizer and transaction extractor take an RNG.

When a librustzcash release carries the NU7 mainnet activation height, this branch becomes `main` by changing the `-pre` versions to the release.

## Testnet run (11 October 2026)

One faucet claim funded everything below; the whole run, confirmation waits included, took twenty minutes.

| Txid | Height | What |
|---|---|---|
| `ca40d21fda31baf8f15b3245323db3229e81eb50f55f79c8b1c7dffe98f6a234` | 4489715 | buyer funds the seller with 0.04 TAZ (`zsettle pay`) |
| `9b8c8d85e0db3230acb70537bbade417d6c431ef1c6f4c5bd7949ff26867b3a4` | 4489728 | mint "testnet item #1" |
| `03c7f211b988f0bae80bcac9f015cf69649b8bc1f1657355c883a71fc104dbf3` | 4489733 | mint "testnet item #2" |
| `787af5f252429c88cfb3a4575f1d91d6fd45d7cb8686411038b7116b881cf94c` | 4489740 | trade: item #1 to the buyer's item account, 100,000 zat to the seller; the seller service verified the proof and signed |
| `689a99860891565660b991edcf50416bac339410687a940ac4f9b6f09811cb0b` | 4489761 | resale: the seller buys it back for 150,000 zat; the buyer's wallet ran `serve` |

`zsettle verify --listings market/listings.json --listings market2/listings.json --item 9b8c8d85…:0` reports three hops, genesis to buyer to seller, every one decrypted by one of the two signed listings, and the item held by the seller's key again.
