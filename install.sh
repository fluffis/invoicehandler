#!/bin/bash
set -e

BINARY_NAME="invoicehandler"
SERVICE_NAME="invoicehandler.service"
INSTALL_DIR="$HOME/.local/bin"
SERVICE_DIR="$HOME/.config/systemd/user"
CONFIG_FILE="$HOME/.invoicehandler"

# Build release binary
echo "Building release binary..."
cargo build --release

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
Description=Invoice Handler - File rename watcher
After=default.target

[Service]
Type=simple
ExecStart=$INSTALL_DIR/$BINARY_NAME
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
EOF

# Reload systemd
echo "Reloading systemd..."
systemctl --user daemon-reload

# Check if config exists
if [ ! -f "$CONFIG_FILE" ]; then
    echo ""
    echo "WARNING: Config file not found at $CONFIG_FILE"
    echo "Create your config file before starting the service."
    echo ""
fi

echo "Installation complete!"
echo ""
echo "Commands:"
echo "  systemctl --user enable $SERVICE_NAME  # Enable on login"
echo "  systemctl --user start $SERVICE_NAME   # Start now"
echo "  systemctl --user status $SERVICE_NAME  # Check status"
echo "  journalctl --user -u $SERVICE_NAME     # View logs"
