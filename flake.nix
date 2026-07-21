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
    # herdr — an "agent multiplexer" (github.com/ogulcancelik/herdr) we are
    # evaluating as a possible replacement for yeschef's tmux TUI/session layer
    # (see docs/herdr-investigation.md). Pulled in only so it is a runnable via
    # this flake (`nix run .#herdr`); nothing in yeschef links or depends on it
    # yet. Deliberately NOT `follows`-ing our nixpkgs/rust-overlay: herdr's flake
    # pins its own toolchain (rust-toolchain.toml + zig/cmake native deps), so we
    # let it build exactly as upstream locks it. Deduping via `follows` is a
    # future optimization to validate, not a first-step requirement.
    herdr.url = "github:ogulcancelik/herdr";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      rust-overlay,
      naersk,
      herdr,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };
        # herdr — the agent multiplexer the backend shells out to at runtime
        # (`Command::new("herdr")`), built straight from herdr's own pinned flake
        # (its rust-toolchain.toml + zig/cmake native deps). yeschef drives it on
        # a named `--session yeschef`, which isolates the brigade from a human's
        # own default herdr session (the analog of the old tmux `-L` socket).
        herdrPkg = herdr.packages.${system}.default;
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
        # bake herdr onto PATH (see below); everything else builds from source.
        yeschef-unwrapped = naersk'.buildPackage { src = ./.; };
      in
      {
        packages = {
          # nix build  /  nix run . -- <args>
          #
          # The backend shells out to bare `herdr` (Command::new("herdr")), so the
          # shipped binary must find herdr at runtime WITHOUT the user putting it
          # on their PATH. We build the plain binary with naersk, then wrap it so
          # herdr's bin dir is baked onto PATH. `--suffix` (not `--prefix`): if a
          # user has explicitly installed their own herdr, that copy on the
          # ambient PATH wins; ours is only the fallback for when herdr is absent.
          # We wrap in a separate symlinkJoin derivation rather than via naersk's
          # postInstall because naersk replays the install phase during its
          # deps-only build too, so a postInstall wrapProgram would run twice and
          # against a dummy src — wrapping outside naersk keeps it to the one real
          # binary. (Building this package therefore builds herdr; the plain
          # `.#check`/`.#clippy`/`.#test` derivations and `nix flake check` build
          # from source only and never pull herdr.)
          default = pkgs.symlinkJoin {
            name = "yeschef";
            paths = [ yeschef-unwrapped ];
            nativeBuildInputs = [ pkgs.makeWrapper ];
            postBuild = ''
              wrapProgram $out/bin/yeschef --suffix PATH : ${herdrPkg}/bin
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

          # nix build .#herdr  /  nix run .#herdr
          #
          # Re-expose upstream herdr's default package so it is buildable and
          # runnable through THIS flake. It is also yeschef's runtime backend:
          # `packages.default` bakes this onto PATH, and the devShell + e2e app
          # pull it in. It is deliberately kept out of `checks` so `nix flake
          # check` (fmt/nixfmt/lint/test) stays a from-source build and never has
          # to compile herdr (a heavy zig/cmake Rust build).
          herdr = herdrPkg;
        };

        # nix run .#e2e [-- <test-name>]  — runs the e2e suite. It drives the
        # orchestrator against real git worktrees and a real herdr server, so it
        # needs `git` and `herdr` on PATH. `herdr` comes from its pinned flake and
        # is prepended to PATH below.
        # nix run . -- <args>  — run THIS checkout's yeschef. The orchestrator
        # uses this so each branch runs its own build.
        apps.default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/yeschef";
        };

        # nix run .#herdr [-- <args>]  — launch herdr through this flake. Bare
        # `herdr` attaches/launches its TUI; pass CLI args for its command groups
        # (`nix run .#herdr -- pane list ...`). This is what makes herdr
        # available as a runnable via the flake, per the investigation ticket.
        apps.herdr = herdr.apps.${system}.default;

        apps.e2e = {
          type = "app";
          # writeShellApplication puts runtimeInputs on PATH for us (no manual
          # `export PATH`). All three tools come from nix so the run is
          # self-contained: the pinned toolchain provides cargo AND the `rustc`
          # it shells out to (resolving rustc from an ambient rustup shim on a
          # clean CI runner corrupts the toolchain under concurrent builds), and
          # git + herdr are guaranteed present without an existence check.
          program = "${
            pkgs.writeShellApplication {
              name = "yeschef-e2e";
              runtimeInputs = [
                rustToolchain
                herdrPkg
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
        #   1. e2e drives a REAL herdr server and REAL git worktrees on a
        #      throwaway per-test session (via `YESCHEF_HERDR_SESSION` + a short
        #      isolated `XDG_CONFIG_HOME`, never the live `yeschef` brigade) and
        #      spawns a detached server. That is an impure integration test, not a
        #      hermetic build.
        #   2. naersk's test mode can't cleanly run the `#[ignore]`d e2e tests:
        #      its deps-only build phase replays the same `cargo test` options
        #      against a dummy src that has no `e2e` target and fails. Forcing it
        #      would mean a bespoke build derivation duplicating naersk's vendoring.
        # The above make a separate un-sandboxed `nix run .#e2e` step the right
        # home for the suite.
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
            # herdr is yeschef's runtime backend, so the dev shell ships it too
            # (so `cargo run` / the e2e suite find it on PATH). This means
            # entering the shell builds herdr the first time.
            herdrPkg
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
