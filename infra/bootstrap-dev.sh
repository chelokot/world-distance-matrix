#!/bin/bash
set -eux
apt-get update -y
DEBIAN_FRONTEND=noninteractive apt-get install -y build-essential pkg-config clang lld cmake git jq zstd htop sysstat iperf3 osmium-tool python3-pip python3-venv docker.io unzip
sysctl -w net.ipv4.tcp_slow_start_after_idle=0
usermod -aG docker ubuntu
sudo -u ubuntu bash -c 'curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable'
touch /var/tmp/bootstrap-done
