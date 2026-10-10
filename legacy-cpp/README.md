# Legacy C++ firmware (rollback only)

Kept buildable until the Rust firmware passes its 7-day soak (see `docs/rust-migration.md`,
Q5/Q21), then deleted. Build from `legacy-cpp/` on any commit of this branch, or from the repo root
at tag `cpp-final` (which predates the move).

Build/flash (on the NixOS build host):

    cd legacy-cpp
    nix --experimental-features 'nix-command flakes' develop github:mirrexagon/nixpkgs-esp-dev/5287d6e1ca9e15ebd5113c41b9590c468e1e001b#esp32c6-idf
    idf.py -p /dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_A0:F2:62:45:CE:34-if00 flash

Do **not** run `idf.py set-target`: it regenerates `sdkconfig` and drops the custom partition table
(only the committed `sdkconfig` has it), and the device then boot-loops on a missing `zb_storage`.
The nixpkgs-esp-dev revision is pinned (same as the root `flake.lock`, ESP-IDF 5.5.2).

Flashing this writes its own partition table (app at 0x10000) but never touches `nvs`,
`zb_storage` or `zb_fct`, so the Zigbee pairing and the Stored Reference survive.

C++-only build gotchas (from the original README): missing `driver` in `REQUIRES`;
`ESP_ZB_*_CONFIG()` / `DEFINE_PSTRING` macros redefined locally in `main.cpp`; narrowing
errors on cluster `measured_value` casts.
