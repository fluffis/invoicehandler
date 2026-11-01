#!/bin/bash
set -e

BINARY_NAME="invoicefetcher"
SERVICE_NAME="invoicefetcher.service"
INSTALL_DIR="$HOME/.local/bin"
SERVICE_DIR="$HOME/.config/systemd/user"
CONFIG_FILE="$HOME/.invoicefetcher"

# Check if service is currently running
WAS_RUNNING=false
if systemctl --user is-active --quiet "$SERVICE_NAME" 2>/dev/null; then
    WAS_RUNNING=true
    echo "Stopping running service..."
    systemctl --user stop "$SERVICE_NAME"
fi

# Build release binary
echo "Building release binary..."
cargo build --release -p invoicefetcher

# Create install directory
mkdir -p "$INSTALL_DIR"

# Install binary
echo "Installing binary to $INSTALL_DIR..."
cp "target/release/$BINARY_NAME" "$INSTALL_DIR/"
chmod +x "$INSTALL_DIR/$BINARY_NAME"

# Create systemd user directory
mkdir -p "$SERVICE_DIR"

# Create systemd service file
echo "Creating systemd service..."
cat > "$SERVICE_DIR/$SERVICE_NAME" << EOF
[Unit]
Description=Invoice Fetcher - Email to file download daemon
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=$INSTALL_DIR/$BINARY_NAME
Restart=on-failure
RestartSec=10
TimeoutStartSec=30

[Install]
WantedBy=default.target
EOF

# Reload systemd
echo "Reloading systemd..."
systemctl --user daemon-reload

# Restart service if it was running before
if [ "$WAS_RUNNING" = true ]; then
    echo "Restarting service..."
    systemctl --user start "$SERVICE_NAME"
fi

# Check if config exists (only relevant for first install)
if [ ! -f "$CONFIG_FILE" ]; then
    echo ""
    echo "WARNING: Config file not found at $CONFIG_FILE"
    echo "Create your config file before starting the service."
    echo "See config.ini.fetcher.example for the required format."
    echo ""
fi

echo "Installation complete!"
echo ""
if [ "$WAS_RUNNING" = true ]; then
    echo "Service was restarted with the new version."
else
    echo "Commands:"
    echo "  systemctl --user enable $SERVICE_NAME  # Enable on login"
    echo "  systemctl --user start $SERVICE_NAME   # Start now"
fi
echo "  systemctl --user status $SERVICE_NAME  # Check status"
echo "  journalctl --user -u $SERVICE_NAME     # View logs"
