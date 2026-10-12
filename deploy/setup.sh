#!/usr/bin/env bash
# Set up the hosted zsettle demo on a fresh Ubuntu 24.04 server, in one command.
#
#   sudo ./setup.sh <domain> <directory with zsettle, zsettle-seller and zsettle-app>
#
# It installs Caddy (HTTPS with an automatic certificate), creates a service user, installs
# the three binaries, writes a house passphrase, and runs zsettle-app in hosted mode as a
# service that restarts by itself. Run it again to upgrade the binaries; data is kept.
set -euo pipefail

DOMAIN=${1:?usage: setup.sh <domain> <binary directory>}
BIN=${2:?usage: setup.sh <domain> <binary directory>}
DATA=/var/lib/zsettle
PORT=8080

if [ "$(id -u)" != 0 ]; then echo "run as root (sudo)"; exit 1; fi
for b in zsettle zsettle-seller zsettle-app; do
  [ -x "$BIN/$b" ] || { echo "missing $BIN/$b"; exit 1; }
done

echo "== packages"
apt-get update -qq
apt-get install -y -qq caddy ufw ca-certificates >/dev/null

echo "== service user and data"
id zsettle >/dev/null 2>&1 || useradd --system --home-dir "$DATA" --shell /usr/sbin/nologin zsettle
install -d -o zsettle -g zsettle -m 700 "$DATA" "$DATA/data"
if [ ! -s "$DATA/house.pass" ]; then
  head -c 32 /dev/urandom | base64 | tr -d '/+=\n' > "$DATA/house.pass"
fi
chown zsettle:zsettle "$DATA/house.pass"; chmod 600 "$DATA/house.pass"

echo "== binaries"
systemctl stop zsettle-demo 2>/dev/null || true
install -m 755 "$BIN/zsettle" "$BIN/zsettle-seller" "$BIN/zsettle-app" /usr/local/bin/

echo "== service"
cat > /etc/systemd/system/zsettle-demo.service <<UNIT
[Unit]
Description=zsettle hosted demo
After=network-online.target
Wants=network-online.target

[Service]
User=zsettle
Group=zsettle
ExecStart=/usr/local/bin/zsettle-app --public https://$DOMAIN --port $PORT --network test --data-dir $DATA/data --house-passphrase-file $DATA/house.pass --bin-dir /usr/local/bin
Restart=always
RestartSec=5
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=$DATA
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectKernelLogs=true
ProtectControlGroups=true
ProtectClock=true
PrivateDevices=true
RestrictSUIDSGID=true
RestrictNamespaces=true
RestrictRealtime=true
LockPersonality=true
MemoryDenyWriteExecute=true
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
SystemCallFilter=@system-service
SystemCallArchitectures=native
CapabilityBoundingSet=
UMask=0077
# No cloud metadata or other link-local addresses from this service.
IPAddressDeny=169.254.0.0/16 fe80::/10
LimitNOFILE=4096
TasksMax=512
MemoryMax=3G
CPUQuota=180%

[Install]
WantedBy=multi-user.target
UNIT

echo "== HTTPS"
cat > /etc/caddy/Caddyfile <<CADDY
{
	servers {
		timeouts {
			read_body 30s
			read_header 10s
			idle 60s
		}
	}
}

$DOMAIN {
	encode gzip
	request_body {
		max_size 2MB
	}
	reverse_proxy 127.0.0.1:$PORT
	header Strict-Transport-Security "max-age=31536000"
}
CADDY

echo "== firewall"
ufw allow 22/tcp >/dev/null
ufw allow 80/tcp >/dev/null
ufw allow 443/tcp >/dev/null
ufw --force enable >/dev/null

systemctl daemon-reload
systemctl enable --now zsettle-demo >/dev/null
systemctl reload caddy || systemctl restart caddy

sleep 3
echo
echo "Done. The demo is at https://$DOMAIN"
echo "House wallet address (fund it with testnet coins once):"
journalctl -u zsettle-demo --no-pager -n 50 | grep -o 'utest1[0-9a-z]*' | tail -1 || echo "  (already funded earlier, or see: journalctl -u zsettle-demo)"
