#!/bin/bash
set -euo pipefail
version="$1"
bucket=dm-datasets-827648153457
target="/opt/dm/data/$version"
if [ ! -d "$target" ]; then
  aws s3 cp --only-show-errors --recursive "s3://$bucket/datasets/$version" "$target.download"
  mv "$target.download" "$target"
fi
/opt/dm-release/deploy/install.sh /opt/dm/bin/dm-server "$target"
find /opt/dm/data -mindepth 1 -maxdepth 1 -type d ! -name "$version" -printf "%T@ %p\n" | sort -n | head -n -1 | cut -d" " -f2- | xargs -r rm -rf
