# Legacy C++ firmware (rollback only)

Frozen at tag `cpp-final`. Kept buildable until the Rust firmware passes its 7-day soak
(see `docs/rust-migration.md`, Q5/Q21), then deleted.

Build/flash (on the NixOS build host):

    cd legacy-cpp
    nix --experimental-features 'nix-command flakes' develop github:mirrexagon/nixpkgs-esp-dev#esp32c6-idf
    idf.py set-target esp32c6   # first time only
    idf.py -p /dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_A0:F2:62:45:CE:34-if00 flash

Flashing this writes its own partition table (app at 0x10000) but never touches `nvs`,
`zb_storage` or `zb_fct`, so the Zigbee pairing and the Stored Reference survive.

C++-only build gotchas (from the original README): missing `driver` in `REQUIRES`;
`ESP_ZB_*_CONFIG()` / `DEFINE_PSTRING` macros redefined locally in `main.cpp`; narrowing
errors on cluster `measured_value` casts.
