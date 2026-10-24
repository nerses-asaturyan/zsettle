# HTTP API

## Seller service (`zsettle-seller serve --listen`)

Code: `serve_http` and `handle_http` in `crates/zsettle-seller/src/main.rs`. Plain HTTP/1.1,
one request per connection (`Connection: close`).

| Method and path | Request | Answers |
|---|---|---|
| `GET /listings` | | `200` with the signed listings file (`application/json`); `404` if there is no file. |
| `POST /offer` | The redacted PCZT as the body, with `Content-Length` | `200` with `{"tag": "<tag>", "reply": "/reply/<tag>"}`; `413` if the body is empty or above 1 MiB; `503` if 1,000 offers already wait in the inbox; `500` if the offer cannot be stored. |
| `GET /reply/<tag>` | `<tag>` is 32 lower-case hex characters | `200` with the signed PCZT (`application/octet-stream`); `202` "pending" while the envelope waits; `410` with the reason if it was refused, or with a fixed text if its reason was not kept, it expired from the service's files, or it was never received; `400` for a malformed tag. |
| anything else | | `404` |

More answers from any path: `431` when the request head is above 8 KiB, and `503` when 32
connections are already open.

**The tag.** The tag of an envelope is the first 16 bytes, in hex, of a BLAKE2b-256 hash of
the envelope bytes with personalisation `zsettle_envelope`. It is not keyed: anyone with the
envelope bytes can compute it. `zsettle buy` checks that the tag the seller answers with is the
tag of what it sent.

**Where offers go.** A posted offer is written to `<inbox>/<tag>.pczt` and handled like a
dropped file. Its reply is `<outbox>/<tag>.<tag>.signed.pczt`. Posting the same bytes again
clears an old refusal reason, so the envelope is checked afresh.

**Clients.** `zsettle` follows no redirects and uses a 30-second timeout. A listings URL that
answers with a redirect is reported as such: give the final URL. When posting an offer fails
with a connection error or a `502`, `503` or `504` answer, `buy` sends the same offer again
after ten seconds, up to four tries in all (the tag does not change). `finish` keeps polling
a reply URL through such errors until `--wait` runs out.

## Hosted server (`zsettle-app --public`)

A hosted server keeps no visitor wallets: the wallet runs in the visitor's page. Code:
`handle_hosted`, `public_route` and `relay_shop` in `crates/zsettle-app/src/main.rs`, and
`crates/zsettle-app/src/mailbox.rs`. It answers:

| Method and path | What it is |
|---|---|
| `GET /` and the page's files | The app page (see [the app's own API](#the-apps-own-api-local-mode) for the file list). |
| `GET /engine.js`, `/worker.js`, `/pkg/...` | The wallet engine, from the `--web-wallet` directory. |
| `GET /wallet`, `/wallet/...` | `301` to `/`. The separate wallet page is gone. |
| `/api/chain/...` | The chain relay, below. |
| `GET /api/hello` | `{ "mode": "hosted", "network", "has_house", "browser": true }`. `network` is the house shop's network. |
| `GET /api/shops` | The open shops: the house shop and the browser shops, each `{ base, code, name, items, house }`. |
| `POST /api/wallet-faucet` | `{ "address": "utest1..." }`: test coins from the house shop to that address. See below. |
| `POST /api/mailbox/<code>/<action>` | A browser shop's owner: `listings`, `poll`, `reply` or `close`. See below. |
| `/shop/<code>/...` | A shop, for buyers. See below. |
| any other `/api/` call | `404` with `{"error":"this server keeps no wallets: the wallet runs in your browser"}`. |

Every POST under `/api/` needs the header `X-Zsettle: 1`. Errors answer `400` with
`{"error": "..."}`, and `429` when a limit is reached.

### Shops, for buyers

Each shop is reached through the app:

| Method and path | The shop's request |
|---|---|
| `GET /shop/<code>/listings` | `GET /listings` |
| `POST /shop/<code>/offer` | `POST /offer` |
| `GET /shop/<code>/reply/<tag>` | `GET /reply/<tag>` |

Anything else under `/shop/` answers `404`. The answers are those of the seller service above.

- **The house shop** (`code` is `house`) is a `zsettle-seller serve` process of the server.
  The app passes the request on. A closed shop answers `503`, and a shop that does not answer
  gives `502`. While a wallet action pauses the shop (the app stops it so the action can use
  the wallet alone), the app answers `GET listings` itself from the shop's signed listings
  file, and holds an offer or a reply request for up to 25 seconds while the shop opens again;
  after that it answers `503`. The answer's status and body are passed back, with the content
  type limited to JSON, octet-stream or plain text, and with the app's own security headers.
- **A browser shop** is answered from its mailbox. `listings` gives the signed listings as
  uploaded. `offer` stores the envelope and answers `{"tag", "reply"}` as the seller service
  does; it answers `503` when the shop is closed (its page has not asked for offers in the last
  45 seconds) or too many offers wait (20, or 8 MiB, per shop), and `413` for an empty envelope
  or one above 1 MiB. An offer the shop does not answer within 15 minutes is dropped, and all
  of a shop's waiting offers when it closes. `reply/<tag>` answers `200` with the signed PCZT,
  `202` "pending" while the offer waits, and `410` with the reason when it was refused or is no
  longer there.

A visitor may post 10 offers a minute, and the server takes 200 a minute in all; more answer
`429`.

So the shop base URL to give a buyer is `https://<domain>/shop/<code>`: `zsettle buy
--seller-url https://<domain>/shop/<code>` and `zsettle list --listings
https://<domain>/shop/<code>/listings` work against it.

### Browser shop mailboxes

A browser shop keeps its item key in its owner's page. The server keeps a mailbox for it: the
signed listings, the offers buyers leave, and the replies. The shop's `code` is the
fingerprint of its listings key (12 hex characters).

Every request body is JSON with `at` (the time, in unix seconds) and `sig` (hex), plus the
fields below. `sig` is a signature with the shop's item key over

```
zsettle-mailbox-v1\n<what>\n<at>
```

where `<what>` depends on the action. The server checks it against the item key in the shop's
listings and refuses a request whose `at` is more than 300 seconds from its own clock.

| Action | Extra fields | `<what>` | Answer |
|---|---|---|---|
| `listings` | `listings`: the signed listings file, as text (at most 256 KiB) | `listings\n<code>\n<listings>` | `{ code, items }`. Opens a mailbox, or replaces its listings. The listings' signature must hold, their item key must be in its plain Orchard form, and its fingerprint must be `<code>`. An existing mailbox takes listings only for the same key, and never from a request older than the last one. |
| `poll` | | `poll\n<code>` | `{ "offers": [{ tag, envelope }] }`, at most 5 at a time, the envelope in hex. Marks the shop open for 45 seconds. |
| `reply` | `tag`, and `signed` (the signed PCZT in hex, at most 256 KiB) or `refused` (the reason) | `reply\n<code>\n<tag>\nsigned:<hex>` or `reply\n<code>\n<tag>\nrefused:<reason>` | `{ "ok": true }` |
| `close` | | `close\n<code>` | `{ "closed": true }`. The shop is closed until the next `poll`, and its waiting offers are dropped. |

The domain prefix keeps a mailbox signature from passing for a listings signature or an
ownership proof made with the same key. A server keeps at most 200 mailboxes; when it is full,
the mailbox closed longest ago makes room. One visitor may make at most 3 mailboxes that are
kept at once. A shop keeps at most 200 replies. A mailbox and its replies are dropped a day
after last use, and all of them on a restart. Mailbox calls count 120 a minute per visitor.

### Test coins

`POST /api/wallet-faucet` with `{ "address": "utest1..." }` pays 300,000 zatoshis from the
house shop to that testnet unified address. It answers at once with
`{ "state": "sending", "amount_zat": 300000 }`; the payment goes out in the background and
the wallet sees it when it syncs. It works only when the server's `--network` is `test` and it
runs a house shop. Each address gets coins once; a visitor four times a day; the
server 12 times an hour; at most four payments wait at once.

## Chain relay (`zsettle-app --web-wallet`)

The relay passes public chain data from the server's light server to the wallet in the page,
for either network (`net=main` or `net=test`). It holds no key; each call is one lightwalletd
request.

| Method and path | Answer |
|---|---|
| `GET /api/chain/tip?net=` | The chain tip, an encoded `BlockID`. |
| `GET /api/chain/blocks?net=&start=&end=` | Compact blocks `start..=end` (at most 500 blocks and 16 MB), each length-delimited. |
| `GET /api/chain/tree-state?net=&height=` | The tree state after `height`, an encoded `TreeState`. |
| `GET /api/chain/subtree-roots?net=&pool=&start=&max=` | Subtree roots of `sapling`, `orchard` or `ironwood` (at most 10,000), each length-delimited. |
| `GET /api/chain/raw-tx?net=&txid=` | A mined transaction: its height (4 bytes, little-endian) and then its raw bytes. |
| `GET /api/chain/tx?net=&txid=` | `{ height }` as JSON: the block the transaction is in, or `null`. |
| `GET /api/chain/info?net=` | `{ chain_name, consensus_branch_id, block_height }` as JSON. |
| `POST /api/chain/send?net=` | Sends the raw transaction in the body (needs `X-Zsettle: 1`); the server's message as text. |

In hosted mode calls are limited per visitor (300 a minute), per wider network (an IPv6 /48 or IPv4 /24, 600
a minute), for the whole server (3,000 a minute) and to 24 at once; a visitor may send 10
transactions a minute. Each call has a time limit at the light server.

The engine files and the relay are served in local mode too when `--web-wallet` is given, but
the local page does not use them.

The engine files carry `Cross-Origin-Opener-Policy: same-origin`,
`Cross-Origin-Embedder-Policy: require-corp` (shared memory for the proving threads), a
content security policy that allows WebAssembly, and an entity tag. In hosted mode the page's
own files carry the same isolation and security headers (without an entity tag), since the
wallet runs in that page.

## The app's own API (local mode)

In local mode `zsettle-app` serves its page and a JSON API that the page uses. It is meant for
the bundled page, not as a stable interface. Code: `handle_local` and `api` in
`crates/zsettle-app/src/main.rs`. A hosted server answers none of these calls except
`/api/hello` and `/api/shops` (see above): there the page's wallet engine answers the same
calls inside the browser.

Static files: `GET /`, `/index.html`, `/app.js`, `/qr.js`, `/jsqr.js`, `/style.css`, `/favicon.svg`,
`/manifest.webmanifest`, `/icon-192.png`, `/icon-512.png`.

API calls (JSON bodies; errors answer `400` with `{"error": "..."}`):

| Method and path | What it does |
|---|---|
| `GET /api/hello` | `{ "mode": "local" }`. |
| `POST /api/wallet/create` | Create the wallet (`network`, `passphrase`, optional `passkey`). |
| `POST /api/unlock` | Unlock with a passphrase. |
| `POST /api/backup` | Show the 24 backup words, for the passphrase given again. |
| `POST /api/lock` | Forget the passphrase. |
| `GET /api/status` | Profile, network, accounts, shop state, last sync. |
| `GET /api/wallet` | Balance, notes and items. |
| `POST /api/address` | A fresh receiving address. |
| `POST /api/sync` | Start a sync job. |
| `POST /api/pay` | Start a payment job (`to`, `amount`, optional `memo`). |
| `POST /api/mint` | Start a mint job (`names`, one to twenty, each at most 80 characters). |
| `GET /api/listings` | This profile's listings file. |
| `POST /api/publish` | List an item (`note`, `price`, optional `name`). |
| `POST /api/delist` | Delist an item (`note`). |
| `POST /api/cancel` | Cancel an item (`note`). |
| `GET /api/shop` | Shop state and its recent events. |
| `POST /api/shop/start` | Open the shop. Local mode takes `listen` (default `127.0.0.1:8900`). |
| `POST /api/shop/stop` | Close the shop. |
| `POST /api/market` | Fetch and check a shop's listings (`url`). |
| `POST /api/buy` | Start a buy job (`url`, `item`, optional `name`). |
| `POST /api/verify` | Start a history job (`item`, optional `url`). |
| `POST /api/prove` | Start an ownership-proof job (`note`, optional `label`, optional `challenge`). |
| `POST /api/check-proof` | Start a job that checks an ownership proof (`proof`, optional `challenge`). |
| `GET /api/activity` | The activity log. |
| `GET /api/jobs/<id>` | State, steps, result or error of a job. |

Long actions return `{"job": <id>}` at once; the page polls `/api/jobs/<id>`.

Every `/api/` call except `/api/hello` needs the page key in `X-Zsettle-Token`. See
[Security model and limits](../security.md#local-mode).
