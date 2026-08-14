#!/usr/bin/env bash
set -euo pipefail

script_root="$(cd "$(dirname "$0")" && pwd)"
exec bash "$script_root/package-macos-apple-silicon.sh" "$@"
