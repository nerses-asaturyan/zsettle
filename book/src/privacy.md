# Privacy

zsettle keeps buyer and seller shielded. The listed price is public, by the seller's choice.
This page says what each party can see, and where the limits are. The threat model in
`spec/THREAT_MODEL.md` has the same points in table form.

## Summary

| Who | Sees | Does not see |
|---|---|---|
| Anyone watching the chain | A two- or three-action Ironwood transaction with default expiry and fee, like other wallet transactions. | That it is a trade, the parties, the amounts. |
| Holder of the listings file | The item notes of the listed account, their values and names, and the block in which each one is spent; the payout address and prices; `issued_at`; the account birthday. | The buyer's new item note, the buyer's address or balance, the seller's other accounts. |
| The seller | The price paid, the trade's expiry, the nullifier of the buyer's paying note. With an exact-value two-action offer: that note's value (price plus fee). Over HTTP: the buyer's IP, unless the buyer uses Tor. | The buyer's viewing key, addresses, other notes, balance, the change amount. |
| The buyer | The listings file and what it shows (above). | The seller's other notes and accounts. |
| The light server | IP addresses, block ranges scanned, imported listing birthdays, transactions broadcast. | The wallet's transparent addresses (zsettle is Orchard-only and queries none). |
| A hosted server | What a light server sees, for the wallets in its visitors' pages; a browser shop's listings, the offers its buyers leave and its replies. | The recovery phrase, the passphrase, the wallet database. It holds no visitor's keys (but it sends the page's code). |

## The buyer

- The item is received at a fresh diversified address of the buyer, and its output is encrypted
  with the buyer's outgoing key. The published item key decrypts no output of a trade.
- The seller copy of the PCZT carries no buyer viewing key, witnesses or value-commitment
  trapdoors, so the seller cannot solve for the buyer's values from the value commitments.
- By default the item goes to the wallet's own item account, apart from the paying account.
  Publishing that account's key for a resale says nothing about your payments.
- **Limit: the paying note's nullifier.** The seller learns it from the envelope. If the trade
  is mined, the chain shows it anyway. If it is not mined, the seller could recognise the later
  transaction that spends that note. Run `zsettle rotate`: it moves such notes to your own
  account in a plain transaction, after which the nullifier points only at that transfer.
- **Limit: exact payments.** With an exact-value note (two actions), the seller, and anyone with
  the listings file who recognises the trade on chain, learns that the note was worth price plus
  fee.
- Your full PCZT (`buy --out`) and the final PCZT written by `finish` hold your full viewing key
  and witnesses. Keep them private. The seller copy and the signed reply do not hold them.

## The seller

- Items live in their own ZIP 32 account. The listings carry only that account's Orchard viewing
  key: no Sapling or transparent key, nothing about other accounts.
- The payout address is derived for the market, not the wallet's default address.
- The service decides on public data only, so offers cannot be used to learn which addresses or
  notes belong to the wallet. A sibling address of the payout account is refused in the same
  words as a stranger's (there is a test for this).
- Refusal texts do not include the seller's own chain heights.
- **Limit: everything in the item account is visible to listings holders.** Whoever has the
  file sees every note of that account and when each one is spent or cancelled. Use one account
  per item (`new-item-account`) to limit this to one item.
- **Limit: `issued_at`.** The file says the height at which it was last signed. The service
  re-signs after every sale, so this shows the seller's sync height at that moment.
- **Limit: sync state.** Someone sending offers can tell whether the seller's wallet is behind
  (an offer is deferred instead of decided).
- The envelope carries the item account's full viewing key in the clear on the item action,
  because the signer needs it. So whoever sees an envelope holds the consignment, even without
  the listings file.

## On chain

A trade uses the standard expiry delta (40 blocks before NU7, 120 from NU7), standard ZIP 317 fees and the standard
anchor depth, and has two or three actions like any wallet transaction. Without the listings
file it is not marked. **Limit:** whoever holds the listings file can derive the listed items'
nullifiers, and so spot every trade of that account. **Limit:** the anchor shows how deep the
spent notes had to be. The command-line tools and the local app keep the default of 3
confirmations (`--min-confirmations`), as ZIP 315 wallets do. The wallet in the browser also
anchors 3 blocks deep whenever notes that deep can pay. Only when they cannot does it spend
coins from their first block, and that transaction anchors at the newest block, which ZIP 315
wallets do not do.

## The light server

Every sync, broadcast and `verify` talks to a lightwalletd server over TLS. The server learns:

- the IP behind each call;
- the block ranges a wallet scans, from its birthday;
- on `import-listing`, the tree state at the listing's birthday, which points at the listing
  you imported;
- on `verify` without `--private`, the item's genesis transaction, which names the item
  (with `--private`, only the listings' birthdays, which still name the consignment);
- the full transaction on broadcast;
- from a running seller service: a sync every few seconds, which marks the IP as a running
  service.

It is not sent the wallet's transparent addresses. zsettle is Orchard-only and makes no
transparent-address query; the one transparent-UTXO refresh the wallet library performs on each
sync is answered inside the process (`broadcast::Shielded`) instead of reaching the server. This
holds as long as the pinned libraries make no other transparent query.

Use Tor or your own lightwalletd if the rest matters to you.

## Network and proxies

- Offers over plain HTTP carry the redacted envelope and the signed copy, which hold no buyer or
  seller secret beyond the item account's viewing key (the consignment), which is already
  semi-public (see “The seller” above). But an observer can link an IP to an offer, and the
  content tag that routes replies is a hash of the plaintext envelope, so it separates buyers
  from each other but hides nothing from an on-path observer. For the internet, put a TLS reverse proxy or
  a Tor onion service in front of `serve --listen`.
- Fetching listings by URL shows the seller's service your IP, the time and the client's
  `User-Agent`, nothing from the wallet. A window-shopper is visible to the seller as such.
- `buy`, `finish` and listings fetches by URL honour `ALL_PROXY`, `HTTPS_PROXY` and
  `HTTP_PROXY` (and lower-case forms). For Tor: `HTTPS_PROXY=socks5://127.0.0.1:9050`
  (`socks5h://` is accepted too).
- A proxy variable the tool cannot use stops the command; it never falls back to a direct
  connection. An empty variable means no proxy. `NO_PROXY` is not read, so a proxy applies to
  every host, a seller on localhost included.
- **Limit:** the lightwalletd connection is not proxied by these variables. Run your own
  lightwalletd, or the whole command under a transparent proxy.

## Explorer links

The app links transactions to the ZecBlock explorer. Opening a link tells the explorer which
transaction you look at.

## The browser wallet

On a hosted server the app at `/` is a wallet that keeps its phrase and database in the
browser, encrypted. The server relays chain data for it and sees what a light server sees: the
IP address, the block ranges the page asks for, the tree state it asks for when a wallet or a
shop key starts (rounded down to 1,000 blocks), the listed items whose blocks it looks up when
you open a shop (which shows which shop you opened; the list is public), the transactions of
your item notes whose names it fetches after a restore (compact blocks carry no memos; the
shop that sold you the item already knows that transaction), and each transaction it sends.
The server passes these requests on to its light server (zec.rocks by default), which sees
them coming from the server, not from you.

Item histories and ownership-proof checks also run in the page, over blocks fetched through
the relay. The trace reads on to the chain tip, so the block ranges do not show the server
where the trace ended.

When you sell from the browser, the server keeps your shop's mailbox: your signed listings,
the offers buyers leave, and your signed replies. It sees when your page is open (the page
asks for offers every few seconds), and your IP address. Buyers' offers carry what they carry
for any shop; see "Network and proxies" above.

The server also sends the page's code: a hostile server could send a page that leaks the
phrase. See [The app in your browser](start/browser-wallet.md#what-the-server-can-and-cannot-do).

## The hosted server

For its limits (relay calls a minute, test coins a day) the server keeps each visitor's IP
address in memory for at most a day, and nothing else about the visitor. The addresses that
got test coins are kept in `hub.json`. It writes no access log, and the web proxy in front of
it writes none either; the proxy's error log (a request it could not pass on, for example
during a restart) and the firewall's log of blocked packets can record an address in the
system journal.
