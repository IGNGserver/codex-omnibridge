#!/usr/bin/env bash
# A `sed s|__PLACEHOLDER__|${VAR}|` substitution must escape the replacement.
#
# The rendered value becomes part of a systemd unit or launchd plist that points
# the service at a registry path. `&` in a replacement means "the whole match" and
# `\` escapes, so a directory whose name contains either character was written
# wrongly:
#   * `&` re-emitted the placeholder (caught by the placeholder check), but
#   * `\` was **silently swallowed**: `/home/a\b/reg` rendered as `/home/ab/reg`,
#     every check passed, and the service read a different registry than the CLI.
#
# These paths come from the environment (`HOME`, `XDG_CONFIG_HOME`,
# `CODEX_MP_REGISTRY`, ...), so they are not "installer-controlled" as an earlier
# comment claimed. Any `s|__X__|...|` substitution in shell or PowerShell script
# must therefore route the value through an escaping helper.
#
# Run: bash scripts/check-sed-escaping.sh

set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
failures=0

while IFS= read -r file; do
  # Only lines that actually perform a placeholder substitution.
  while IFS=: read -r lineno line; do
    # Skip comments: this guard's own documentation quotes the pattern.
    trimmed="${line#"${line%%[![:space:]]*}"}"
    if [[ "${trimmed}" == \#* ]]; then
      continue
    fi
    # An escaped value is either a variable ending in _ESCAPED or a call to the
    # escaping helper.
    if [[ "${line}" == *"_ESCAPED}"* || "${line}" == *"sed_replacement"* ]]; then
      continue
    fi
    printf 'check-sed-escaping: %s:%s substitutes an unescaped value: %s\n' \
      "${file#"${ROOT}/"}" "${lineno}" "$(printf '%s' "${line}" | sed 's/^[[:space:]]*//')" >&2
    failures=$((failures + 1))
  done < <(grep -n 's|__[A-Z_]*__|' "${file}" || true)
done < <(find "${ROOT}/installer" "${ROOT}/scripts" -type f \( -name '*.sh' -o -name '*.ps1' \) | sort)

if [[ "${failures}" -gt 0 ]]; then
  printf 'check-sed-escaping: FAILED (%d substitution(s) without escaping)\n' "${failures}" >&2
  exit 1
fi

echo "check-sed-escaping: OK (placeholder substitutions escape their values)"
