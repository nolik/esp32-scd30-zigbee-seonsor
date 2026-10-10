{
  description = "ESP32-C6 SCD30 Zigbee sensor - Rust firmware dev shell";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    esp-dev = {
      url = "github:mirrexagon/nixpkgs-esp-dev";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { nixpkgs, esp-dev, rust-overlay, ... }:
    let
      systems = [ "x86_64-linux" "aarch64-darwin" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f (import nixpkgs {
        inherit system;
        overlays = [ esp-dev.overlays.default (import rust-overlay) ];
        # esptool needs ecdsa (see nixpkgs-esp-dev issue #109)
        config.permittedInsecurePackages = [ "python3.13-ecdsa-0.19.1" ];
      }));
    in {
      devShells = forAllSystems (pkgs:
        let
          # Q12: ESP-IDF 5.3.3 (proven by the Rust reference projects), not the flake default.
          esp-idf = pkgs.esp-idf-riscv.override {
            rev = "v5.3.3";
            sha256 = "sha256-ESX+K0BrIKEYKzgdRlwZqAY7EvrQZ88V9xr7PtBofI4=";
          };
          rust = pkgs.rust-bin.fromRustupToolchainFile ./firmware/rust-toolchain.toml;
        in {
          default = pkgs.mkShell {
            buildInputs = [ esp-idf rust pkgs.espflash pkgs.ldproxy pkgs.mosquitto pkgs.jq ];
            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
            # Use the IDF + toolchains from this shell instead of embuild downloading its own
            # (downloaded dynamic binaries don't run on NixOS).
            ESP_IDF_TOOLS_INSTALL_DIR = "fromenv";
            MCU = "esp32c6";
          };
        });
    };
}
