{
  description = "nixsand";

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
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay, naersk }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };
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
          # nix build
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

        # nix run .#e2e [-- <test-name>]  — runs the heavy e2e suite outside
        # the nix sandbox. Requires macOS aarch64 with `container`, `tmux`, and
        # `git` available on PATH (not provided by nix — the suite shells out
        # to Apple's `container` CLI which lives outside nixpkgs).
        apps.e2e = {
          type = "app";
          program = toString (pkgs.writeShellScript "nixsand-e2e" ''
            set -euo pipefail
            for bin in container tmux git; do
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
            pkgs.tmux
          ];

          RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
        };
      });
}
