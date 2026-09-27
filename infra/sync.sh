#!/bin/sh
set -e
cd "$(dirname "$0")/.."
tar --exclude=./target --exclude=./.ssh --exclude=./.aws-credentials --exclude=./results/raw -czf - . | ssh -F .ssh/config "${1:-dm-dev}" 'mkdir -p ~/dm && cd ~/dm && tar -xzf -'
