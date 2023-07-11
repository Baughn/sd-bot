#!/usr/bin/env nix-shell
#!nix-shell -i bash -p shellcheck cargo rustc
# shellcheck shell=bash
#
# Start script that sets bot token, etc.

set -euo pipefail
cd "$(dirname "$(readlink -f "$0")")"

shellcheck "$0"

export DISCORD_BOT_TOKEN="${DISCORD_BOT_TOKEN:-YOUR_TOKEN_HERE}"

if [[ "$DISCORD_BOT_TOKEN" = "YOUR_TOKEN_HERE" ]]; then
  echo 'Please edit start.sh to set your bot token, or pass it in'
  exit 1
fi

# For debugging
export RUST_LOG=info,sd_bot_2,tracing=warn,serenity=warn 
export RUST_BACKTRACE=1
export COMPILE_FLAGS=()

# For release
# export RUST_LOG=info,tracing=warn,serenity=warn
# export RUST_BACKTRACE=0
# export COMPILE_FLAGS=(--release)

cargo run "${COMPILE_FLAGS[@]}"
