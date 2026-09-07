{
  description = "rushi-tui — the swappable TUI front-end (Tier-2, docs/tui-ext-repo-split.md)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # Bootstrap (local path-dep split, docs/tui-ext-repo-split.md section
    # 4): `rushi-common` is a *path* dep on the sibling kernel checkout
    # (../rust-unix-harness/crates/rushi), so a hermetic flake build
    # cannot see it. At hosting time, add the kernel as a flake input
    # (a pinned git source) and feed it into a `buildRustPackage`:
    #   rushi = { url = "<kernel git URL>"; };
    # The devShell below is the current dev path.
  };

  outputs = { self, nixpkgs, flake-utils, fenix, ... }:
    flake-utils.lib.eachDefaultSystem (system:
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
      in
      {
        devShells.default = pkgs.mkShell {
          buildInputs = [
            rustToolchain
            pkgs.jq
            pkgs.python3
            pkgs.file
            # Lean toolchain for the DRT gate (`lake build` +
            # `lean-verify` op=drt against bin/tui-stream-drt):
            # `lean`, `lake`, and `z3` on PATH. `leanPackages.mathlib`
            # exports LEAN_PATH with the Nix-prebuilt oleans.
            pkgs.lean4
            pkgs.z3
            pkgs.leanPackages.mathlib
          ];
          shellHook = ''
            echo "rushi-tui dev shell: rust + lean on PATH."
            echo "Build the TUI (sibling kernel path-dep):  cargo build"
            echo "DRT gate:  cd lean && lake build"
            echo "Run the PTY smoke (two-repo):"
            echo "  EXTS_ROOT=../rushi-exts python3 scripts/tui-pty-smoke.py \\"
            echo "      target/debug/tui <kernel-root>"
          '';
        };
      });
}
