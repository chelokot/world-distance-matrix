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
aws s3 cp --only-show-errors "s3://$BUCKET/releases/dm-server" /opt/dm-release/dm-server
aws s3 cp --only-show-errors --recursive "s3://$BUCKET/releases/deploy" /opt/dm-release/deploy
mkdir -p /opt/dm/data
aws s3 cp --only-show-errors --recursive "s3://$BUCKET/datasets/$DATASET" "/opt/dm/data/$DATASET"
chmod +x /opt/dm-release/deploy/install.sh
/opt/dm-release/deploy/install.sh /opt/dm-release/dm-server "/opt/dm/data/$DATASET"
