{
  description = "yeschef";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    naersk = {
      url = "github:nix-community/naersk";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # zmx — session attach/detach for the terminal. Its build is tightly
    # coupled to zig2nix's pinned nixpkgs + Apple SDK, so we deliberately do
    # NOT make it follow our nixpkgs; we just consume its built package.
    zmx-flake.url = "github:thrawny/zmx-flake";
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay, naersk, zmx-flake }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };
        # The zmx binary the backend shells out to at runtime.
        zmx = zmx-flake.packages.${system}.default;
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" ];
        };
        naersk' = pkgs.callPackage naersk {
          cargo = rustToolchain;
          rustc = rustToolchain;
        };
      in
      {
        packages = {
          # nix build  /  nix run . -- <args>
          default = naersk'.buildPackage { src = ./.; };

          # nix build .#check  — cargo check (type check without codegen)
          check = naersk'.buildPackage {
            src = ./.;
            mode = "check";
          };

          # nix build .#clippy  — clippy with strict lints
          clippy = naersk'.buildPackage {
            src = ./.;
            mode = "clippy";
            # default already includes "-D warnings"; add pedantic on top
            cargoClippyOptions = prev: prev ++ [ "-D clippy::pedantic" ];
          };

          # nix build .#test  — unit tests (ignores #[ignore] e2e tests)
          test = naersk'.buildPackage {
            src = ./.;
            mode = "test";
          };
        };

        # nix run .#e2e [-- <test-name>]  — runs the e2e suite. It drives the
        # orchestrator against real git worktrees and a real zmx session, so it
        # needs `git` and `zmx` on PATH (no containers, no macOS requirement).
        # `zmx` is supplied by the zmx-flake package and prepended to PATH below.
        # nix run . -- <args>  — run THIS checkout's yeschef. The orchestrator
        # uses this so each branch runs its own build.
        apps.default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/yeschef";
        };

        apps.e2e = {
          type = "app";
          program = toString (pkgs.writeShellScript "yeschef-e2e" ''
            set -euo pipefail
            export PATH="${zmx}/bin:$PATH"
            for bin in git zmx; do
              if ! command -v "$bin" >/dev/null 2>&1; then
                echo "error: '$bin' not found in PATH; e2e tests require it" >&2
                exit 1
              fi
            done
            exec ${rustToolchain}/bin/cargo test --test e2e -- --ignored --test-threads=1 "$@"
          '');
        };

        devShells.default = pkgs.mkShell {
          buildInputs = [
            rustToolchain
            pkgs.cargo-watch
            zmx
          ];

          RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
        };
      });
}
