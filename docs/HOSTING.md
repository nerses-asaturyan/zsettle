# Hosting the public demo

`zsettle-app --public https://<domain>` serves many people from one server. Each visitor creates a wallet of their own (passkey, or a wallet code and a passphrase), can get free test coins once, buys from any open shop, and can open a shop of their own at `https://<domain>/shop/<code>`. A house shop keeps a few items listed and makes new ones when they sell.

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

- 40 new wallets an hour and 300 in total; 25 shops open at once.
- A session lasts 12 hours; a wallet nobody used for two hours is locked again.
- A failed sign-in waits a second, so codes and passphrases cannot be tried quickly.
- Requests must come from the site itself: the Host and Origin are checked, POSTs need a custom header, and the session cookie is HttpOnly, Secure and SameSite=Strict.
- Only three requests pass to a shop: its listings, an offer, and a reply.
