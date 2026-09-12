#!/usr/bin/env bash
set -euo pipefail

EXPECTED_COMMIT="73a1148c9c775c2a4616ce5096291740a00ed68a"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PATCH_FILE="$SCRIPT_DIR/0001-per-turn-local-router.patch"

usage() {
  printf 'usage: %s PATH-TO-CODEX-RS\n' "$(basename "$0")" >&2
  printf 'PATH may be the codex-rs checkout or its parent containing codex-rs/.\n' >&2
}

if [[ $# -ne 1 ]]; then
  usage
  exit 64
fi

INPUT_DIR="$(cd -- "$1" && pwd)"
if [[ -f "$INPUT_DIR/core/src/client.rs" ]]; then
  TARGET_DIR="$INPUT_DIR"
elif [[ -f "$INPUT_DIR/codex-rs/core/src/client.rs" ]]; then
  TARGET_DIR="$INPUT_DIR/codex-rs"
else
  printf 'error: %s does not look like a Codex source checkout\n' "$INPUT_DIR" >&2
  exit 1
fi

GIT_ROOT="$(git -C "$TARGET_DIR" rev-parse --show-toplevel)"
PREFIX="$(git -C "$TARGET_DIR" rev-parse --show-prefix)"
HEAD="$(git -C "$GIT_ROOT" rev-parse HEAD)"
if [[ "$HEAD" != "$EXPECTED_COMMIT" ]]; then
  printf 'error: expected upstream HEAD %s, found %s\n' "$EXPECTED_COMMIT" "$HEAD" >&2
  exit 1
fi

APPLY_ARGS=()
if [[ -n "$PREFIX" ]]; then
  APPLY_ARGS+=("--directory=${PREFIX%/}")
fi

if git -C "$GIT_ROOT" -c core.whitespace=error apply --reverse --check \
  "${APPLY_ARGS[@]}" "$PATCH_FILE" >/dev/null 2>&1; then
  printf 'patch already applied to %s\n' "$TARGET_DIR"
  exit 0
fi

git -C "$GIT_ROOT" -c core.whitespace=error apply --check "${APPLY_ARGS[@]}" "$PATCH_FILE"
git -C "$GIT_ROOT" apply "${APPLY_ARGS[@]}" "$PATCH_FILE"
printf 'applied per-turn local-router patch to %s\n' "$TARGET_DIR"
