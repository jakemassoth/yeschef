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

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      rust-overlay,
      naersk,
      zmx-flake,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };
        # The zmx binary the backend shells out to at runtime.
        zmx = zmx-flake.packages.${system}.default;
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
          ];
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
          # writeShellApplication puts runtimeInputs on PATH for us (no manual
          # `export PATH`). All three tools come from nix so the run is
          # self-contained: the pinned toolchain provides cargo AND the `rustc`
          # it shells out to (resolving rustc from an ambient rustup shim on a
          # clean CI runner corrupts the toolchain under concurrent builds), and
          # git + zmx are guaranteed present without an existence check.
          program = "${
            pkgs.writeShellApplication {
              name = "yeschef-e2e";
              runtimeInputs = [
                rustToolchain
                zmx
                pkgs.git
              ];
              text = ''
                exec cargo test --test e2e -- --ignored --test-threads=1 "$@"
              '';
            }
          }/bin/yeschef-e2e";
        };

        # nix flake check — the CI suite, all sandboxed builds.
        #
        # Covered here: fmt (rustfmt), nixfmt, lint (clippy), test (unit). These
        # are pure and run cleanly in the Nix sandbox.
        #
        # NOT covered here: the e2e suite. It is intentionally kept out of
        # `nix flake check` and run as a separate `nix run .#e2e` CI step. Two
        # reasons:
        #   1. e2e drives a REAL zmx session and REAL git worktrees, sharing the
        #      global `yeschef` zmx session namespace and spawning detached zmx
        #      daemons. That is an impure integration test, not a hermetic build.
        #   2. naersk's test mode can't cleanly run the `#[ignore]`d e2e tests:
        #      its deps-only build phase replays the same `cargo test` options
        #      against a dummy src that has no `e2e` target and fails. Forcing it
        #      would mean a bespoke build derivation duplicating naersk's vendoring.
        # zmx itself does run in the sandbox, but the above make a separate
        # un-sandboxed `nix run .#e2e` step the right home for the suite.
        checks = {
          # rustfmt is the formatter; `cargo fmt --check` IS the formatting check.
          fmt =
            pkgs.runCommand "check-fmt"
              {
                nativeBuildInputs = [ rustToolchain ];
              }
              ''
                cd ${./.}
                export HOME="$TMPDIR"
                cargo fmt --all --check
                touch $out
              '';

          # nixfmt-rfc-style on the flake itself (cheap, keeps the .nix tidy).
          nixfmt =
            pkgs.runCommand "check-nixfmt"
              {
                nativeBuildInputs = [ pkgs.nixfmt-rfc-style ];
              }
              ''
                nixfmt --check ${./flake.nix}
                touch $out
              '';

          # clippy strict (-D warnings -D clippy::pedantic) — reuse the package.
          lint = self.packages.${system}.clippy;

          # unit tests (e2e tests are #[ignore]d, so they compile but don't run).
          test = self.packages.${system}.test;
        };

        devShells.default = pkgs.mkShell {
          buildInputs = [
            rustToolchain
            pkgs.cargo-watch
            zmx
          ];

          RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
        };
      }
    );
}
