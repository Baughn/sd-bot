#!/usr/bin/env nix-shell
#!nix-shell -i bash -p shellcheck
# shellcheck shell=bash
#
# Start script that sets bot token, etc.

set -euo pipefail
cd "$(dirname "$(readlink -f "$0")")"

shellcheck "$0"

if [[ ! -e .env ]]; then
  echo 'Please edit the .env file to add your bot token'
  echo 'export DISCORD_BOT_TOKEN=your_token_here' > .env
  exit 1
fi

# shellcheck source=/dev/null
source .env

if [[ "${DISCORD_BOT_TOKEN}" == "your_token_here" ]]; then
  echo 'Please edit the .env file to add your bot token'
  exit 1
fi

# For debugging
export RUST_LOG="${RUST_LOG:-info,sd_bot_2,tracing=warn,serenity=warn}"
#export RUST_BACKTRACE=1

nix-shell --run "cargo run --release"
