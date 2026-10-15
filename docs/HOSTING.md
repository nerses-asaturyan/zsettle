# Hosting the public demo

`zsettle-app --public https://<domain>` serves many people from one server. Each visitor creates a wallet of their own (passkey, or a wallet code and a passphrase), can get free test coins once, buys from any open shop, and can open a shop of their own at `https://<domain>/shop/<code>`. A house shop keeps a few items listed, from a small catalogue with different icons, descriptions and prices (0.0002 to 0.0015), and makes new ones when they sell.

The demo is for the testnet. The server holds every demo wallet, which is fine for free test coins and wrong for real money: for real ZEC, people run zsettle on their own computer.

## What it needs

- A small Linux server that stays on: 2 CPUs and 4 GB of memory are enough (for example Hetzner CX22, about €4 a month).
- A domain name pointing at the server. A free DuckDNS name works.
- The three binaries built for the testnet line (`nu7-testnet` branch): `zsettle`, `zsettle-seller`, `zsettle-app`.

## Setup

On the server, as root:

```
./deploy/setup.sh demo.example.org /path/to/binaries
```

The script installs Caddy for HTTPS, creates a `zsettle` service user, installs the binaries, writes a random house passphrase to `/var/lib/zsettle/house.pass`, opens ports 22, 80 and 443, and starts the `zsettle-demo` service, which restarts by itself. It prints the house wallet's address: send it testnet coins once (0.05 TAZ covers the house items and the test coins for about fifteen visitors). Running the script again upgrades the binaries and keeps the data.

## Running it

```
systemctl status zsettle-demo
journalctl -u zsettle-demo -f
```

Data lives in `/var/lib/zsettle/data`: one directory per wallet, `hub.json` (which passkey belongs to which wallet, each shop's port, who got test coins) and `prf.salt` (the site's passkey salt; keep it, or every passkey stops working).

## Limits built in

- The server talks to its own shops only: a visitor cannot make it fetch or post to any other address.
- A visitor is an IPv4 address or an IPv6 /64. 40 new wallets an hour in all, 3 an hour per visitor, 500 in total; a wallet that was never once synced, got no test coins and listed nothing is removed after three days. 25 shops open at once.
- Test coins once per wallet, twice a day per visitor, 12 an hour in all; at most four faucet payments wait at once. A payment that may have reached the network is never sent again.
- At most two tasks per wallet and twelve in all; at most four passphrase checks at once (others wait their turn a few seconds); at most three commands that prove at once; a request must arrive within 30 seconds.
- Passphrases are at least 12 characters. Ten passphrase checks a minute per visitor, right or wrong. Five wrong tries in a row make that visitor wait one minute, then longer each time, up to an hour; the count is forgotten an hour after the last wrong try. Wrong tries never hold back the wallet's owner on another connection, nor a passkey sign-in.
- A session lasts 12 hours; a wallet nobody used for two hours is locked again. An open shop keeps signing offers while its owner is away (the page says so at sign-out). After a restart of the server every shop is closed, since the server keeps no passphrase on disk; a shop its owner never closed opens again when the owner next signs in.
- While a wallet action pauses a shop, its listings are still served and offers wait for it to open again; buyers retry for a short outage.
- State files are replaced whole (a new file, then a rename), so a crash never leaves half a file. If `hub.json` or a wallet's `app.json` cannot be read, the server refuses to start rather than start empty.
- Requests must come from the site itself: the Host and Origin are checked, POSTs need a custom header, and the session cookie is `__Host-` prefixed, HttpOnly, Secure and SameSite=Strict.
- Only three requests pass to a shop: its listings, an offer, and a reply; every answer carries the server's own security headers.
- The service runs under systemd with a read-only system, no new privileges, no link-local or private addresses (so no cloud metadata), a hidden process list, and caps on memory, processes and CPU. Caddy refuses bodies above 2 MB and slow requests. The script keeps a copy of any Caddyfile it replaces.
- `--network main` is refused unless `--allow-hosted-mainnet` is given: a hosted server holds its visitors' wallets. A build from `main` refuses `--network test`, since it syncs mainnet only.
