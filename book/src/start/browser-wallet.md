# The app in your browser

On a hosted server (`zsettle-app --public ... --web-wallet ...`) the app at `/` is the
wallet, and it runs inside the page. It is the same wallet code as the command-line tools,
compiled to WebAssembly. The keys stay on your device; the server keeps no wallets.

On the hosted demo it is the start page, `https://<demo domain>/`. The old `/wallet/` address
leads there too.

## What stays where

| What | Where |
|---|---|
| The 24-word recovery phrase | In this browser, encrypted. It is made in the page and never sent anywhere. |
| The wallet database (notes, history) | In this browser (IndexedDB), encrypted with the same key. |
| Proving and signing | In the page, in a background worker with a thread pool. |
| Chain data | Fetched through the server's relay at `/api/chain/`: blocks, tree states, the chain tip, and finished transactions to send. These are the requests any light wallet makes. |
| Offers to a shop | Posted to the shop on the same server (`/shop/<code>/offer`), as the command-line `buy` posts them. |
| Your shop's item key | In the wallet's worker, while your shop is open. |

The key that encrypts the phrase and the database comes from a passkey (its PRF output) or
from a passphrase (PBKDF2, 600,000 rounds). A passkey wallet asks for the passkey again
before every action that signs (a payment, a purchase, minting, listing, delisting,
cancelling, opening the shop, a proof) and before showing the backup words; a passphrase
wallet asks for the passphrase again before showing the backup words.

## Create or restore

1. Open the app and choose **Create a wallet**. Pick the network: testnet (free test coins)
   or mainnet (real ZEC). You can switch later.
2. Write the 24 words down. They are the only way back if the browser loses its data.
3. Lock it with a passkey, or with a passphrase of 12 characters or more.

**Restore from 24 words** takes the words and, for a wallet made more than a day ago, the
block height it was made at. Without the height the wallet starts scanning at a recent block
and misses older payments.

The welcome screen also has a way back for a forgotten passphrase or a lost passkey: restore
from the recovery phrase. A wallet opens in one tab at a time.

## Testnet and mainnet

The same wallet works on both networks. The **Testnet** or **Mainnet** label in the top bar
(and **Network** in the menu) opens the network switch; switching needs no second unlock.

- Each network has its own coins, items, shops and history. The 24 words give each network
  its own keys (ZIP 32), so nothing on chain links the two.
- The first switch to a network starts it at a recent block. Payments these words got on that
  network before then do not show; restore from the 24 words with the block height to find
  them.
- On mainnet a notice under the top bar, on every page, says the coins are real and the
  software is not audited. The house shop and free test coins are on the testnet only.
- A switch waits for a running sync or task to finish, and closes your shop if it is open.
- If this browser already holds a wallet for that network under another lock (other words,
  or the same words restored with a new passphrase), the switch leaves it alone and goes back
  to the start page, where it unlocks with its own lock.
- The app opens on the network used last; the start page can open the other one instead.

Add the page to the home screen to use it like an app; the page also asks the browser to keep
its storage. Browsers can still clear site data (Safari does after weeks without a visit to a
site that is not on the home screen), so keep the 24 words.

## Use it

- **Receive** shows a fresh address each time. Coins can be spent once they are in a block:
  Send, Buy, Create items and Cancel all take them from the first confirmation. Notes three
  blocks deep are used first when they can pay, so the transaction anchors where ZIP 315
  wallets anchor theirs; coins from the newest blocks anchor at the tip, which shows on chain
  (see [Privacy](../privacy.md#on-chain)).
- **Send** pays a unified address with an Orchard receiver.
- On the testnet, a new wallet asks the server's faucet for free test coins at once, if the
  server runs a house shop. Each address gets them once. If the server refuses the request
  (its limits), a **Get coins** button stays for later; if the house shop has no coins ready,
  the server keeps trying for about ten minutes.
- **Market** lists the open shops of this server. Opening a shop adds its item key to the
  wallet and scans for its items in the background, so a purchase does not wait for that scan.
  The scan starts at the block of the oldest listed item (rounded down to 1,000 blocks), not at
  the shop's own start. A purchase made before the scan ends says it is waiting for it.
- **Buy** shows the price, the network fee and the total, from the shop's signed price list,
  which the wallet has checked. It then builds and proves the trade in the page, hands the shop
  its copy, waits for the shop's signature, signs the payment and sends the transaction. The
  payment goes to the payout address in the signed list. If the shop's signed list says another
  price by then, the purchase stops. The paying
  note is saved as held for this offer before the offer leaves. On a phone, proving takes up
  to a minute or two.
- **History** traces an item through the chain in the page, with the shop's signed listings
  key and the wallet's own item key, as `zsettle verify` does.
- **Prove I own it** makes an ownership proof of an item you hold, as `zsettle-seller prove` does.
  Anyone can check it with `zsettle check-proof` or the app; the app checks it in the page.
- After a restore, item names come back with the next sync: the wallet fetches each item
  note's transaction and reads its memo with its own key. The activity list starts with "Wallet restored".
- **Backup words** shows the 24 words again, after the passkey or the passphrase.

## Sell from the browser

Minting, listing, delisting and cancelling all run in the page, on the **Sell** tab.

1. **Create items.** Each one is a new note in the wallet's item account (minting).
2. **List** an item with a price, and if you like a short description and an icon. The page
   signs the listings with the item key.
3. **Open the shop.** The page uploads the signed listings to the server and asks for offers
   every few seconds. Buyers reach the shop at `https://<domain>/shop/<code>`; the code is the
   fingerprint of your listings key (12 hex characters).

The shop is open while its page is open. The page checks each offer with the same checks as
the seller service (`zsettle-seller serve`), signs its half of a trade it accepts, and posts
the reply. Close the page, or lock the wallet, and the shop closes; the server counts it as
closed 45 seconds after the page last asked for offers. A server restart empties the shops'
mailboxes: a shop whose page is still open opens again by itself the next time it asks for
offers (every few seconds). Offers that were waiting at that moment are lost; those buyers'
ZEC does not move.

The server keeps a mailbox for your shop: the signed listings, the offers buyers leave, and
your replies. Every request the page makes to it is signed with your item key, so nobody else
can change your listings or answer for your shop.

**Cancel offers**, on a listed item, spends it to a fresh note of the same account, so no offer the shop signed
earlier can settle. Delisting alone does not do that.

## What the server can and cannot do

The server cannot spend, and it cannot read the phrase or the database. It sees what a light
server sees: your IP address, which block ranges the page asks for (from the wallet's
birthday, and from a shop key's birthday when you open that shop), and the transactions you
send. When you sell, it also sees your shop's listings, the offers and your replies, and when
your page is open. See [Privacy](../privacy.md#the-browser-wallet).

The server does send the page's code. A server that is hostile, or taken over, could send a
page that copies the phrase. That is the trust any web wallet asks for. For the strongest
setting run the tools or the app on your own computer; see [Install or build](install.md).

## Requirements

A current Chrome, Edge, Safari or Firefox. The page needs cross-origin isolation (for the
threads), which the server sets up; a browser inside another app may block passkeys. The
first visit downloads about 4.5 MB. On Safari, and on every browser on an iPhone or iPad, the
page sets aside 512 MB of memory when it starts: Safari 26.0 to 26.2 stops a wallet whose
threads grow their memory later.

## Limits

- The first purchase of a visit builds the proving key (seconds on a computer, longer on a
  phone). The page starts it in the background once a shop opened in the Market has been
  scanned, if the page is in front and the wallet is idle; otherwise the purchase builds it.
- A payment (Send) builds its own proving key each time, so it takes longer than a purchase.
- The page buys from shops on its own server only.
- A shop sells only while its page is open.
