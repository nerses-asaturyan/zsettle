# Hosting the demo

`zsettle-app --public https://<domain>` serves many people from one server. Each visitor
creates a wallet of their own (passkey, or a wallet code and a passphrase), can get free test
coins once, buys from any open shop, and can open a shop of their own at
`https://<domain>/shop/<code>`. A house shop keeps a few items listed and makes new ones when
they sell.

The demo is for the testnet. The server holds every demo wallet. That is fine for free test
coins and wrong for real money: for real ZEC, people run zsettle on their own computer.

## What it needs

- A small Linux server that stays on. The setup script is written for Ubuntu 24.04. 2 CPUs and
  4 GB of memory are enough (for example a Hetzner CX22, about 4 euros a month).
- A domain name pointing at the server. A free DuckDNS name works.
- The three binaries: `zsettle`, `zsettle-seller`, `zsettle-app`. The same build serves the
  testnet and mainnet; see [Networks](reference/networks.md).

## Setup

On the server, as root:

```
./deploy/setup.sh demo.example.org /path/to/binaries
```

The script (`deploy/setup.sh`):

1. installs Caddy, ufw and CA certificates;
2. creates a `zsettle` system user and the data directory `/var/lib/zsettle` (owner-only);
3. writes a random house passphrase to `/var/lib/zsettle/house.pass` (only the first time);
4. installs the three binaries to `/usr/local/bin`;
5. writes the systemd unit `zsettle-demo.service`, which runs
   `zsettle-app --public https://<domain> --port 8080 --network test --data-dir /var/lib/zsettle/data --house-passphrase-file /var/lib/zsettle/house.pass --bin-dir /usr/local/bin`
   and restarts it by itself;
6. writes the Caddyfile: HTTPS with an automatic certificate, a reverse proxy to
   `127.0.0.1:8080`, gzip, HSTS;
7. opens ports 22, 80 and 443 with ufw;
8. starts the service and prints the house wallet's address.

Send the house wallet testnet coins once. 0.05 TAZ covers the house items and the test coins
for about fifteen visitors (each new wallet gets 300,000 zatoshis).

Running the script again upgrades the binaries and keeps the data.

## Running it

```
systemctl status zsettle-demo
journalctl -u zsettle-demo -f
```

Data lives in `/var/lib/zsettle/data`:

- one directory per wallet, named by its wallet code (the house wallet is `house`);
- `hub.json`: which passkey belongs to which wallet, each shop's port, who got test coins;
- `prf.salt`: the site's passkey salt. Keep it, or every passkey stops working.

State files are replaced whole (a new file, then a rename), so a crash never leaves half a
file. If `hub.json` or a wallet's `app.json` cannot be read, the server refuses to start rather
than start empty: restore the file from a backup, or move it away to start empty.

A wallet that was never synced, got no test coins and listed nothing is removed after three
days, so wallets made and left do not fill the server.

## The hosted-mode flags

| Flag | Default | Meaning |
|---|---|---|
| `--public <origin>` | | Turns on hosted mode. The public address as visitors type it, for example `https://demo.example`. Must be `https://` (plain `http://` only for `localhost` or `127.0.0.1` tests) and a bare origin. |
| `--network <main\|test>` | `test` | Network that new wallets are made on. A `main` build refuses `test`. |
| `--house-passphrase-file <file>` | | With it, a house shop is created if missing, unlocked at start, kept stocked, and pays the test coins. |
| `--allow-hosted-mainnet` | off | Needed for `--network main`: the server would hold real money for its visitors. |
| `--port`, `--data-dir`, `--bin-dir` | | As in local mode. The app always listens on 127.0.0.1; the TLS proxy forwards to it. |

## What the setup protects

The systemd unit runs the app as the `zsettle` user with, among others:
`NoNewPrivileges`, `ProtectSystem=strict` (only `/var/lib/zsettle` is writable),
`ProtectHome`, `PrivateTmp`, `PrivateDevices`, a system call filter, `MemoryDenyWriteExecute`,
`ProtectProc=invisible` (a hidden process list), an `IPAddressDeny` list that blocks
link-local and private ranges (`169.254.0.0/16`, `fe80::/10`, `10.0.0.0/8`, `172.16.0.0/12`,
`192.168.0.0/16`, `100.64.0.0/10`, `fc00::/7`; so no cloud metadata, while loopback stays open
for the app's own shops), `MemoryMax=3G`, `TasksMax=512` and `CPUQuota=180%`.

Caddy refuses request bodies above 2 MB and has timeouts of 10 seconds for headers, 30 seconds
for bodies and 60 seconds idle. It does not buffer request bodies (the Caddy that Ubuntu 24.04
ships, 2.6, has no such setting): a slow sender holds one of the app's 200 connections for up
to 30 seconds, and the app counts calls per visitor. If a Caddyfile not written by this script is
already there, the script keeps a copy (`/etc/caddy/Caddyfile.before-zsettle.<time>`) before
replacing it.

The app's own limits (wallets per hour, shops, sign-in tries, sessions) are listed in
[Security model and limits](security.md#hosted-mode).

## Linking the demo from the project page

The project page `docs/index.html` has a `DEMO_URL` constant. Set it to the demo's address
once the server is up; until then the page shows "Live demo coming soon".
