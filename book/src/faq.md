# FAQ

## Does zsettle need smart contracts or a new Zcash feature?

No. A trade is an ordinary Ironwood transaction with two spends. It works because one
transaction needs every spend signed, and a note can be spent only once.

## Who holds the money during a trade?

Nobody. The payment and the item move in the same transaction. Until the buyer broadcasts it,
nothing has moved.

## What if the seller sells the item to someone else first?

The other trade spends the item note first. The trade you built then spends a nullifier that
is already used, so the network rejects it, and your ZEC does not move. Run `zsettle rotate`
later so the seller cannot recognise the note you offered (see
[Buying](cli/buyer.md#after-an-offer-that-did-not-settle)).

## What if the buyer gets the seller's signature and waits?

The signature works only until the trade's expiry height, at most about 50 minutes with the default
limit (40 blocks before NU7, 120 from NU7). The seller can end it sooner with `zsettle-seller cancel`, which moves the item to a new
note.

## Can several people make offers for the same item?

Yes. The seller signs every acceptable offer. They all spend the same item note, so only one
can be mined. The others become invalid.

## Does the seller see my wallet?

No. The seller sees the price you paid, your offer's expiry, and the nullifier of the note you
paid with. Your viewing key, addresses and balance stay with you. Over HTTP the seller also sees
your IP, unless you use Tor. Details: [Privacy](privacy.md).

## Is the price private?

No. The seller publishes prices in the listings file. Everything else about the trade is
shielded.

## Who can see that my item was sold?

Anyone who has the listings file of the item's account can see when the item note is spent.
That is part of how consignment works. Give an item its own account if you want that to cover
only that item.

## How do I know a listings file is really from the seller?

The file is signed with the item account's key, so it was written by whoever controls those
items. To know it is the seller you meant, compare the key fingerprint that `zsettle list`
prints with one the seller gives you some other way.

## Can I use the hosted demo with real ZEC?

The wallet runs in your browser and the server holds no keys, so the page can make a mainnet
wallet too. But the server sends the page's code, and a hostile server could send a bad page.
The demo's house shop and test coins are on the testnet. For real ZEC, the strongest setting is
to run zsettle on your own computer. See [The app in your browser](start/browser-wallet.md).

## Why does my buy say there is no spendable note?

A note received from another wallet needs ten confirmations, change from your own wallet
needs three. These are the numbers [ZIP 315](https://zips.z.cash/zip-0315) recommends and the
wallet library's default policy, not a network rule: the wait guards against a chain
reorganisation undoing the transaction that paid you. The app's balance card counts them down
per note. Notes reserved for earlier offers are skipped until `rotate` retires them. A trade
is paid from one note: use `consolidate` to merge small ones.

## Why does a command stop with a consensus branch message?

The light server is on a network upgrade this build does not sign for, for example a later
upgrade than this build knows. Update to a newer build. See [Networks](reference/networks.md).

## Can I use zcash-devtool or another wallet with these wallets?

The wallet layout is zcash-devtool's, so devtool can open wallets made with a key file (not
passphrase-protected ones). To move money out to any other wallet, use `pay`.

## Can the item be resold?

Yes. Receive it into an item account (the default), then publish it with `zsettle-seller` from
the same wallet. See [Resale](cli/resale.md).

## Has it been audited?

No. See [Security model and limits](security.md).
