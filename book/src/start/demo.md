# Try the hosted testnet demo

The public demo is a server that runs `zsettle-app` in hosted mode, with its house shop on the
Zcash testnet. The link to it is on the project page: <https://nerses-asaturyan.github.io/zsettle/>.

The wallet runs in your browser. The 24 words and the wallet stay on your device, and proofs
and signatures are made in the page. The server keeps no wallets. See
[The app in your browser](browser-wallet.md) for the details.

What you can do there:

- **Create or restore a wallet.** Pick testnet, write down the 24 words, and lock the wallet
  in this browser with a passkey or a passphrase of at least 12 characters. To bring back a
  wallet you made before, choose **Restore from 24 words**.
- **Get free test coins.** If the server runs a house shop, a new testnet wallet is sent test
  coins right away, once per address (300,000 zatoshis, that is 0.003 TAZ). The wallet page
  shows them on the way. If the faucet cannot pay at that moment, a **Get coins** button stays
  for later.
- **Buy from an open shop.** The Market lists the shops that are open now. The house shop
  keeps a few items listed and makes new ones when they sell. The page checks the shop's
  signed price list, builds and proves the trade, and sends it.
- **Sell from your browser.** On the Sell tab, create items, set prices, and open your shop.
  Other visitors reach it at `https://<demo domain>/shop/<code>`. The shop is open while your
  page is open: the page signs offers with the key in your browser. Close the page and the
  shop closes.
- **See an item's history**, traced from the chain with viewing keys, in your browser.
- **Prove you own an item**, or check that someone else does: the holder makes a proof in the
  wallet and shares it (text, QR code or the Share button), and anyone pastes or scans it in the
  market's Ownership card. No code needs to go back and forth; a live check with a code is
  optional. See [Ownership proofs](../how/ownership.md).
- **Describe what you sell.** When you list an item, add a short description and pick an icon.
- **Add it to your phone's home screen.** The page has a web app manifest, so a phone browser
  can install it and open it like an app.

## Open it in a real browser

Links tapped inside apps (Instagram, Facebook, Telegram, Slack, X) often open in that app's
own small browser. Passkeys usually do not work there, and some of these browsers change the
page's colours. The page notices this and says so: use the app's menu (⋮ or ···) and choose
"Open in browser", or use a passphrase instead.

## Know the limits

- The demo is for test coins. The wallet can be made on mainnet too, but the server sends the
  page's code, so for real ZEC the strongest setting is running zsettle on your own computer.
- Your wallet lives in this browser only. Keep the 24 words: they are the only way back if the
  browser loses its data, or on another device.
- Your shop sells only while its page is open. If the server restarts, open the shop again.
- The server limits how many requests, shops and test coins it serves. See
  [Security model and limits](../security.md#hosted-mode).
