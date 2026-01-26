#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_DIR="${HOME}/.local/bin"
SERVICE_DIR="${HOME}/.config/systemd/user"
BIN_PATH="${BIN_DIR}/pttkey"
SERVICE_PATH="${SERVICE_DIR}/pttkey.service"
UDEV_RULE_SRC="${ROOT_DIR}/99-ptt-input.rules"
UDEV_RULE_DEST="/etc/udev/rules.d/99-ptt-input.rules"

mkdir -p "${BIN_DIR}" "${SERVICE_DIR}"

if [[ -x "${ROOT_DIR}/pttkey" ]]; then
    install -m 755 "${ROOT_DIR}/pttkey" "${BIN_PATH}"
elif [[ -x "${ROOT_DIR}/target/release/pttkey" ]]; then
    install -m 755 "${ROOT_DIR}/target/release/pttkey" "${BIN_PATH}"
else
    cargo build --release
    install -m 755 "${ROOT_DIR}/target/release/pttkey" "${BIN_PATH}"
fi

cat > "${SERVICE_PATH}" <<EOF
[Unit]
Description=Push-to-talk mic toggle (pttkey)
After=pipewire.service pipewire-pulse.service

[Service]
Type=simple
ExecStart=${BIN_PATH}
Restart=on-failure
RestartSec=1

[Install]
WantedBy=default.target
EOF

systemctl --user daemon-reload
systemctl --user enable --now pttkey.service
echo "Installed ${BIN_PATH} and enabled user service pttkey.service"

if [[ -f "${UDEV_RULE_SRC}" ]]; then
    if [[ -w "/etc/udev/rules.d" ]]; then
        install -m 644 "${UDEV_RULE_SRC}" "${UDEV_RULE_DEST}"
        udevadm control --reload-rules
        udevadm trigger
        echo "Installed udev rule ${UDEV_RULE_DEST}"
    elif command -v sudo >/dev/null 2>&1; then
        sudo install -m 644 "${UDEV_RULE_SRC}" "${UDEV_RULE_DEST}"
        sudo udevadm control --reload-rules
        sudo udevadm trigger
        echo "Installed udev rule ${UDEV_RULE_DEST} (via sudo)"
    else
        echo "Warning: cannot install udev rule; run with sudo to install ${UDEV_RULE_DEST}"
    fi
fi

if ! id -nG "${USER}" | grep -qw "input"; then
    echo "Note: add your user to the input group and re-login:"
    echo "  sudo usermod -aG input ${USER}"
fi
