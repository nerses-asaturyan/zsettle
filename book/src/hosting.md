# Hosting the demo

`zsettle-app --public https://<domain> --web-wallet <dir>` serves many people from one
server. The app at `/` is the wallet, and it runs in each visitor's browser: the keys stay on
the visitor's device, and the server keeps no visitor wallets. A visitor makes or restores a
wallet in the page, can get free test coins once, buys from any open shop, and can open a shop
of their own at `https://<domain>/shop/<code>` while the page is open. A house shop keeps a few
items listed and makes new ones when they sell.

The wallet engine is `crates/zsettle-web`: build it with `crates/zsettle-web/build.sh` and pass
`crates/zsettle-web/www` to `--web-wallet`. Hosted mode does not start without it. See
[The app in your browser](start/browser-wallet.md).

The server holds one wallet of its own: the house shop's. It pays the test coins, so run the
house shop on the testnet.

## What it needs

- A small Linux server that stays on. The setup script is written for Ubuntu 24.04. 2 CPUs and
  4 GB of memory are enough (for example a Hetzner CX22, about 4 euros a month).
- A domain name pointing at the server. A free DuckDNS name works.
- The three binaries: `zsettle`, `zsettle-seller`, `zsettle-app`. The same build serves the
  testnet and mainnet; see [Networks](reference/networks.md).
- The built wallet engine: the `www/` directory of `crates/zsettle-web` after
  `crates/zsettle-web/build.sh` (see `crates/zsettle-web/README.md` for its toolchain).

## Setup

On the server, as root:

```
./deploy/setup.sh demo.example.org /path/to/binaries /path/to/zsettle-web/www
```

The script (`deploy/setup.sh`):

1. installs Caddy, ufw and CA certificates;
2. creates a `zsettle` system user and the data directory `/var/lib/zsettle` (owner-only);
3. writes a random house passphrase to `/var/lib/zsettle/house.pass` (only the first time);
4. installs the three binaries to `/usr/local/bin`, and copies the wallet engine (the third
   argument) to `/usr/local/share/zsettle-web/www`;
5. writes the systemd unit `zsettle-demo.service`, which runs
   `zsettle-app --public https://<domain> --port 8080 --network test --data-dir /var/lib/zsettle/data --house-passphrase-file /var/lib/zsettle/house.pass --bin-dir /usr/local/bin --web-wallet /usr/local/share/zsettle-web/www`
   and restarts it by itself;
6. writes the Caddyfile: HTTPS with an automatic certificate, a reverse proxy to
   `127.0.0.1:8080`, compression (the wallet engine's WebAssembly included), HSTS;
7. opens ports 22, 80 and 443 with ufw;
8. starts the service and prints the house wallet's address.

Send the house wallet testnet coins once. 0.05 TAZ covers the house items and the test coins
for about fifteen visitors (each address gets 300,000 zatoshis).

Running the script again upgrades the binaries and the wallet engine, and keeps the data.

## Running it

```
systemctl status zsettle-demo
journalctl -u zsettle-demo -f
```

Data lives in `/var/lib/zsettle/data`:

- `house/`: the house shop's wallet and shop;
- `hub.json`: the house shop's port and the addresses that got test coins.

The browser shops' mailboxes are kept in memory only, so a restart of the server empties
them; a shop whose page is still open opens again by itself the next time it asks for offers. Folders and files left by an older
hosted mode (visitor wallets, `prf.salt`) are not loaded; you may move them away.

State files are replaced whole (a new file, then a rename), so a crash never leaves half a
file. If `hub.json` or the house's `app.json` cannot be read, the server refuses to start
rather than start empty: restore the file from a backup, or move it away to start empty.

## The hosted-mode flags

| Flag | Default | Meaning |
|---|---|---|
| `--public <origin>` | | Turns on hosted mode. The public address as visitors type it, for example `https://demo.example`. Must be `https://` (plain `http://` only for `localhost` or `127.0.0.1` tests) and a bare origin. |
| `--web-wallet <dir>` | | Required. The wallet engine (`crates/zsettle-web/www`, with `pkg/` inside), served with the page. It is read once at start. |
| `--network <main\|test>` | `test` | Network of the house shop and its faucet. Visitors' wallets run in their browsers, on either network, and the relay serves both. Test coins are given only on `test`. |
| `--house-passphrase-file <file>` | | With it, a house shop is created if missing, unlocked at start, kept stocked, and pays the test coins. |
| `--port`, `--data-dir`, `--bin-dir` | | As in local mode. The app always listens on 127.0.0.1; the TLS proxy forwards to it. |

## What the setup protects

The systemd unit runs the app as the `zsettle` user with, among others:
`NoNewPrivileges`, `ProtectSystem=strict` (only `/var/lib/zsettle` is writable),
`ProtectHome`, `PrivateTmp`, `PrivateDevices`, a system call filter, `MemoryDenyWriteExecute`,
`ProtectProc=invisible` (a hidden process list), an `IPAddressDeny` list that blocks
link-local and private ranges (`169.254.0.0/16`, `fe80::/10`, `10.0.0.0/8`, `172.16.0.0/12`,
`192.168.0.0/16`, `100.64.0.0/10`, `fc00::/7`; so no cloud metadata, while loopback stays open
for the house shop), `MemoryMax=3G`, `TasksMax=512` and `CPUQuota=180%`.

Caddy refuses request bodies above 2 MB and has timeouts of 10 seconds for headers, 30 seconds
for bodies and 60 seconds idle. It does not buffer request bodies (the Caddy that Ubuntu 24.04
ships, 2.6, has no such setting): a slow sender holds one of the app's 200 connections for up
to 30 seconds, and the app counts calls per visitor. If a Caddyfile not written by this script is
already there, the script keeps a copy (`/etc/caddy/Caddyfile.before-zsettle.<time>`) before
replacing it.

The app's own limits (relay calls, mailboxes, test coins) are listed in
[Security model and limits](security.md#hosted-mode).

## Linking the demo from the project page

The project page `docs/index.html` has a `DEMO_URL` constant. Set it to the demo's address
once the server is up; until then the page shows "Live demo coming soon".
