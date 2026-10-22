# Use the wallet in your browser

A server that runs `zsettle-app --web-wallet` also serves a wallet that runs inside the page,
at `/wallet/`. It is the same wallet code as the command-line tools, compiled to WebAssembly.
The keys stay on your device; the server never holds them.

On the hosted demo it is at `https://<demo domain>/wallet/`, and the start page links to it.

## What stays where

| What | Where |
|---|---|
| The 24-word recovery phrase | In this browser, encrypted. It is made in the page and never sent anywhere. |
| The wallet database (notes, history) | In this browser (IndexedDB), encrypted with the same key. |
| Proving and signing | In the page, in a background worker with a thread pool. |
| Chain data | Fetched through the server's relay at `/api/chain/`: blocks, tree states, the chain tip, and finished transactions to send. These are the requests any light wallet makes. |
| Offers to a shop | Posted to the shop on the same server (`/shop/<code>/offer`), as the command-line `buy` posts them. |

The key that encrypts the phrase and the database comes from a passkey (its PRF output) or
from a passphrase (PBKDF2, 600,000 rounds). A passkey wallet asks for the passkey again
before every payment, purchase, proof and backup; a passphrase wallet asks for the passphrase
again before showing the recovery phrase. An open wallet locks itself after 15 minutes
without a touch or a key.

## Create or restore

1. Open `/wallet/` and choose **Create a wallet**. Pick the network: testnet (free test coins)
   or mainnet (real ZEC).
2. Write the 24 words down. They are the only way back if the browser loses its data.
3. Lock it with a passkey, or with a passphrase of 12 characters or more.

**Restore from a recovery phrase** takes the 24 words and, for a wallet older than a day, the
block height it was made at (shown under **Show recovery phrase**). Without the height the
wallet starts scanning at a recent block and misses older payments.

The browser keeps one wallet per network. The lock screen offers the other one.

## Use it

- **Receive** shows a fresh address each time. Money from another wallet can be spent after
  10 blocks, change after 3.
- **Send** pays a unified address with an Orchard receiver.
- **Shops** lists the open shops of this server. Opening a shop adds its item key to the
  wallet and scans for its items in the background, so a purchase does not wait for that scan.
- **Buy** shows the price and the shop's payment address from its signed price list, which
  the wallet has checked. It then builds and proves the trade in the page, hands the shop its
  copy, waits for the shop's signature, signs the payment and sends the transaction. If the
  shop's signed list says another price or address by then, the purchase stops. The paying
  note is saved as held for this offer before the offer leaves. On a phone, proving takes up
  to a minute or two.
- On the testnet, **Receive** offers free test coins from the server's faucet, once per
  address.
- **Prove** makes an ownership proof of an item you hold, as `zsettle-seller prove` does.
  Anyone can check it with `zsettle check-proof` or the app.
- **Show recovery phrase** and **Remove from this browser** are under *This wallet*.

## What the server can and cannot do

The server cannot spend, and it cannot read the phrase or the database. It sees what a light
server sees: your IP address, which block ranges the page asks for (from the wallet's
birthday, and from a shop key's birthday when you open that shop), and the transactions you
send. See [Privacy](../privacy.md#the-browser-wallet).

The server does send the page's code. A server that is hostile, or taken over, could send a
page that copies the phrase. That is the trust any web wallet asks for. For the strongest
setting run the tools or the app on your own computer; see [Install or build](install.md).

## Requirements

A current Chrome, Edge, Safari or Firefox. The page needs cross-origin isolation (for the
threads), which the server sets up; a browser inside another app may block passkeys. The
first visit downloads about 4.5 MB.

## Limits

- The first purchase of a session builds the proving key (seconds on a computer, longer on a
  phone); the page starts it in the background after the first sync.
- A payment (Send) builds its own proving key each time, so it takes longer than a purchase.
- The page buys from shops on its own server only.
