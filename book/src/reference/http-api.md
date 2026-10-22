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

## Hosted relay (`zsettle-app --public`)

In hosted mode each shop is reached through the app:

| Method and path | Passed to the shop as |
|---|---|
| `GET /shop/<code>/listings` | `GET /listings` |
| `POST /shop/<code>/offer` | `POST /offer` |
| `GET /shop/<code>/reply/<tag>` | `GET /reply/<tag>` |

Anything else under `/shop/` answers `404`. A closed shop answers `503`, and a shop that does
not answer gives `502`. While a wallet action pauses a shop (the app stops it so the action
can use the wallet alone), the relay answers `GET listings` itself from the shop's signed
listings file, and holds an offer or a reply request for up to 25 seconds while the shop opens
again; after that it answers `503`. The answer's status and body are passed back, with the content type
limited to JSON, octet-stream or plain text, and with the app's own security headers.

So the shop base URL to give a buyer is `https://<domain>/shop/<code>`: `zsettle buy
--seller-url https://<domain>/shop/<code>` and `zsettle list --listings
https://<domain>/shop/<code>/listings` work against it.

## Browser wallet and chain relay (`zsettle-app --web-wallet`)

`GET /wallet/` and the files under it serve the browser wallet from the given directory, read
once at start. Its answers carry `Cross-Origin-Opener-Policy: same-origin`,
`Cross-Origin-Embedder-Policy: require-corp` (shared memory for the proving threads), a
content security policy that allows WebAssembly, and an entity tag.

The relay passes public chain data from the server's light server, for either network
(`net=main` or `net=test`). It holds no key; each call is one lightwalletd request.

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

Calls are limited per visitor (300 a minute), per wider network (an IPv6 /48 or IPv4 /24, 600
a minute), for the whole server (3,000 a minute) and to 24 at once; a visitor may send 10
transactions a minute. Each call has a time limit at the light server. In hosted mode
`GET /api/shops` lists the open shops without a session, and `POST /api/wallet-faucet`
(`{ "address": "utest1..." }`) sends test coins to a browser wallet once per address, under
the faucet's limits.

## The app's own API

`zsettle-app` serves its page and a JSON API that the page uses. It is meant for the bundled
page, not as a stable interface. Code: `handle_local`, `handle_hosted` and `api` in
`crates/zsettle-app/src/main.rs`.

Static files: `GET /`, `/index.html`, `/app.js`, `/qr.js`, `/style.css`, `/favicon.svg`.

API calls (JSON bodies; errors answer `400` with `{"error": "..."}`):

| Method and path | What it does |
|---|---|
| `GET /api/hello` | Mode (`local` or `hosted`); in hosted mode also the network, the PRF salt, whether the visitor is signed in, and whether a house shop exists. |
| `POST /api/wallet/create` | Create the wallet (`network`, `passphrase`, optional `passkey`). |
| `POST /api/unlock` | Unlock with a passphrase. Hosted: by passkey `credential` or wallet `code`. |
| `POST /api/backup` | Show the 24 backup words, for the passphrase given again. |
| `POST /api/lock` | Forget the passphrase. Hosted: also end the session. |
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
| `GET /api/activity` | The activity log. |
| `GET /api/jobs/<id>` | State, steps, result or error of a job. |
| `GET /api/shops` | Hosted only: the open shops of this server. |
| `POST /api/faucet` | Hosted only: test coins from the house wallet, once per wallet. |

Long actions return `{"job": <id>}` at once; the page polls `/api/jobs/<id>`.

Local mode needs the page key in `X-Zsettle-Token` for every `/api/` call except
`/api/hello`. Hosted mode needs the session cookie for everything except `/api/hello`,
`/api/wallet/create` and `/api/unlock`, and the header `X-Zsettle: 1` on every POST. See
[Security model and limits](../security.md).
