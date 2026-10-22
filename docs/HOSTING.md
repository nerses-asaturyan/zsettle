# Hosting the public demo

`zsettle-app --public https://<domain> --web-wallet <dir>` serves many people from one server. The app at `/` is the wallet, and it runs in each visitor's browser: the 24 words and the wallet database stay on the visitor's device, encrypted under a passkey or a passphrase, and proofs and signatures are made in the page. The server keeps no visitor wallets. A visitor can get free test coins once per address, buy from any open shop, and open a shop of their own at `https://<domain>/shop/<code>` while the page is open. A house shop keeps a few items listed, from a small catalogue with different icons, descriptions and prices (0.0002 to 0.0015), and makes new ones when they sell.

The server still sends the page's code, so visitors trust it not to send a bad page. For the strongest setting, people run zsettle on their own computer.

## What it needs

- A small Linux server that stays on: 2 CPUs and 4 GB of memory are enough (for example Hetzner CX22, about €4 a month).
- A domain name pointing at the server. A free DuckDNS name works.
- The three binaries: `zsettle`, `zsettle-seller`, `zsettle-app` (the same build serves the testnet and mainnet).
- The built wallet engine: `crates/zsettle-web/www` after `crates/zsettle-web/build.sh`.

## Setup

On the server, as root:

```
./deploy/setup.sh demo.example.org /path/to/binaries /path/to/zsettle-web/www
```

The script installs Caddy for HTTPS, creates a `zsettle` service user, installs the binaries and the wallet engine, writes a random house passphrase to `/var/lib/zsettle/house.pass`, opens ports 22, 80 and 443, and starts the `zsettle-demo` service, which restarts by itself. It prints the house wallet's address: send it testnet coins once (0.05 TAZ covers the house items and the test coins for about fifteen visitors). Running the script again upgrades the binaries and the engine and keeps the data.

## Running it

```
systemctl status zsettle-demo
journalctl -u zsettle-demo -f
```

Data lives in `/var/lib/zsettle/data`: `house/` (the house shop's wallet) and `hub.json` (the house shop's port and the addresses that got test coins). Browser shops' mailboxes are kept in memory only. Folders left by an older hosted mode (visitor wallets, `prf.salt`) are not loaded.

## What the server serves

- The app page and the wallet engine (`/engine.js`, `/worker.js`, `/pkg/...`). `/wallet` and `/wallet/...` answer `301` to `/`.
- A chain relay for both networks (`/api/chain/...?net=main|test`): the requests any light wallet makes.
- `/api/hello`, the open shops (`/api/shops`) and test coins to an address (`/api/wallet-faucet`).
- A mailbox per browser shop (`/api/mailbox/<code>/...`), each request signed with the shop's item key.
- The shops for buyers: `/shop/<code>/listings`, `/shop/<code>/offer`, `/shop/<code>/reply/<tag>`.

Any other `/api/` call answers `404`. The full list is in the book's HTTP API page.

## Limits built in

- The server talks to its own house shop only: a visitor cannot make it fetch or post to any other address.
- A visitor is an IPv4 address or an IPv6 /64. Relay calls: 300 a minute per visitor, 600 a minute per wider network (IPv6 /48 or IPv4 /24), 3,000 a minute in all, 24 at once; 10 transactions sent a minute per visitor. Other API calls: 120 a minute per visitor; mailbox calls also 120 a minute per visitor.
- Test coins once per address, four times a day per visitor, 12 an hour in all; at most four faucet payments wait at once. A payment that may have reached the network is never sent again.
- Mailboxes: 200 shops at once (when full, the one closed longest ago makes room), 3 per visitor, signed requests within 300 seconds of the server's clock. Offers: 10 a minute per visitor and 200 a minute in all; 20 waiting per shop, dropped after 15 minutes unanswered or when the shop closes. A browser shop counts as open for 45 seconds after its page last asked for offers.
- While a wallet action pauses the house shop, its listings are still served and offers wait for it to open again; buyers retry for a short outage.
- State files are replaced whole (a new file, then a rename), so a crash never leaves half a file. If `hub.json` or the house's `app.json` cannot be read, the server refuses to start rather than start empty.
- Requests must come from the site itself: the Host and Origin are checked, and POSTs need a custom header.
- Only three requests pass to a shop: its listings, an offer, and a reply; every answer carries the server's own security headers.
- The service runs under systemd with a read-only system, no new privileges, no link-local or private addresses (so no cloud metadata), a hidden process list, and caps on memory, processes and CPU. Caddy refuses bodies above 2 MB and slow requests. The script keeps a copy of any Caddyfile it replaces.
