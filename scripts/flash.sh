#!/usr/bin/env bash
# Flash the Rust firmware: bootloader + partition table + app. Never erases flash -
# nvs / zb_storage / zb_fct (pairing + Stored Reference) must survive (ADR 0002).
set -euo pipefail
cd "$(dirname "$0")/../firmware"
PORT=/dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_A0:F2:62:45:CE:34-if00
PROFILE=${PROFILE:-release}
ELF=target/riscv32imac-esp-espidf/$PROFILE/firmware
BOOTLOADER=$(find target/riscv32imac-esp-espidf/$PROFILE/build -path '*esp-idf-sys*' -name bootloader.bin | head -1)
[ -n "$BOOTLOADER" ] || { echo "bootloader.bin not found - build first" >&2; exit 1; }
exec espflash flash --port "$PORT" --chip esp32c6 --flash-size 4mb \
  --bootloader "$BOOTLOADER" --partition-table partitions.csv "$ELF" "$@"
