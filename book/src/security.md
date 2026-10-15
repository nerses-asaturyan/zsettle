# Security model and limits

zsettle is young software and has not been audited. It has moved real funds in small amounts.
Report problems privately as described in `SECURITY.md`. The full threat table is
`spec/THREAT_MODEL.md`; this page sums it up and adds the app's own protections.

## The trade

| Threat | What happens |
|---|---|
| Seller sells the item elsewhere after signing | The trade's item nullifier is already used; consensus rejects the whole trade; the buyer's ZEC does not move. |
| Buyer alters the payment after the seller signed | Not possible: the ZIP 244 sighash covers every action's value commitment, nullifier, randomized key, note commitment and ciphertext. The combiner also refuses differing fields. |
| Buyer never broadcasts | The seller's signature is a free option until expiry, at most `max_expiry_delta` (default 40) blocks. The seller can end it sooner with `cancel`. |
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

`zsettle-app --public https://<domain>` is meant for the testnet. **The server holds every
visitor's wallet.** It sees the passphrase (or the passkey's PRF output, which is the
passphrase) whenever a wallet is unlocked. `--network main` is refused unless
`--allow-hosted-mainnet` is given, and a build of `main` refuses `--network test` (it syncs
mainnet only), so the testnet demo needs a `nu7-testnet` build.

Requests must come from the site itself:

- the `Host` must be the public host, and an `Origin`, when sent, must be the public origin;
- every POST needs the `X-Zsettle: 1` header (a cross-site form cannot set it, and the server
  answers no CORS preflight);
- the session cookie `__Host-zs` is HttpOnly, Secure, SameSite=Strict and lasts 12 hours.

In both modes the app refuses chunked bodies (`Transfer-Encoding`) and a request head that
repeats `Host` or `Content-Length`.

Limits built into the code (the constants at the top of `crates/zsettle-app/src/main.rs`):

| What | Limit |
|---|---|
| New wallets | 40 an hour in all, 5 an hour per visitor, 500 in total |
| Unused wallets | a wallet that was never synced, got no test coins and listed nothing is removed after three days (never the house wallet, a signed-in wallet or one with an open shop) |
| Open shops | 25 at once |
| Test coins | once per wallet, four times a day per visitor, 12 an hour in all; at most 4 faucet payments waiting at once |
| Tasks | 2 per wallet, 12 in all |
| Commands that prove (pay, mint, buy, ...) | 3 at once |
| Passphrase checks (scrypt) at once | 4; others wait their turn up to 10 seconds |
| Passphrase checks a minute | 10 per visitor, right or wrong; backup words also 6 a minute per wallet |
| Passphrase length | at least 12 characters (10 in local mode) |
| Wrong sign-in tries | see below |
| Session | 12 hours; a wallet unused for two hours is locked |
| Request | must arrive whole within 30 seconds; 64 KiB body for the API, about 1 MiB for an offer |
| Connections | 200 at once |

A "visitor" is the first address in `X-Forwarded-For`, set by the proxy in front: an IPv4
address, or an IPv6 /64 network (an IPv4-mapped IPv6 address counts as the IPv4 address). The
app listens on 127.0.0.1 only, so no one else can set the header.

**Wrong tries.** Each wrong passphrase costs a second. Wrong tries are counted per visitor,
and per wallet for that visitor only. After five in a row, that visitor waits one minute, then
longer each time, up to an hour. A count is forgotten an hour after its last wrong try. Wallet
codes are public (they name the shops), so the count is never shared by everyone: wrong tries
from others do not hold back the wallet's owner on another connection. A passkey sign-in is
not held back by the wallet's count, only by the visitor's.

**Test coins.** A faucet payment that may already have reached the network (the broadcast
call itself failed) is never sent again; the visitor is told to sync and look again.

**State files.** `hub.json`, each wallet's `app.json` and the other state files are written to
a new file and then renamed into place, so a crash never leaves half a file. If `hub.json` or a
wallet's `app.json` cannot be read, the server refuses to start instead of starting empty.

More rules:

- The server talks to its own shops only. A visitor cannot make it fetch or post to any other
  address.
- The relay passes only three requests to a shop (its listings, an offer, a reply), and
  rebuilds every answer with the server's own security headers.
- Error texts shown to visitors have server paths removed.
- An open shop keeps signing offers while its owner is away; the page says so at sign-out.
- After a restart of the server every shop is closed, since no passphrase is kept on disk. A shop its owner never closed opens again when the owner next signs in.

The setup script adds more around the process; see [Hosting the demo](hosting.md#what-the-setup-protects).

## Known limits

- Not audited.
- The hosted demo is custodial by design. Do not use it for real money.
- The privacy limits on the [Privacy](privacy.md) page.
- Anyone who holds a listings file can see every trade of that item account.
- The light server can see a lot of metadata unless you use Tor or your own server.
