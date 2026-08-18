#!/usr/bin/env bash

# 用法：
#   ./aaa_mine/script/build-local.sh
#   ./aaa_mine/script/build-local.sh --release
#
# 脚本会下载或复用与当前 Apple Silicon macOS 构建匹配的 Codex V8
# sandbox 预编译归档，然后构建 codex 与 codex-code-mode-host。
# V8 资产缓存于：~/.cache/codex/rusty-v8/150.4.0/aarch64-apple-darwin
# 需要预先安装：cargo、curl。

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repository_dir="$(cd "$script_dir/../.." && pwd)"
v8_cache_dir="${XDG_CACHE_HOME:-$HOME/.cache}/codex/rusty-v8/150.4.0/aarch64-apple-darwin"
archive_name="librusty_v8_ptrcomp_sandbox_release_aarch64-apple-darwin.a.gz"
binding_name="src_binding_ptrcomp_sandbox_release_aarch64-apple-darwin.rs"
release_url="https://github.com/openai/codex/releases/download/rusty-v8-v150.4.0"

mkdir -p "$v8_cache_dir"

download_asset() {
  local asset_name="$1"
  local asset_path="$v8_cache_dir/$asset_name"

  if [[ -s "$asset_path" ]]; then
    return
  fi

  local partial_path="$asset_path.partial"
  rm -f "$partial_path"
  curl --fail --location --retry 3 --output "$partial_path" "$release_url/$asset_name"
  mv "$partial_path" "$asset_path"
}

download_asset "$archive_name"
download_asset "$binding_name"

cd "$repository_dir/codex-rs"

RUSTY_V8_ARCHIVE="$v8_cache_dir/$archive_name" \
RUSTY_V8_SRC_BINDING_PATH="$v8_cache_dir/$binding_name" \
cargo build \
  -p codex-cli --bin codex \
  -p codex-code-mode-host --bin codex-code-mode-host \
  "$@"
