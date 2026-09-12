#!/usr/bin/env bash
set -euo pipefail

if [[ "${EUID:-$(id -u)}" -ne 0 ]]; then
  exit 0
fi

resolve_user_id() {
  for candidate in "${SUDO_UID:-}" "${PKEXEC_UID:-}"; do
    if [[ -n "${candidate}" && "${candidate}" != "0" && "${candidate}" =~ ^[0-9]+$ ]]; then
      printf '%s\n' "${candidate}"
      return 0
    fi
  done
  for candidate_name in "${SUDO_USER:-}" "${PKEXEC_USER:-}"; do
    if [[ -n "${candidate_name}" ]]; then
      candidate_id="$(getent passwd "${candidate_name}" | cut -d: -f3 || true)"
      if [[ -n "${candidate_id}" && "${candidate_id}" != "0" ]]; then
        printf '%s\n' "${candidate_id}"
        return 0
      fi
    fi
  done
  return 1
}

user_id="$(resolve_user_id || true)"
[[ -n "${user_id}" ]] || exit 0

user_record="$(getent passwd "${user_id}" || true)"
[[ -n "${user_record}" ]] || exit 0
user_name="$(printf '%s' "${user_record}" | cut -d: -f1)"
user_home="$(printf '%s' "${user_record}" | cut -d: -f6)"
user_group_id="$(printf '%s' "${user_record}" | cut -d: -f4)"
[[ -n "${user_name}" && -n "${user_group_id}" && -d "${user_home}" ]] || exit 0

autostart_dir="${user_home}/.config/autostart"
autostart_file="${autostart_dir}/codex-multiprovider-panel.desktop"
if [[ -e "${autostart_file}" ]] && ! grep -Fxq 'Exec=/usr/bin/codex-mp-panel' "${autostart_file}"; then
  exit 0
fi

install -d -m 0700 -o "${user_id}" -g "${user_group_id}" "${autostart_dir}"
temporary_file="$(mktemp "${autostart_file}.tmp.XXXXXX")"
trap 'rm -f "${temporary_file}"' EXIT
cat >"${temporary_file}" <<'EOF'
[Desktop Entry]
Type=Application
Name=Codex MultiProvider
Comment=Provider and model manager for Codex
Exec=/usr/bin/codex-mp-panel
Terminal=false
Categories=Development;
StartupNotify=false
X-GNOME-Autostart-enabled=true
EOF
install -o "${user_id}" -g "${user_group_id}" -m 0644 "${temporary_file}" "${autostart_file}"
