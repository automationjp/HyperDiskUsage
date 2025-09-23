#!/usr/bin/env bash
set -euo pipefail

# Build with Cargo, printing produced executables after a successful build.
# This is a convenience wrapper that forwards all args to `cargo build`.
#
# Usage:
#   scripts/build_print.sh [CARGO_BUILD_ARGS...]
#
# Examples:
#   scripts/build_print.sh -p hyperdu-cli --release
#   scripts/build_print.sh -p hyperdu-gui
#   HYPERDU_LOG=1 scripts/build_print.sh -p hyperdu-cli --release --target x86_64-unknown-linux-musl
#
# Environment:
#   HYPERDU_TIMINGS=1      -> pass `--timings` to cargo (report in target/cargo-timings)
#   HYPERDU_SELF_PROFILE=1 -> add `-Z self-profile` to RUSTFLAGS (nightly required)
#   HYPERDU_NIGHTLY=1      -> prefer `cargo +nightly`
#   HYPERDU_LOG=1          -> print verbose cargo logs and tee to dist/build_*.log

usage() {
  awk 'NR<=30 && /^#( |$)/ { sub(/^# ?/, ""); print }' "$0"
  echo
  echo "Prints built executables (unique paths) to stdout."
}

for a in "$@"; do
  case "$a" in
    -h|--help) usage; exit 0;;
  esac
done

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")"/.. && pwd)"

# Extract optional package/target for log naming
pkg="unknown"
triple="host"
args=("$@")
for ((i=0; i<${#args[@]}; i++)); do
  if [[ "${args[$i]}" == "-p" && $((i+1)) -lt ${#args[@]} ]]; then pkg="${args[$i+1]}"; fi
  if [[ "${args[$i]}" == "--target" && $((i+1)) -lt ${#args[@]} ]]; then triple="${args[$i+1]}"; fi
done

logfile="$repo_root/dist/build_${pkg}_${triple}.log"

cargo_cmd=(cargo)
if [[ "${HYPERDU_NIGHTLY:-}" == "1" || "${HYPERDU_SELF_PROFILE:-}" == "1" ]]; then
  if command -v rustup >/dev/null 2>&1 && rustup toolchain list | rg -q '^nightly'; then
    cargo_cmd=(cargo +nightly)
  fi
fi

timings_args=( )
if [[ "${HYPERDU_TIMINGS:-}" == "1" ]]; then
  timings_args+=("--timings")
fi

# Self-profile requires nightly rustc and -Z flag
if [[ "${HYPERDU_SELF_PROFILE:-}" == "1" ]]; then
  export RUSTFLAGS="${RUSTFLAGS:-} -Z self-profile"
fi

if [[ "${HYPERDU_LOG:-}" == "1" ]]; then
  mkdir -p "$repo_root/dist" || true
  echo "==> Verbose cargo build log: $logfile" >&2
  # 第一段: 人間可読な詳細ログをコンソールとファイルの両方へ
  CARGO_TERM_COLOR=always "${cargo_cmd[@]}" build -vv "${timings_args[@]}" "$@" 2>&1 | tee "$logfile"
fi

# Second pass: JSON to print produced executables (fast no-op if already built)
if command -v jq >/dev/null 2>&1; then
  if [[ "${HYPERDU_LOG:-}" == "1" ]]; then
    "${cargo_cmd[@]}" build "${timings_args[@]}" "$@" --message-format=json \
      | tee -a "$logfile" \
      | jq -r 'select(.reason=="compiler-artifact" and .executable!=null) | .executable' \
      | sort -u
  else
    "${cargo_cmd[@]}" build "${timings_args[@]}" "$@" --message-format=json \
      | jq -r 'select(.reason=="compiler-artifact" and .executable!=null) | .executable' \
      | sort -u
  fi
else
  if [[ "${HYPERDU_LOG:-}" == "1" ]]; then
    "${cargo_cmd[@]}" build "${timings_args[@]}" "$@" --message-format=json \
      | tee -a "$logfile" \
      | sed -n 's/.*"reason":"compiler-artifact".*"executable":"\([^"]*\)".*/\1/p' \
      | sort -u
  else
    "${cargo_cmd[@]}" build "${timings_args[@]}" "$@" --message-format=json \
      | sed -n 's/.*"reason":"compiler-artifact".*"executable":"\([^"]*\)".*/\1/p' \
      | sort -u
  fi
fi
