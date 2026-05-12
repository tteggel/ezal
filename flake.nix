# ────────────────────────────────────────────────────────────────────────────
# ezal — development-environment flake
# ────────────────────────────────────────────────────────────────────────────
#
# This flake pins every host-side tool the project needs into a single,
# reproducible development shell. With Nix + direnv installed, simply
# `cd`ing into the workspace activates everything below; no manual
# `cargo install`s, no version drift.
#
# What's inside the shell:
#   * Rust toolchain (channel, components, targets all read straight from
#     `rust-toolchain.toml` so there is one source of truth)
#   * probe-rs       — flashes the Pico 2 and streams defmt logs
#   * flip-link      — optional linker wrapper for stack-overflow trapping
#   * picotool       — optional, for UF2 / BOOTSEL flashing
#   * Python 3 + schemdraw + matplotlib — for re-rendering design/circuit.py
#   * libusb / udev (Linux) / IOKit (macOS) — backend libs probe-rs uses
#
# Updating any of the above: change the pin here, run `nix flake update`,
# review the diff to flake.lock, commit.

{
  description = "ezal — satellite antenna tracker firmware for the Raspberry Pi Pico 2";

  inputs = {
    # `nixos-unstable` rather than a release branch because we want
    # current versions of probe-rs / embassy-rs tooling, which lag in
    # the stable channels. Pinned exactly by flake.lock once generated.
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    # Cross-platform helper: builds the same outputs for each system
    # we care about (x86_64-linux, aarch64-linux, *-darwin) without
    # us writing the system-dispatch boilerplate.
    flake-utils.url = "github:numtide/flake-utils";

    # rust-overlay gives us `rust-bin.fromRustupToolchainFile`, which
    # reads `rust-toolchain.toml` directly. That keeps a single source
    # of truth for the channel, components, and targets — change the
    # toolchain file, both rustup-only and Nix-using contributors stay
    # in sync.
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };

        # Read the toolchain pin from rust-toolchain.toml. The overlay
        # honours `channel`, `components`, `targets`, and `profile`.
        rustToolchain =
          pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

        # Python environment for re-rendering design/circuit.py. We
        # bake the deps into a `python3` derivation so `python3` Just
        # Works inside the shell — no `pip install` needed.
        pythonEnv = pkgs.python3.withPackages (ps: with ps; [
          schemdraw
          matplotlib
        ]);
      in
      {
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            # ── Rust ────────────────────────────────────────────────
            rustToolchain

            # ── Embedded toolchain ──────────────────────────────────
            # probe-rs-tools provides `probe-rs` (the binary used by
            # the cargo runner in .cargo/config.toml), plus `cargo-flash`
            # and `cargo-embed` if you want them.
            probe-rs-tools

            # flip-link: inverts the stack-pointer to trap overflows
            # with an MPU fault instead of corrupting BSS. Off by
            # default in .cargo/config.toml; uncomment the `linker =`
            # line there to enable it when you need it.
            flip-link

            # picotool: drag-and-drop UF2 flashing path; only used
            # when you don't have a debug probe handy.
            picotool

            # ── Hardware-design tooling ─────────────────────────────
            pythonEnv     # python3 + schemdraw + matplotlib

            # ── General build helpers ───────────────────────────────
            pkg-config
          ]
          ++ lib.optionals stdenv.isLinux [
            # probe-rs talks to debug probes via libusb, and reads
            # device permissions via libudev (Linux only).
            udev
            libusb1
          ]
          ++ lib.optionals stdenv.isDarwin [
            # macOS USB backend.
            libusb1
            darwin.apple_sdk.frameworks.IOKit
            darwin.apple_sdk.frameworks.CoreFoundation
            darwin.apple_sdk.frameworks.AppKit
          ];

          # A small banner so it's obvious the shell is active and
          # what's available. Output is cached by direnv so it only
          # appears on the first cd into the workspace.
          shellHook = ''
            echo
            echo "── ezal dev shell ───────────────────────────────────"
            echo "  rustc:    $(rustc --version 2>/dev/null || echo 'missing')"
            echo "  cargo:    $(cargo --version 2>/dev/null || echo 'missing')"
            echo "  probe-rs: $(probe-rs --version 2>/dev/null | head -1 || echo 'missing')"
            echo "  python:   $(python3 --version 2>/dev/null || echo 'missing') + schemdraw"
            echo
            echo "  Build & flash : cargo run   -p ezal-firmware --release"
            echo "  Host tests    : ./scripts/test-host.sh"
            echo "  Re-render SVG : python3 design/circuit.py"
            echo "─────────────────────────────────────────────────────"
            echo
          '';
        };
      });
}
