#!/bin/bash
set -euo pipefail
binary="$1"
dataset="$2"
version="$(basename "$dataset")"
id -u dm >/dev/null 2>&1 || useradd --system --no-create-home --shell /usr/sbin/nologin dm
install -d -o root -g root /opt/dm/bin /opt/dm/data
install -m 0755 "$binary" /opt/dm/bin/dm-server.new
mv /opt/dm/bin/dm-server.new /opt/dm/bin/dm-server
if [ "$(readlink -f "$dataset")" != "/opt/dm/data/$version" ]; then
  rm -rf "/opt/dm/data/$version.tmp"
  cp -r "$dataset" "/opt/dm/data/$version.tmp"
  mv "/opt/dm/data/$version.tmp" "/opt/dm/data/$version"
fi
ln -sfn "/opt/dm/data/$version" /opt/dm/data/current.new
mv -T /opt/dm/data/current.new /opt/dm/data/current
here="$(dirname "$0")"
install -m 0644 "$here/dm-server.service" /etc/systemd/system/dm-server.service
[ -f /etc/dm-server.env ] || install -m 0644 "$here/dm-server.env" /etc/dm-server.env
install -m 0644 "$here/90-dm-server.conf" /etc/sysctl.d/90-dm-server.conf
sysctl --system >/dev/null
systemctl daemon-reload
systemctl enable dm-server >/dev/null
systemctl restart dm-server
for _ in $(seq 1 180); do
  if curl -fsS http://127.0.0.1:8080/health >/dev/null 2>&1; then
    echo "dm-server is serving dataset $version"
    exit 0
  fi
  sleep 1
done
echo "dm-server did not become healthy" >&2
journalctl -u dm-server -n 50 --no-pager >&2
exit 1
