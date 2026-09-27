#!/bin/bash
set -euxo pipefail
BUCKET=dm-datasets-827648153457
apt-get update -y
DEBIAN_FRONTEND=noninteractive apt-get install -y unzip curl
curl -sSL "https://awscli.amazonaws.com/awscli-exe-linux-x86_64.zip" -o /tmp/awscli.zip
(cd /tmp && unzip -q awscli.zip && ./aws/install)
latest="$(aws s3 ls --no-sign-request --recursive s3://osm-pds/ | grep -o '[0-9]\{4\}/planet-[0-9]*\.osm\.pbf$' | sort -t/ -k2 | tail -1)"
version="$(basename "$latest" .osm.pbf)"
mkdir -p /data
aws s3 cp --no-sign-request --only-show-errors "s3://osm-pds/$latest" /data/planet.osm.pbf
aws s3 cp --only-show-errors "s3://$BUCKET/releases/dm-build" /usr/local/bin/dm-build
chmod +x /usr/local/bin/dm-build
dm-build --input /data/planet.osm.pbf --output /data/dataset 2>&1 | tee /var/log/dm-build.log
name="$version-$(python3 -c 'import json, sys; m = json.load(open(sys.argv[1])); print(m["profile"] + "-f" + str(m["format_version"]))' /data/dataset/manifest.json)"
aws s3 cp --only-show-errors --recursive /data/dataset "s3://$BUCKET/datasets/$name"
aws s3 cp --only-show-errors /var/log/dm-build.log "s3://$BUCKET/datasets/$name.build.log"
shutdown -h now
