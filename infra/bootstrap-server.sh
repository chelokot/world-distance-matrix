#!/bin/bash
set -euxo pipefail
BUCKET=dm-datasets-827648153457
DATASET="$(cat /etc/dm-dataset-version 2>/dev/null || echo __DATASET__)"
apt-get update -y
DEBIAN_FRONTEND=noninteractive apt-get install -y unzip curl
if ! command -v aws >/dev/null; then
  curl -sSL "https://awscli.amazonaws.com/awscli-exe-linux-x86_64.zip" -o /tmp/awscli.zip
  (cd /tmp && unzip -q awscli.zip && ./aws/install)
fi
mkdir -p /opt/dm-release
aws s3 cp --only-show-errors --recursive "s3://$BUCKET/releases/deploy" /opt/dm-release/deploy
chmod +x /opt/dm-release/deploy/*.sh
/opt/dm-release/deploy/upgrade.sh "$DATASET"
