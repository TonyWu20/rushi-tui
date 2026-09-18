{
  description = "rushi-tui — the terminal UI front-end for the rushi agent harness";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # The Nix-built `rushi` launcher (kernel flake packages.default),
    # fetched from GitHub so the flake is hostable (no local sibling
    # checkout required). The `rushi-common` crate is a git dep in
    # bin/tui/Cargo.toml (rev pinned in Cargo.lock), resolved by cargo
    # at build time, so no raw source-tree input is needed here.
    rushi-kernel = { url = "github:TonyWu20/rushi"; };
  };

  outputs = { self, nixpkgs, fenix, rushi-kernel, ... }:
    let
      # Explicit system list instead of eachDefaultSystem (same reason
      # as the kernel flake: eachDefaultSystem transposes the result and
      # breaks the standard packages.<system>.<name> shape).
      supportedSystems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ];
      pkgLib = nixpkgs.lib;

      forSystem = system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ fenix.overlays.default ];
          };
          rustToolchain = (fenix.packages.${system}.stable.withComponents [
            "cargo"
            "clippy"
            "rust-src"
            "rustc"
            "rustfmt"
            "rust-analyzer"
          ]);
          rushiPkg = rushi-kernel.packages.${system}.default;
          moldStdenv = pkgs.stdenvAdapters.useMoldLinker pkgs.clangStdenv;

          # TUI package ($out/bin/tui contract, ext-flake-authoring.md
          # section 5.1). `src` stays the flake source so buildRustPackage
          # can read the workspace Cargo.toml/Cargo.lock at eval time.
          # The `rushi-common` kernel git dep (rev pinned in Cargo.lock)
          # and the `ratatui-markdown` git dep resolve via cargo at build
          # time; the `tui-highlight` intra-repo path dep resolves within
          # the source tree.
          tuiPkg = pkgs.rustPlatform.buildRustPackage {
            pname = "rushi-tui";
            version = "0.1.0";
            src = self;
            nativeBuildInputs = [ rustToolchain ];
            cargoLock = {
              lockFile = ./Cargo.lock;
              outputHashes = {
                "ratatui-markdown-0.3.6" = "sha256-++qk2uLCBvak22vQf2OmGta5YaLtHCKiKAx4wLVg7yk=";
              };
            };
            # Build only the `tui` binary (and its path-dep
            # `tui-highlight`); skip the standalone `tui-stream-drt`
            # DRT mirror.
            cargoBuildFlags = [ "-p" "tui" ];
            doCheck = false;
            # Without this, `nix search` / `nix path-info` show the
            # package as "Undocumented". One attribute covers both
            # packages.<system>.default and .tui (same derivation).
            # MIT: repo-root LICENSE, mirroring the kernel repo
            # (2026-09-18 decision).
            meta = {
              description = "Interactive terminal front-end for the rushi agent harness (the Ratatui TUI binary)";
              license = pkgLib.licenses.mit;
            };
            # No installPhase override: the default cargoInstallHook copies
            # the target-<triple>/release binaries into $out/bin (it knows
            # the target subdir). The kernel's side-by-side resolver
            # (<exe_dir>/tui) then finds $out/bin/tui.
          };
        in
        {
          # The TUI binary as a Nix package. A consumer (rushi-config,
          # analogous to pi-config) sets:
          #   rushi.tui = rushiTuiFlake.packages.<system>.default;
          # via lib.mkRushi's `rushi.tui` option.
          packages = {
            default = tuiPkg;
            tui = tuiPkg;
          };

          devShells = {
            default = pkgs.mkShell.override { stdenv = moldStdenv; } {
              buildInputs = with pkgs;[
                rustToolchain
                # The tree-sitter grammar crates (tui-highlight) compile
                # C parser sources via the `cc` build-dep; the fenix
                # Rust toolchain does not ship a C compiler.
                stdenv.cc
                jq
                python3
                file
                # Lean toolchain for the DRT gate (`lake build` +
                # `lean-verify` op=drt against bin/tui-stream-drt):
                # `lean`, `lake`, and `z3` on PATH. `leanPackages.mathlib`
                # exports LEAN_PATH with the Nix-prebuilt oleans.
                lean4
                z3
                leanPackages.mathlib
                # The Nix-built `rushi` launcher (kernel flake packages.default).
                # The launcher finds `tui` on PATH after its side-by-side
                # check (resolve_tui_binary); the .envrc export of
                # target/release completes that contract.
                rushiPkg
              ];
              shellHook = ''
                echo "rushi-tui dev shell: rust + lean + Nix-built rushi on PATH."
                echo "Build the TUI (rushi-common git dep):      cargo build --release"
                echo "Ext-PTY tests: KERNEL_ROOT=<kernel abs path> EXTS_ROOT=<exts abs path> cargo test -p tui"
                echo "DRT gate:  cd lean && lake build"
                echo "Run the PTY smoke (two-repo):"
                echo "  EXTS_ROOT=../rushi-exts python3 scripts/tui-pty-smoke.py \\"
                echo "      target/debug/tui <kernel-root>"
              '';
            };
          };
        };
    in
    {
      packages = pkgLib.genAttrs supportedSystems (system: (forSystem system).packages);
      devShells = pkgLib.genAttrs supportedSystems (system: (forSystem system).devShells);
    };
}
