#!/usr/bin/env bash
set -euo pipefail

EXPECTED_COMMIT="73a1148c9c775c2a4616ce5096291740a00ed68a"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PATCH_FILE="$SCRIPT_DIR/0001-per-turn-local-router.patch"
SHA256SUMS_FILE="$SCRIPT_DIR/SHA256SUMS"

usage() {
  printf 'usage: %s PATH-TO-CODEX-RS\n' "$(basename "$0")" >&2
  printf '       %s --verify-checksums\n' "$(basename "$0")" >&2
  printf 'PATH may be the codex-rs checkout or its parent containing codex-rs/.\n' >&2
  printf '%s\n' '--verify-checksums checks SHA256SUMS and exits; no checkout is required.' >&2
}

# SHA256SUMS is mandatory: manifest.json declares it and the patch is applied to a
# third-party checkout, so an unverified patch file must never reach `git apply`.
#
# The recorded paths are relative to the repository root, while this script is
# invoked from an arbitrary CWD (scripts/build-patched-codex.sh calls it from
# wherever the caller happened to be). Resolve each entry against this script's
# own location instead of the CWD:
#   1. find the ancestor of SCRIPT_DIR under which the recorded path resolves
#      (the normal in-repo layout <root>/patches/codex/<commit>/);
#   2. otherwise fall back to the file travelling next to this script, which is
#      what a copied/vendored patch directory looks like.
# The digest itself is always checked with `sha256sum -c`.
verify_patch_checksums() {
  if [[ ! -f "$SHA256SUMS_FILE" ]]; then
    printf 'error: checksum file not found at %s\n' "$SHA256SUMS_FILE" >&2
    return 1
  fi

  local line hash rel dir parent root relative
  local entries=0
  while IFS= read -r line || [[ -n "$line" ]]; do
    line="${line%$'\r'}"
    [[ -z "$line" || "$line" == \#* ]] && continue

    read -r hash rel <<<"$line"
    rel="${rel#\*}"
    if [[ -z "$hash" || -z "$rel" ]]; then
      printf 'error: malformed line in %s: %s\n' "$SHA256SUMS_FILE" "$line" >&2
      return 1
    fi

    root=""
    relative="$rel"
    dir="$SCRIPT_DIR"
    while :; do
      if [[ -f "$dir/$rel" ]]; then
        root="$dir"
        break
      fi
      parent="$(dirname -- "$dir")"
      [[ "$parent" == "$dir" ]] && break
      dir="$parent"
    done

    if [[ -z "$root" ]]; then
      root="$SCRIPT_DIR"
      relative="$(basename -- "$rel")"
      if [[ ! -f "$root/$relative" ]]; then
        printf 'error: %s records %s, which was found neither at the repository root nor next to %s\n' \
          "$SHA256SUMS_FILE" "$rel" "$SCRIPT_DIR" >&2
        return 1
      fi
    fi

    if ! ( cd -- "$root" && sha256sum -c --strict <(printf '%s  %s\n' "$hash" "$relative") ); then
      printf 'error: checksum mismatch in %s; refusing to apply a drifted patch\n' "$SHA256SUMS_FILE" >&2
      return 1
    fi
    entries=$((entries + 1))
  done <"$SHA256SUMS_FILE"

  if [[ "$entries" -eq 0 ]]; then
    printf 'error: %s contains no checksum entries\n' "$SHA256SUMS_FILE" >&2
    return 1
  fi
}

if [[ $# -eq 1 && ( "$1" == "--verify-checksums" || "$1" == "--verify-only" ) ]]; then
  verify_patch_checksums
  printf 'verified patch checksums declared by %s\n' "$SHA256SUMS_FILE"
  exit 0
fi

if [[ $# -ne 1 ]]; then
  usage
  exit 64
fi

verify_patch_checksums

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
