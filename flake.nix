{
  description = "rushi-tui — the swappable TUI front-end (Tier-2, docs/tui-ext-repo-split.md)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # The Nix-built `rushi` launcher (kernel flake packages.default)
    # and the raw kernel source tree, both fetched from GitHub so the
    # flake is hostable (no local sibling checkout required).
    rushi-kernel = { url = "github:TonyWu20/rushi"; };
    # Raw source tree (not a flake) so `builtins.toPath` can hand the
    # crates/ subtree to the sed rewrite in patchPhase.
    rushi-kernel-src = {
      url = "github:TonyWu20/rushi";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, flake-utils, fenix, rushi-kernel, rushi-kernel-src, ... }:
    let
      # Explicit system list instead of eachDefaultSystem (same reason
      # as the kernel flake: eachDefaultSystem transposes the result and
      # breaks the standard packages.<system>.<name> shape).
      supportedSystems = [ "x86_64-linux" "aarch64-linux" ];
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
          # The kernel source tree, used to satisfy the `rushi-common`
          # path dep. Referenced in the patchPhase string interpolation,
          # which tracks it as a build dependency for the sed rewrite.
          kernelSrc = builtins.toPath rushi-kernel-src;
          moldStdenv = pkgs.stdenvAdapters.useMoldLinker pkgs.clangStdenv;

          # TUI package ($out/bin/tui contract, ext-flake-authoring.md
          # section 5.1). `src` stays the flake source so buildRustPackage
          # can read the workspace Cargo.toml/Cargo.lock at eval time.
          # The bootstrap sibling-kernel path dep in bin/tui/Cargo.toml is
          # rewritten to the kernel source in patchPhase. The `tui-highlight`
          # intra-repo path dep and the `ratatui-markdown` git dep resolve
          # within the source tree and the cargo lock respectively.
          tuiPkg = pkgs.rustPlatform.buildRustPackage {
            pname = "rushi-tui";
            version = "0.1.0";
            src = self;
            nativeBuildInputs = [ rustToolchain ];
            cargoLock = {
              lockFile = ./Cargo.lock;
              outputHashes = { "ratatui-markdown-0.3.6" = "sha256-++qk2uLCBvak22vQf2OmGta5YaLtHCKiKAx4wLVg7yk="; };
            };
            # Build only the `tui` binary (and its path-dep
            # `tui-highlight`); skip the standalone `tui-stream-drt`
            # DRT mirror.
            cargoBuildFlags = [ "-p" "tui" ];
            doCheck = false;
            patchPhase = ''
              sed -i \
                "s|\.\./\.\./\.\./rust-unix-harness/crates/rushi|${kernelSrc}/crates/rushi|" \
                bin/tui/Cargo.toml
            '';
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
                echo "Build the TUI (sibling kernel path-dep):  cargo build --release"
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
