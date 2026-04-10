#!/usr/bin/env bash
set -euo pipefail

DOWNLOAD_URL="https://github.com/XXNOUR/Vitruvius/releases/download/v0.1.0-alpha/Vitruvuis"
INSTALL_PATH="/usr/local/bin/vitruvius"
SERVICE_NAME="vitruvius"
HTTP_PORT=9000
WS_PORT=9001

GREEN='\033[0;32m'
BLUE='\033[0;34m'
RED='\033[0;31m'
BOLD='\033[1m'
NC='\033[0m'

info()    { echo -e "${BLUE}[Vitruvius]${NC} $1"; }
success() { echo -e "${GREEN}[Vitruvius]${NC} $1"; }
error()   { echo -e "${RED}[Vitruvius]${NC} $1"; exit 1; }

echo -e ""
echo -e "${BOLD}  Vitruvius — P2P LAN File Sync${NC}"
echo -e "  No cloud. No account. Just sync."
echo -e ""

# ── Check dependencies ────────────────────────────────────────────────────────
command -v curl >/dev/null 2>&1 || error "curl is required but not installed."
command -v systemctl >/dev/null 2>&1 || error "systemd is required but not found."

# ── Check architecture ────────────────────────────────────────────────────────
ARCH=$(uname -m)
if [ "$ARCH" != "x86_64" ]; then
  error "Only x86_64 is supported right now. Your arch: $ARCH"
fi

# ── Download binary ───────────────────────────────────────────────────────────
info "Downloading Vitruvius..."
TMP=$(mktemp)
curl -fsSL "$DOWNLOAD_URL" -o "$TMP"
chmod +x "$TMP"

# ── Install binary ────────────────────────────────────────────────────────────
info "Installing to $INSTALL_PATH..."
if [ -w "$(dirname $INSTALL_PATH)" ]; then
  mv "$TMP" "$INSTALL_PATH"
else
  sudo mv "$TMP" "$INSTALL_PATH"
fi

# ── Install systemd user service ──────────────────────────────────────────────
info "Setting up autostart service..."
SERVICE_DIR="${HOME}/.config/systemd/user"
mkdir -p "$SERVICE_DIR"

cat > "${SERVICE_DIR}/${SERVICE_NAME}.service" << EOF
[Unit]
Description=Vitruvius P2P Sync Daemon
After=network.target

[Service]
ExecStart=${INSTALL_PATH} --http-port ${HTTP_PORT} --ws-port ${WS_PORT}
Restart=on-failure
RestartSec=5
Environment=RUST_LOG=warn

[Install]
WantedBy=default.target
EOF

# ── Enable and start ──────────────────────────────────────────────────────────
systemctl --user daemon-reload
systemctl --user enable "$SERVICE_NAME" --now

# ── Open browser ──────────────────────────────────────────────────────────────
sleep 1
success "Vitruvius is installed and running!"
echo ""
echo -e "  ${BOLD}Open your browser:${NC} http://localhost:${HTTP_PORT}"
echo ""
echo -e "  Useful commands:"
echo -e "  systemctl --user status vitruvius    ${BLUE}# check if running${NC}"
echo -e "  systemctl --user stop vitruvius      ${BLUE}# stop${NC}"
echo -e "  systemctl --user restart vitruvius   ${BLUE}# restart${NC}"
echo -e "  journalctl --user -u vitruvius -f    ${BLUE}# live logs${NC}"
echo ""
xdg-open "http://localhost:${HTTP_PORT}" 2>/dev/null || true
