#!/bin/sh
set -eu

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
binary_path="$("$script_dir/ensure-runtime.sh")"
exec "$binary_path" mcp-server
