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
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      rust-overlay,
      naersk,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };
        # The tmux binary the backend shells out to at runtime. Plain nixpkgs
        # tmux — yeschef drives it on a private `-L` socket with its own `-f`
        # config, so no packaging quirks (unlike the old zmx dependency).
        tmux = pkgs.tmux;
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
        # The plain, unwrapped yeschef binary. `packages.default` wraps this to
        # bake tmux onto PATH (see below); everything else builds from source.
        yeschef-unwrapped = naersk'.buildPackage { src = ./.; };
      in
      {
        packages = {
          # nix build  /  nix run . -- <args>
          #
          # The backend shells out to bare `tmux` (Command::new("tmux")), so the
          # shipped binary must find tmux at runtime WITHOUT the user putting it
          # on their PATH. We build the plain binary with naersk, then wrap it so
          # the tmux package's bin dir is baked onto PATH. `--suffix` (not
          # `--prefix`): if a user has explicitly installed their own tmux, that
          # copy on the ambient PATH wins; ours is only the fallback for when
          # tmux is absent. We wrap in a separate symlinkJoin derivation rather
          # than via naersk's postInstall because naersk replays the install
          # phase during its deps-only build too, so a postInstall wrapProgram
          # would run twice and against a dummy src — wrapping outside naersk
          # keeps it to the one real binary.
          default = pkgs.symlinkJoin {
            name = "yeschef";
            paths = [ yeschef-unwrapped ];
            nativeBuildInputs = [ pkgs.makeWrapper ];
            postBuild = ''
              wrapProgram $out/bin/yeschef --suffix PATH : ${tmux}/bin
            '';
          };

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
        # orchestrator against real git worktrees and real tmux sessions, so it
        # needs `git` and `tmux` on PATH (no containers, no macOS requirement).
        # `tmux` comes from nixpkgs and is prepended to PATH below.
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
          # git + tmux are guaranteed present without an existence check.
          program = "${
            pkgs.writeShellApplication {
              name = "yeschef-e2e";
              runtimeInputs = [
                rustToolchain
                tmux
                pkgs.git
              ];
              text = ''
                exec cargo test --test e2e -- --ignored "$@"
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
        #   1. e2e drives REAL tmux sessions and REAL git worktrees on a
        #      throwaway per-test `-L` socket (via `YESCHEF_TMUX_SOCKET`, never the
        #      live `yeschef` server) and spawns detached sessions. That is an
        #      impure integration test, not a hermetic build.
        #   2. naersk's test mode can't cleanly run the `#[ignore]`d e2e tests:
        #      its deps-only build phase replays the same `cargo test` options
        #      against a dummy src that has no `e2e` target and fails. Forcing it
        #      would mean a bespoke build derivation duplicating naersk's vendoring.
        # tmux would run fine in the sandbox, but the above make a separate
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
            tmux
            # vhs records the terminal headlessly so line cooks can attach
            # demo recordings to PRs. The nixpkgs derivation wraps its
            # `ttyd` + `ffmpeg` runtime deps, so the binary is self-contained.
            pkgs.vhs
          ];

          RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
        };
      }
    );
}
