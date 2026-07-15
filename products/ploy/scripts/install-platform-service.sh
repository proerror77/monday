#!/bin/bash
set -euo pipefail

ROOT_DIR="/opt/ploy"
SYSTEMD_DIR="/etc/systemd/system"
SERVICE_SRC="${ROOT_DIR}/deployment/ployd.service"
SERVICE_DST="${SYSTEMD_DIR}/ployd.service"
MAINTENANCE_SERVICE_SRC="${ROOT_DIR}/deployment/ploy-maintenance.service"
MAINTENANCE_TIMER_SRC="${ROOT_DIR}/deployment/ploy-maintenance.timer"
WATCHDOG_SERVICE_SRC="${ROOT_DIR}/deployment/ploy-platform-watchdog.service"
WATCHDOG_TIMER_SRC="${ROOT_DIR}/deployment/ploy-platform-watchdog.timer"

echo "==> Installing ployd platform service..."

if ! id -u ploy >/dev/null 2>&1; then
  sudo useradd --system --home "${ROOT_DIR}" --shell /usr/sbin/nologin --no-create-home ploy
fi

sudo mkdir -p \
  "${ROOT_DIR}/bin" \
  "${ROOT_DIR}/config" \
  "${ROOT_DIR}/config/deployments" \
  "${ROOT_DIR}/data/state" \
  "${ROOT_DIR}/deployment" \
  "${ROOT_DIR}/logs" \
  "${ROOT_DIR}/run/platform" \
  "${ROOT_DIR}/scripts" \
  "${ROOT_DIR}/scripts/drills"
sudo chown -R ploy:ploy "${ROOT_DIR}"

sudo install -m 0644 "${SERVICE_SRC}" "${SERVICE_DST}"
if [[ -f "${MAINTENANCE_SERVICE_SRC}" ]]; then
  sudo install -m 0644 "${MAINTENANCE_SERVICE_SRC}" "${SYSTEMD_DIR}/ploy-maintenance.service"
fi
if [[ -f "${MAINTENANCE_TIMER_SRC}" ]]; then
  sudo install -m 0644 "${MAINTENANCE_TIMER_SRC}" "${SYSTEMD_DIR}/ploy-maintenance.timer"
fi
if [[ -f "${WATCHDOG_SERVICE_SRC}" ]]; then
  sudo install -m 0644 "${WATCHDOG_SERVICE_SRC}" "${SYSTEMD_DIR}/ploy-platform-watchdog.service"
fi
if [[ -f "${WATCHDOG_TIMER_SRC}" ]]; then
  sudo install -m 0644 "${WATCHDOG_TIMER_SRC}" "${SYSTEMD_DIR}/ploy-platform-watchdog.timer"
fi

if [[ -f "${ROOT_DIR}/data/state/deployments.json.sample" && ! -f "${ROOT_DIR}/data/state/deployments.json" ]]; then
  sudo cp "${ROOT_DIR}/data/state/deployments.json.sample" "${ROOT_DIR}/data/state/deployments.json"
fi

if [[ -f "${ROOT_DIR}/config/deployments/example.live.dry-run.json.sample" && ! -f "${ROOT_DIR}/config/deployments/example.live.dry-run.json" ]]; then
  sudo cp "${ROOT_DIR}/config/deployments/example.live.dry-run.json.sample" "${ROOT_DIR}/config/deployments/example.live.dry-run.json"
fi

sudo touch "${ROOT_DIR}/.env"

ensure_env_default() {
  local env_file="$1"
  local key="$2"
  local value="$3"

  if ! sudo grep -qE "^${key}=" "${env_file}"; then
    echo "${key}=${value}" | sudo tee -a "${env_file}" >/dev/null
  fi
}

ensure_env_default "${ROOT_DIR}/.env" "PLOY_DEPLOYMENTS_FILE" "${ROOT_DIR}/data/state/deployments.json"
ensure_env_default "${ROOT_DIR}/.env" "PLOY_RUNTIME_ROOT" "${ROOT_DIR}/run/platform"
ensure_env_default "${ROOT_DIR}/.env" "PLOY_SYSTEM_STATUS_FILE" "${ROOT_DIR}/run/platform/system-status.json"
ensure_env_default "${ROOT_DIR}/.env" "PLOY_DEPLOYMENT_STATUS_FILE" "${ROOT_DIR}/run/platform/deployments.json"
ensure_env_default "${ROOT_DIR}/.env" "PLOY_TRADING_STATE_FILE" "${ROOT_DIR}/run/platform/trading-state.json"
ensure_env_default "${ROOT_DIR}/.env" "PLOY_LISTEN_ADDR" "127.0.0.1:8081"
ensure_env_default "${ROOT_DIR}/.env" "PLOY_TICK_INTERVAL_MS" "1000"

sudo chmod 600 "${ROOT_DIR}/.env"
sudo chown ploy:ploy "${ROOT_DIR}/.env" "${ROOT_DIR}/data/state/deployments.json" "${ROOT_DIR}/config/deployments/example.live.dry-run.json" 2>/dev/null || true

sudo systemctl daemon-reload
sudo systemctl enable ployd
if [[ -f "${SYSTEMD_DIR}/ploy-platform-watchdog.timer" ]]; then
  sudo systemctl enable --now ploy-platform-watchdog.timer
fi
if [[ -f "${SYSTEMD_DIR}/ploy-maintenance.timer" ]]; then
  sudo systemctl enable --now ploy-maintenance.timer
fi

echo "==> ployd service installed"
echo ""
echo "Commands:"
echo "  sudo systemctl start ployd          # Start the platform daemon"
echo "  sudo systemctl status ployd         # Check daemon status"
echo "  sudo systemctl status ploy-platform-watchdog.timer"
echo "  sudo systemctl start ploy-maintenance.service"
echo "  sudo systemctl status ploy-maintenance.timer"
echo "  sudo journalctl -u ployd -f         # Tail daemon logs"
echo "  ${ROOT_DIR}/bin/ployctl system status"
echo "  ${ROOT_DIR}/bin/ployctl system metrics"
echo "  ${ROOT_DIR}/bin/ployctl system alerts"
echo "  ${ROOT_DIR}/bin/ployctl trading status"
echo "  ${ROOT_DIR}/bin/ploytui"
