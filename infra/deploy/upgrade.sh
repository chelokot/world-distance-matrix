#!/bin/bash
set -euo pipefail
version="$1"
bucket=dm-datasets-827648153457
release="$(cd "$(dirname "$0")/.." && pwd)"
target="/opt/dm/data/$version"
aws s3 cp --only-show-errors "s3://$bucket/releases/dm-server" "$release/dm-server"
if [ ! -d "$target" ]; then
  mkdir -p /opt/dm/data
  aws s3 cp --only-show-errors --recursive "s3://$bucket/datasets/$version" "$target.download"
  mv "$target.download" "$target"
fi
"$release/deploy/install.sh" "$release/dm-server" "$target"
find /opt/dm/data -mindepth 1 -maxdepth 1 -type d ! -name "$version" -printf "%T@ %p\n" | sort -n | head -n -1 | cut -d" " -f2- | xargs -r rm -rf
