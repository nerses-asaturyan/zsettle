# Try the hosted testnet demo

The public demo is a server that runs `zsettle-app` in hosted mode on the Zcash testnet. The
link to it is on the project page: <https://nerses-asaturyan.github.io/zsettle/>.

What you can do there:

- **Create a wallet.** Use a passkey, or a wallet code and a passphrase of at least 12
  characters. The server shows you the wallet code; keep it if you use a passphrase.
- **Get free test coins.** If the server runs a house shop, a new wallet is sent test coins
  right away, one time (300,000 zatoshis, that is 0.003 TAZ). The wallet page shows them on
  the way. If the faucet cannot pay at that moment, a "Get coins" button stays for later.
- **Buy from an open shop.** The market lists the shops that are open now. The house shop
  keeps a few items listed and makes new ones when they sell.
- **Open your own shop.** Mint items, set prices, and open the shop. Other visitors reach it at
  `https://<demo domain>/shop/<your wallet code>`.
- **See an item's history**, traced from the chain with viewing keys.
- **Prove you own an item**, or check that someone else does: the checker gives a challenge,
  the holder makes a proof in the wallet, and the checker pastes it in the market's Ownership
  card. See [Ownership proofs](../how/ownership.md).
- **Describe what you sell.** When you list an item, add a short description and pick an icon.
- **Add it to your phone's home screen.** The page has a web app manifest, so a phone browser
  can install it and open it like an app.

## Open it in a real browser

Links tapped inside apps (Instagram, Facebook, Telegram, Slack, X) often open in that app's
own small browser. Passkeys usually do not work there, and some of these browsers change the
page's colours. The page notices this and says so: use the app's menu (⋮ or ···) and choose
"Open in browser", or use a wallet code and a passphrase instead.

## Know the limits

- The demo is for test coins only. The server holds every demo wallet, so whoever runs it
  could take what is in them. For real ZEC, run zsettle on your own computer.
- A session lasts 12 hours. A wallet nobody used for two hours is locked again.
- An open shop keeps signing offers while you are away. The page warns you about this when you
  sign out with the shop open; stop the shop on the Sell tab first to close it.
- If the demo server restarts, your shop is closed until you sign in again; then it opens
  again by itself.
- A wallet that was never synced, got no test coins and listed nothing is removed after three
  days.
- The server limits how many wallets and shops it serves. See
  [Security model and limits](../security.md#hosted-mode).
