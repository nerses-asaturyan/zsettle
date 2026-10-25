# Testnet (NU7)

zsettle runs on the Zcash testnet, which activated NU7 at height 4,465,026 on 4 October 2026, with the same build as mainnet: the released NU7 crate line (`zcash_client_backend` 0.25 and its companions). The default light server follows the wallet's network (`https://testnet.zec.rocks:443` for testnet, `https://zec.rocks:443` for mainnet).

Keep testnet and mainnet wallets in separate directories.

## Wallets

```
zsettle-seller init --network test --wallet ~/.zsettle-testnet/seller --identity ~/.zsettle-testnet/seller.key
zsettle        init --network test --wallet ~/.zsettle-testnet/buyer  --identity ~/.zsettle-testnet/buyer.key
```

`init` makes the same two accounts as on mainnet (see [SETUP.md](SETUP.md)) and prints an address to fund. A wallet made by an older build is brought up to date by its first `sync`; a migration that needs the seed is applied with `zsettle upgrade --wallet <dir> --identity <key>`.

## Funds

Testnet coins (TAZ) are free and worthless. Since the NU7 activation the one faucet verified to pay is Valar's: `https://faucet.testnet.valargroup.dev`, 0.125 TAZ per address and per IP every 24 hours, claimed on the page or with one request:

```
curl -H 'Content-Type: application/json' -d '{"address":"utest1…"}' https://faucet.testnet.valargroup.dev/api/claim
```

Take the address `init` printed, or a fresh one from `zsettle address --wallet ~/.zsettle-testnet/buyer --account <BUYER_UUID>`. One claim (12,500,000 zat) funds both wallets: the buyer pays the seller with `zsettle pay --wallet ~/.zsettle-testnet/buyer --account <BUYER_UUID> --identity ~/.zsettle-testnet/buyer.key --to <SELLER_ADDRESS> --amount 4000000`. A note received from another party is spendable after 10 confirmations (about four minutes on testnet), the wallet's own change after 3.

`zsettle tx --network test --txid <id>` asks the light server whether a transaction is mined, in the mempool or unknown.

## Flow

The same as [TRADE.md](TRADE.md) with the testnet wallet directories. Every command picks `testnet.zec.rocks` from the wallet's network; `--server` still overrides it. Blocks arrive every 25 seconds on testnet since NU7, so a trade's 120-block expiry is about 50 minutes and three confirmations about 75 seconds.

## What the NU7 line changed

- Dependencies: the 0.25 line, `rand` 0.10 (`rand::rng()` wherever a role needs randomness), `pasta_curves` 0.6.
- `Wallet::db` runs the wallet schema migrations; new `upgrade` command on both binaries.
- `pick_server`: the default light server follows the wallet's network.
- `pay` (plain shielded payment, shares the mint path) and `tx` (transaction status) on the buyer CLI.
- `COMPACT_NOTE_SIZE` moved to `orchard::note_encryption`; `UnifiedAddress::from_receivers` and the unified key encoder took new parameters; the signer, prover, IO finalizer and transaction extractor take an RNG.

The release of 21 October 2026 (`zcash_protocol` 0.11) set the mainnet NU7 activation height to 3,543,405. Since then `main` builds on the released crates, and the `nu7-testnet` branch is the same.

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
