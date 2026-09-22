{
  inputs = {
    systems.url = "github:nix-systems/default";
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    devshell.url = "github:numtide/devshell";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      self,
      systems,
      nixpkgs,
      devshell,
      rust-overlay,
      crane,
    }:
    {
      lib = {
        eachSystem = nixpkgs.lib.genAttrs (import systems);
        makePkgs =
          system: nixpkgs':
          import nixpkgs' {
            inherit system;
            config.allowUnsupportedSystem = true;
            overlays = [
              devshell.overlays.default
              (import rust-overlay)
            ];
          };
        rpkgs =
          pkgs: with pkgs.rPackages; [
            renv
            devtools
          ];
        rustToolchain =
          pkgs:
          pkgs.rust-bin.selectLatestNightlyWith (
            toolchain:
            toolchain.default.override {
              targets = [
                "x86_64-unknown-linux-gnu"
                "x86_64-unknown-linux-musl"
                "x86_64-pc-windows-gnu"
                "aarch64-apple-darwin"
              ];
              extensions = [
                "rust-src"
                "rust-analyzer"
              ];
            }
          );
      };

      packages = self.lib.eachSystem (
        system:
        let
          pkgs = self.lib.makePkgs system nixpkgs;
          rustToolchain = self.lib.rustToolchain pkgs;

          craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;
          unfilteredRoot = ./.;

          src = pkgs.lib.fileset.toSource {
            root = unfilteredRoot;
            fileset = pkgs.lib.fileset.unions [
              (craneLib.fileset.commonCargoSources unfilteredRoot)
              # Non-Rust files pulled in by `include_str!` / tests that
              # `commonCargoSources` filters out and must be added explicitly.
              ./types
              ./crates/format/tests/format
              ./crates/ry/ry.exe.manifest
              # The release-metadata test reads the VS Code manifest to check it
              # carries the workspace version.
              ./editors/code/package.json
            ];
          };

          commonArgs = {
            inherit src;
            pname = "ry";
            strictDeps = true;
          };

          # The dep-only builds compile dependencies against a dummified copy
          # of the workspace: every local .rs file is replaced by an empty
          # stub so the dependency cache survives source edits. The
          # [patch.crates-io] crate in patches/ must keep its REAL sources in
          # that dummy copy — chrono compiles against it — so it is restored
          # verbatim. Interpolating only ./patches keeps the dummy source
          # independent of the rest of the tree, preserving the cache.
          keepPatchesInDummySrc = ''
            rm -rf "$out"/patches
            cp -r --no-preserve=mode ${./patches} "$out"/patches
          '';

          # Every release binary is linked by zig, which bundles each target's
          # C runtime and link stubs, so no target SDK or C cross toolchain is
          # needed.
          #
          # - Linux targets musl, which links statically: the binary has no
          #   ELF interpreter and no RUNPATH, so it runs on any x86_64 Linux
          #   distribution. It is the one binary the build machine can run,
          #   so it is the one that runs the test suite.
          # - macOS: zig bundles link stubs for libSystem only — no Apple
          #   frameworks. The dependency graph is kept framework-free on
          #   purpose (see patches/iana-time-zone and the preflight in the
          #   justfile's release recipe), so no macOS SDK is needed here.
          makeReleaseArgs =
            target:
            commonArgs
            // {
              CARGO_BUILD_TARGET = target;

              nativeBuildInputs = [
                pkgs.cargo-zigbuild
                pkgs.zig
              ];

              # zig keeps its compilation cache in the user cache directory,
              # and so does the stub materialization the LSP tests exercise.
              # The sandbox's HOME is not writable, so give it a real one.
              preBuild =
                ''
                  export XDG_CACHE_HOME="$TMPDIR/.cache"
                ''
                # The winapi crate (pulled in through reedline → crossterm)
                # links the Windows "synchronization" API-set import library
                # (the WaitOnAddress family). cargo-zigbuild disables winapi's
                # bundled import libraries (they are incompatible with zig's
                # lld), and zig's bundled mingw-w64 ships no synchronization
                # import library either — so synthesize one and put it on the
                # library search path.
                + pkgs.lib.optionalString (target == "x86_64-pc-windows-gnu") ''
                  synchronization_lib_dir="$TMPDIR/synchronization-lib"
                  mkdir -p "$synchronization_lib_dir"
                  cat > "$synchronization_lib_dir/synchronization.def" <<'EOF'
                  LIBRARY "api-ms-win-core-synch-l1-2-0.dll"
                  EXPORTS
                  WaitOnAddress
                  WakeByAddressAll
                  WakeByAddressSingle
                  EOF
                  zig dlltool -m i386:x86-64 \
                    -d "$synchronization_lib_dir/synchronization.def" \
                    -l "$synchronization_lib_dir/libsynchronization.a"
                  export RUSTFLAGS="-L native=$synchronization_lib_dir''${RUSTFLAGS:+ $RUSTFLAGS}"
                '';

              doCheck = target == "x86_64-unknown-linux-musl";
            };

          buildReleasePackage =
            target:
            craneLib.buildPackage (
              (makeReleaseArgs target)
              // {
                cargoArtifacts = craneLib.buildDepsOnly (
                  (makeReleaseArgs target)
                  // {
                    extraDummyScript = keepPatchesInDummySrc;
                    buildPhaseCargoCommand = "cargo zigbuild --release -p ry-lang";
                    checkPhaseCargoCommand = "cargo-zigbuild test --release --no-run -p ry-lang";
                  }
                );
                buildPhaseCargoCommand = ''
                  cargoBuildLog=$(mktemp cargoBuildLogXXXX.json)
                  cargo zigbuild --release --message-format json-render-diagnostics -p ry-lang >"$cargoBuildLog"
                '';
                checkPhaseCargoCommand = "cargo-zigbuild test --release -p ry-lang";
                # A release binary runs on machines without Nix, so it must not
                # reference anything in the Nix store: an ELF interpreter or
                # RUNPATH there fails this build instead of shipping.
                allowedReferences = [ ];
              }
            );
        in
        {
          ry-linux-x86_64 = buildReleasePackage "x86_64-unknown-linux-musl";
          ry-macos-aarch64 = buildReleasePackage "aarch64-apple-darwin";
          ry-windows-x86_64 = buildReleasePackage "x86_64-pc-windows-gnu";
        }
      );

      devShells = self.lib.eachSystem (system: {
        default =
          let
            pkgs = self.lib.makePkgs system nixpkgs;
          in
          pkgs.devshell.mkShell {
            motd = "";
            packages = with pkgs; [
              # development environment
              just
              evcxr
              (radianWrapper.override {
                packages = (self.lib.rpkgs pkgs);
                wrapR = true;
              })
              # build tools
              (self.lib.rustToolchain pkgs)
              gnumake
              cargo-edit
              cargo-insta
              cargo-nextest
              # cross compilation
              cargo-zigbuild
              zig
              # libs
              tree-sitter
              # website
              bun
              # for releasing
              zip
            ];
          };
      });
    };
}
