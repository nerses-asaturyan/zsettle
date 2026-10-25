# Networks and NU7

zsettle is one code line on the released librustzcash crates of the NU7 line
(`zcash_client_backend` 0.25, `zcash_client_sqlite` 0.23, `pczt` 0.10, `orchard` 0.16, ...,
published on 21 October 2026). The same build works on both networks:

| Network | Network upgrade | Default light server |
|---|---|---|
| mainnet | NU6.3 until block 3,543,405, NU7 from that block (about 5 November 2026) | `https://zec.rocks:443` |
| testnet | NU7 since block 4,465,026 (4 October 2026) | `https://testnet.zec.rocks:443` |

The light server follows the wallet's network; `--server` overrides it. The build signs for
whichever consensus branch is live at the next block, so a mainnet wallet keeps working across
the NU7 activation without a new build.

The `nu7-testnet` branch is kept identical to `main`. Before the release, it carried the NU7
pre-release crates for the testnet while `main` stayed on the NU6.3 line.

## Block spacing and expiry

NU7 shortens the target block spacing from 75 to 25 seconds. Durations meant to stay the same
in time are counted in 75-second *reference blocks* and converted at the current height:

- A trade's expiry is the wallet library's standard expiry: 40 blocks before NU7 and 120 from
  NU7 (ZIP 203, ZIP 218), about 50 minutes either way. A listing's `max_expiry_delta` (default
  40) is in reference blocks, so the default limit is exactly that standard expiry on both
  sides of the activation, and a trade carries no distinguishing expiry.
- An ownership proof is accepted for about 50 minutes by default: 40 blocks before NU7, 120
  from NU7.
- Confirmations are counted in blocks and do not scale. The command-line tools and the local
  app want 10 for a note from another wallet and 3 for the wallet's own change: on a 25-second
  chain about four minutes and 75 seconds. The wallet in the browser spends from the first
  block.

## Wallet databases

Opening a wallet migrates its database to the schema of the crates in use; an `upgrade`
command on both binaries applies the migrations that need the seed
(`upgrade --wallet <dir> --identity <key>`). A wallet opened by this build cannot be read by an
older build of the NU6.3 line. Keep testnet and mainnet wallets in separate directories.

## The consensus-branch guard

The consensus branch id is part of the signature hash. Before building, minting or
broadcasting, the tools ask the light server which branch is live and compare it with the
branch this build signs for at the next block. When they differ, for example after a network
upgrade this build does not know, the tools stop with a message instead of making a transaction
the network would reject.
