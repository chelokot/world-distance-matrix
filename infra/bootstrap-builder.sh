#!/bin/bash
set -euxo pipefail
BUCKET=dm-datasets-827648153457
apt-get update -y
DEBIAN_FRONTEND=noninteractive apt-get install -y unzip curl
curl -sSL "https://awscli.amazonaws.com/awscli-exe-linux-x86_64.zip" -o /tmp/awscli.zip
(cd /tmp && unzip -q awscli.zip && ./aws/install)
latest="$(for year in "$(date +%Y)" "$(( $(date +%Y) - 1 ))"; do aws s3 ls --no-sign-request "s3://osm-pds/$year/" | grep -o "planet-[0-9]*\.osm\.pbf$" | sed "s|^|$year/|"; done | sort -t/ -k2 | tail -1)"
version="$(basename "$latest" .osm.pbf)"
mkdir -p /data
aws s3 cp --no-sign-request --only-show-errors "s3://osm-pds/$latest" /data/planet.osm.pbf
aws s3 cp --only-show-errors "s3://$BUCKET/releases/dm-build" /usr/local/bin/dm-build
chmod +x /usr/local/bin/dm-build
dm-build --input /data/planet.osm.pbf --output "/data/$version" 2>&1 | tee /var/log/dm-build.log
aws s3 cp --only-show-errors --recursive "/data/$version" "s3://$BUCKET/datasets/$version"
aws s3 cp --only-show-errors /var/log/dm-build.log "s3://$BUCKET/datasets/$version.build.log"
shutdown -h now
