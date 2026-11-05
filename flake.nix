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
#   * cargo-binutils — `cargo size` / `cargo objdump` over the toolchain's
#     llvm-tools
#   * probe-rs       — flashes the Pico 2 and streams defmt logs
#   * picotool       — optional, for UF2 / BOOTSEL flashing
#   * Node.js        — executes the dashboard behavior tests without npm
#   * Python 3 + schemdraw — for re-rendering design/circuit.py
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

    # Unstable dropped Intel macOS in 26.11. Keep that existing host supported
    # on its newest maintained branch while Linux/Apple Silicon track unstable.
    nixpkgs-intel-darwin.url = "github:NixOS/nixpkgs/nixpkgs-26.05-darwin";

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

  outputs = { self, nixpkgs, nixpkgs-intel-darwin, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        hostNixpkgs = if system == "x86_64-darwin" then nixpkgs-intel-darwin else nixpkgs;
        pkgs = import hostNixpkgs {
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
        pythonEnv = pkgs.python3.withPackages (ps: [ ps.schemdraw ]);
      in
      {
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            # ── Rust ────────────────────────────────────────────────
            rustToolchain
            cargo-binutils # `cargo size` / `cargo objdump` over llvm-tools
            nodejs
            jq             # read Cargo's actual ELF path for UF2 packaging

            # ── Embedded toolchain ──────────────────────────────────
            # probe-rs-tools provides `probe-rs` (the binary used by
            # the cargo runner in .cargo/config.toml), plus `cargo-flash`
            # and `cargo-embed` if you want them.
            probe-rs-tools

            # picotool: drag-and-drop UF2 flashing path; only used
            # when you don't have a debug probe handy.
            picotool

            # ── Hardware-design tooling ─────────────────────────────
            pythonEnv     # python3 + schemdraw
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
            echo "  Build UF2     : ./scripts/build-uf2.sh   (BOOTSEL flashing)"
            echo "  Host tests    : ./scripts/test-host.sh"
            echo "  Dashboard     : ./scripts/test-dashboard.sh"
            echo "  Re-render SVG : python3 design/circuit.py"
            echo "─────────────────────────────────────────────────────"
            echo
          '';
        };

        # ── Lean shell for CI's Rust jobs ───────────────────────────
        # Everything those jobs need and nothing they don't: the pinned
        # Rust toolchain (cargo/clippy/rustfmt plus the cross-compile
        # target, all read from rust-toolchain.toml) and picotool for UF2
        # packaging. mkShell pulls in a host C toolchain automatically,
        # which is all the host-side ezal-core tests need to link.
        #
        # Deliberately omits probe-rs, Node.js, and the
        # python/schemdraw stack from the default shell: CI's Rust jobs
        # never flash hardware, run the dashboard tests, or re-render the
        # schematic, and those closures would slow every run. `ci.yml`
        # loads this with `nix develop .#ci`, so flake.lock is the single
        # source of truth for CI's toolchain too.
        devShells.ci = pkgs.mkShell {
          packages = [
            rustToolchain
            pkgs.picotool
            pkgs.jq
          ];
        };

        # ── Dashboard tests ─────────────────────────────────────────
        # `scripts/test-dashboard.sh` runs the served JavaScript under
        # Node's built-in test runner. Rust exports the exact browser
        # configuration from the firmware constants. Node stays out of
        # the other Rust jobs; flake.lock pins both toolchains.
        devShells.dashboard = pkgs.mkShell {
          packages = [ rustToolchain pkgs.nodejs ];
        };
      });
}
