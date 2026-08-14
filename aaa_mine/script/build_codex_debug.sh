#!/usr/bin/env bash
# Build the development Codex CLI and its Code Mode host, then stage both
# executables under aaa_mine/target.

set -euo pipefail

usage() {
    cat <<EOF
用法：$(basename "$0") [debug|release|--debug|--release] [--v8-from-source]

构建 Codex CLI（codex）和 Code Mode host（codex-code-mode-host），并复制到：
  $repo_root/aaa_mine/target/<profile>/

构建类型：
  debug、--debug       Debug 构建（默认）。产物位于 target/debug/。
  release、--release   Release 构建。产物位于 target/release/。

选项：
  --v8-from-source     强制从源码构建 V8。
  -h、--help           显示本说明。

示例：
  $(basename "$0")
  $(basename "$0") release

说明：
  macOS Apple Silicon（arm64）缺少所需 V8 预编译包，脚本会自动改为从源码构建 V8。
EOF
}

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"
rust_workspace="$repo_root/codex-rs"

build_profile=debug
build_profile_set=false
build_v8_from_source=false
for argument in "$@"; do
    case "$argument" in
        debug|--debug)
            if [[ "$build_profile_set" == true ]]; then
                printf 'error: build profile was specified more than once\n' >&2
                usage >&2
                exit 2
            fi
            build_profile=debug
            build_profile_set=true
            ;;
        release|--release)
            if [[ "$build_profile_set" == true ]]; then
                printf 'error: build profile was specified more than once\n' >&2
                usage >&2
                exit 2
            fi
            build_profile=release
            build_profile_set=true
            ;;
        --v8-from-source)
            build_v8_from_source=true
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            usage >&2
            exit 2
            ;;
    esac
done

# rusty_v8 does not publish the required prebuilt archive for this target.
# Select a source build automatically so the usual no-argument invocation works.
if [[ "$(uname -s)" == Darwin && "$(uname -m)" == arm64 ]]; then
    build_v8_from_source=true
fi

build_output_dir="$rust_workspace/target/$build_profile"
staging_root="$repo_root/aaa_mine/target"
staging_dir="$staging_root/$build_profile"

cli_binary="$build_output_dir/codex"
code_mode_host_binary="$build_output_dir/codex-code-mode-host"

if ! command -v cargo >/dev/null 2>&1; then
    printf 'error: cargo was not found in PATH. Install Rust, then load its environment (for example: source "$HOME/.cargo/env").\n' >&2
    exit 1
fi

if [[ "$build_v8_from_source" == true ]]; then
    if ! command -v brew >/dev/null 2>&1; then
        printf 'error: Homebrew is required to locate LLVM. Install it, then run: brew install llvm\n' >&2
        exit 1
    fi

    llvm_lib_dir="$(brew --prefix llvm)/lib"
    if [[ ! -d "$llvm_lib_dir" ]]; then
        printf 'error: Homebrew LLVM was not found. Run: brew install llvm\n' >&2
        exit 1
    fi
    if [[ ! -x /usr/bin/python3 ]]; then
        printf 'error: /usr/bin/python3 was not found. Install the macOS command line tools or Xcode.\n' >&2
        exit 1
    fi
fi

printf 'Building Codex CLI and Code Mode host'
printf ' (%s)' "$build_profile"
if [[ "$build_v8_from_source" == true ]]; then
    printf ' with V8 from source'
fi
printf '...\n'
cargo_arguments=(cargo build)
if [[ "$build_profile" == release ]]; then
    cargo_arguments+=(--release)
fi
cargo_arguments+=(
    -p codex-cli --bin codex
    -p codex-code-mode-host --bin codex-code-mode-host
)
(
    cd "$rust_workspace"
    if [[ "$build_v8_from_source" == true ]]; then
        V8_FROM_SOURCE=1 PYTHON=/usr/bin/python3 LIBCLANG_PATH="$llvm_lib_dir" "${cargo_arguments[@]}"
    else
        "${cargo_arguments[@]}"
    fi
)

for binary in "$cli_binary" "$code_mode_host_binary"; do
    if [[ ! -f "$binary" ]]; then
        printf 'error: expected build artifact was not produced: %s\n' "$binary" >&2
        exit 1
    fi
done

mkdir -p "$staging_root/debug" "$staging_root/release"
install -m 755 "$cli_binary" "$staging_dir/codex"
install -m 755 "$code_mode_host_binary" "$staging_dir/codex-code-mode-host"

printf '\nBuild and staging complete:\n'
printf '  %s\n' "$staging_dir/codex"
printf '  %s\n' "$staging_dir/codex-code-mode-host"
