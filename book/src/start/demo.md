# Try the hosted testnet demo

The public demo is a server that runs `zsettle-app` in hosted mode on the Zcash testnet. The
link to it is on the project page: <https://nerses-asaturyan.github.io/zsettle/>.

What you can do there:

- **Create a wallet.** Use a passkey, or a wallet code and a passphrase of at least 12
  characters. The server shows you the wallet code; keep it if you use a passphrase.
- **Get free test coins once.** If the server runs a house shop, a new wallet can ask for test
  coins one time (300,000 zatoshis, that is 0.003 TAZ).
- **Buy from an open shop.** The market lists the shops that are open now. The house shop
  keeps a few items listed and makes new ones when they sell.
- **Open your own shop.** Mint items, set prices, and open the shop. Other visitors reach it at
  `https://<demo domain>/shop/<your wallet code>`.
- **See an item's history**, traced from the chain with viewing keys.

## Know the limits

- The demo is for test coins only. The server holds every demo wallet, so whoever runs it
  could take what is in them. For real ZEC, run zsettle on your own computer.
- A session lasts 12 hours. A wallet nobody used for two hours is locked again.
- An open shop keeps signing offers while you are away. The page warns you about this when you
  sign out with the shop open; stop the shop on the Sell tab first to close it.
- A wallet that was never synced, got no test coins and listed nothing is removed after three
  days.
- The server limits how many wallets and shops it serves. See
  [Security model and limits](../security.md#hosted-mode).
