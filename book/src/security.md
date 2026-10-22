# Security model and limits

zsettle is young software and has not been audited. It has moved real funds in small amounts.
Report problems privately as described in `SECURITY.md`. The full threat table is
`spec/THREAT_MODEL.md`; this page sums it up and adds the app's own protections.

## The trade

| Threat | What happens |
|---|---|
| Seller sells the item elsewhere after signing | The trade's item nullifier is already used; consensus rejects the whole trade; the buyer's ZEC does not move. |
| Buyer alters the payment after the seller signed | Not possible: the ZIP 244 sighash covers every action's value commitment, nullifier, randomized key, note commitment and ciphertext. The combiner also refuses differing fields. |
| Buyer never broadcasts | The seller's signature is a free option until expiry, at most `max_expiry_delta` (default 40 reference blocks, about 50 minutes). The seller can end it sooner with `cancel`. |
| Offer with no expiry, or already expired | Refused. |
| Envelope with a proof over a made-up tree | Refused: the anchor must be one of the seller wallet's own tree roots from the last 100 blocks, and the proof is checked against it. |
| Seller asked to sign an extra spend | The service signs exactly one action, the listed item's. The policy also refuses a trade that spends another note of the listed item account. |
| Payment too small, or paid in dust | Payment outputs are decrypted with the payout key and must reach the price; outputs of 5,000 zatoshis or less do not count. |
| Envelope for another consensus branch | Refused: the branch must be the one live at the next block. |
| Many offers for one item | All acceptable ones are signed; only one can be mined; the seller is paid once; nothing is locked. |
| Tampered listings file | Refused: the file is signed by the item account's key. Compare the key fingerprint to be sure it is the seller you meant. |
| Old listings file served again | `issued_at` is signed; `buy` warns after 1,000 blocks. |
| Seller signs against an old view of the chain | The service syncs before every pass and signs nothing in a pass whose sync failed. |
| Network upgrade this build does not know | The tools compare the server's consensus branch with their own before building or sending, and stop. |

**Message signing.** Listings files and ownership proofs are signed with the item account's
Orchard spend key, the same key that authorizes its spends. This cannot be abused to forge a
transaction. A transaction is signed over a 32-byte ZIP 244 sighash under a *randomized* key
(`rk = ak + alpha*G`); a listing or a proof is signed over a longer, `zsettle-...`-prefixed
message under the base key. So a message signature can never be, nor be turned into, a spend
authorization, and the two message kinds cannot stand in for each other either (different
prefixes). As defence in depth the policy refuses any item action whose spend randomizer is
zero, so the bare key never reaches the chain.

**Delisting is not cancelling.** `publish --remove` only takes an item out of the file; an offer
the service already signed for it stays valid until it expires. To end outstanding offers for
good, spend the item with `cancel`, which moves it to a fresh note of the same account.

## Wallet files

- The mnemonic is age-encrypted: to an identity file created owner-only, or to a passphrase
  (scrypt). Someone who copies the wallet directory learns its history from the database but
  cannot spend. A passphrase that comes from a passkey (`passkey:` and 64 hex digits, 256
  random bits) gets a light scrypt work factor, since it cannot be guessed; a typed
  passphrase keeps age's full one.
- On Unix, new wallet directories, `keys.toml`, the reservations file, the full and final
  PCZT files and the reply file are owner-only. The reservations file, the PCZT files and the
  reply file are written to a new file first and then renamed into place, so a crash or a
  second writer never leaves half a file.
- The passphrase is held in zeroising memory. `serve --sync-cmd` does not pass
  `ZSETTLE_PASSPHRASE` to the command it runs.

## The seller service

The service is built to keep running against bad input:

- Inbox files above 1 MiB are refused unread. Symlinks and other non-regular files are
  skipped. One bad envelope never fails the pass for the others.
- Replies are named by a content tag, so two envelopes with the same file name never collide,
  and a restart never signs the same envelope twice.
- Cheap checks run before the proof; the proof is verified once per envelope.
- Envelopes are handled in arrival order. At most 100 stay deferred per pass, and a deferral is
  given up after ten blocks.
- Refused envelopes are deleted; at most 10,000 reason files are kept. Filed envelopes,
  replies and reasons are deleted after a day.
- A failed pass is retried with exponential backoff up to five minutes.

The HTTP endpoint (`serve --listen`) is the service's own small server:

- a request head of at most 8 KiB, read and write timeouts of ten seconds;
- bodies above 1 MiB refused before they are read;
- at most 32 connections at once;
- `503` once 1,000 offers wait in the inbox;
- no `Server` header; one request per connection;
- each buyer can fetch only the reply to its own content tag.

It speaks plain HTTP. Put a TLS proxy or a Tor onion service in front of it for the internet.

## Local mode

`zsettle-app` without `--public`:

- listens on 127.0.0.1 only;
- answers only when the `Host` header is `localhost:<port>` or `127.0.0.1:<port>`, which stops
  DNS rebinding;
- refuses a request whose `Origin` is not the page's own;
- requires the page key in the `X-Zsettle-Token` header for every `/api/` call except
  `/api/hello`. The key is 16 random bytes, saved owner-only as `page.token` in the profile
  directory. It reaches the page in the link's `#token=` fragment, which the browser does not
  send to the server; the page keeps it in session storage;
- sends a strict content security policy and `X-Frame-Options: DENY`, `nosniff`,
  `no-referrer` and `no-store` on every answer.

The page key only keeps other web pages and other local users' browsers away from the app.
Anyone who can read your profile directory, or who has the link, can use the app. The wallet
itself is still locked by its passphrase or passkey.

## Hosted mode

`zsettle-app --public https://<domain> --web-wallet <dir>` keeps no visitor wallets. The
wallet runs in the visitor's page:

- The 24 words and the wallet database are kept in the browser (IndexedDB), encrypted with
  AES-GCM under a key from a passkey (its WebAuthn PRF output, with a salt of the wallet's own)
  or from a passphrase (PBKDF2 with SHA-256, 600,000 rounds). The server never sees the words,
  the passphrase or the passkey's output.
- Proofs and signatures are made in the page. The server relays chain data and passes offers
  and replies between buyers and shops.
- A browser shop holds its item key in the wallet's worker while its page is open, and checks
  each offer with the seller service's checks before it signs its half. Every request it makes
  to its mailbox is signed with that key under its own domain (`zsettle-mailbox-v1`), so only
  the key's holder can change its listings or answer for it, and such a signature cannot pass
  for a listings signature or an ownership proof. A mailbox request more than 300 seconds from
  the server's clock is refused.
- The server's only wallet is its house shop's, which pays the test coins.

**What you trust the server with.** The server sends the page's code. A hostile server, or one
taken over, could send a page that copies the words or the passphrase. That is the trust any
web wallet asks for. The server also sees what a light server sees: your IP address, the block
ranges and transactions the page asks for, and the transactions you send; see
[Privacy](privacy.md#the-browser-wallet). For the strongest setting, run the tools or the app on
your own computer.

Requests must come from the site itself:

- the `Host` must be the public host, and an `Origin`, when sent, must be the public origin;
- every POST under `/api/` needs the `X-Zsettle: 1` header (a cross-site form cannot set it,
  and the server answers no CORS preflight).

The page runs cross-origin isolated (`Cross-Origin-Opener-Policy: same-origin`,
`Cross-Origin-Embedder-Policy: require-corp`), with a content security policy that allows
scripts and WebAssembly from the site only and connections to the site only.

In both modes the app refuses chunked bodies (`Transfer-Encoding`) and a request head that
repeats `Host` or `Content-Length`.

Limits built into the code (the constants at the top of `crates/zsettle-app/src/main.rs` and
`crates/zsettle-app/src/mailbox.rs`):

| What | Limit |
|---|---|
| Chain relay calls | 300 a minute per visitor, 600 a minute per wider network (an IPv6 /48 or IPv4 /24), 3,000 a minute in all, 24 at once |
| Transactions sent through the relay | 10 a minute per visitor |
| Other API calls | 120 a minute per visitor |
| Mailbox calls | 120 a minute per visitor |
| Browser shops | 200 mailboxes at once (when full, the one closed longest ago makes room); 3 per visitor; a mailbox is dropped a day after last use |
| Offers to shops | 10 a minute per visitor, 200 a minute in all; per browser shop 20 waiting (8 MiB), 5 handed to its page per poll, dropped after 15 minutes unanswered or when the shop closes; 256 MiB of offers and replies in all mailboxes together |
| Replies | 200 kept per browser shop, a signed reply at most 256 KiB |
| Listings uploaded to a mailbox | 256 KiB |
| Test coins | once per address, four times a day per visitor, 12 an hour in all; at most 4 faucet payments waiting at once |
| Request | must arrive whole within 30 seconds; 64 KiB body for the API, about 1 MiB for an offer or a mailbox call |
| Connections | 200 at once |

A "visitor" is the first address in `X-Forwarded-For`, set by the proxy in front: an IPv4
address, or an IPv6 /64 network (an IPv4-mapped IPv6 address counts as the IPv4 address). The
app listens on 127.0.0.1 only, so no one else can set the header.

**Test coins.** A faucet payment that may already have reached the network (the broadcast
call itself failed) is never sent again. A payment cut short by a restart may have been sent,
and the wallet it went to is out of the server's sight, so its address stays counted as paid.

**State files.** `hub.json`, the house's `app.json` and the other state files are written to a
new file and then renamed into place, so a crash never leaves half a file. If `hub.json` or
the house's `app.json` cannot be read, the server refuses to start instead of starting empty.

More rules:

- The server talks to its own house shop only. A visitor cannot make it fetch or post to any
  other address.
- Only three requests reach a shop (its listings, an offer, a reply). Answers from the house
  shop are rebuilt with the server's own security headers.
- A browser shop is open only while its page is: it counts as closed 45 seconds after the page
  last asked for offers, and buyers then get `503`.
- Mailboxes are kept in memory. After a restart of the server every browser shop is closed
  until its owner opens it again.

The setup script adds more around the process; see [Hosting the demo](hosting.md#what-the-setup-protects).

## Known limits

- Not audited.
- On a hosted server the wallet runs the code the server sends; see
  [The app in your browser](start/browser-wallet.md#what-the-server-can-and-cannot-do).
- The privacy limits on the [Privacy](privacy.md) page.
- Anyone who holds a listings file can see every trade of that item account.
- The light server can see a lot of metadata unless you use Tor or your own server.
