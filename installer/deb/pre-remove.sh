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
[[ -n "${user_name}" && -d "${user_home}" ]] || exit 0

panel_pids() {
  ps -eo pid=,uid=,comm= \
    | awk -v current_uid="${user_id}" '$2 == current_uid && $3 == "codex-mp-panel" {print $1}'
}

pids="$(panel_pids)"
if [[ -n "${pids}" ]]; then
  kill -TERM ${pids}
  for _ in $(seq 1 20); do
    [[ -z "$(panel_pids)" ]] && break
    sleep 0.1
  done
  pids="$(panel_pids)"
  if [[ -n "${pids}" ]]; then
    kill -KILL ${pids}
  fi
fi

cli_bin=""
for candidate in /usr/bin/codex-mp /usr/lib/codex-multiprovider/codex-mp /usr/lib/codex-multiprovider/resources/codex-mp; do
  if [[ -x "${candidate}" ]]; then
    cli_bin="${candidate}"
    break
  fi
done
[[ -n "${cli_bin}" ]] || exit 0

runuser -u "${user_name}" -- env \
  HOME="${user_home}" \
  XDG_CONFIG_HOME="${user_home}/.config" \
  "${cli_bin}" uninstall

autostart_file="${user_home}/.config/autostart/codex-multiprovider-panel.desktop"
if [[ -f "${autostart_file}" ]] && grep -Fxq 'Exec=/usr/bin/codex-mp-panel' "${autostart_file}"; then
  rm -f "${autostart_file}"
fi
