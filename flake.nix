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
              # Non-Rust files read by `include_str!` and tests, which `commonCargoSources` drops.
              ./types
              ./crates/format/tests/format
              ./crates/ry/ry.exe.manifest
              ./editors/code/package.json
            ];
          };

          commonArgs = {
            inherit src;
            pname = "ry";
            strictDeps = true;
          };

          commonArgsNative = commonArgs // {
            cargoExtraArgs = "-p ry-lang";
          };

          # The dep-only build stubs out every local .rs file, but chrono compiles against the
          # patched crate's real sources; interpolating only ./patches keeps the cache stable.
          keepPatchesInDummySrc = ''
            rm -rf "$out"/patches
            cp -r --no-preserve=mode ${./patches} "$out"/patches
          '';

          # For NixOS and flake users; the only build the sandbox can run, so it carries the tests.
          cargoArtifactsNative = craneLib.buildDepsOnly (
            commonArgsNative
            // {
              extraDummyScript = keepPatchesInDummySrc;
            }
          );
          packageNative = craneLib.buildPackage (
            commonArgsNative
            // {
              cargoArtifacts = cargoArtifactsNative;
              # Stub materialization writes to the user cache; the sandbox's HOME is read-only.
              preCheck = ''
                export XDG_CACHE_HOME="$TMPDIR/.cache"
              '';
            }
          );

          # Release binaries are cross-linked by zig and cannot run in the sandbox, so they skip
          # tests. Linux targets glibc 2.17 with the standard /lib64 loader, so it can load R. zig
          # has only libSystem stubs for macOS, so the graph stays framework-free (patches/iana-time-zone).
          makeCrossArgs =
            target:
            commonArgs
            // {
              CARGO_BUILD_TARGET = builtins.head (pkgs.lib.splitString "." target);

              nativeBuildInputs = [
                pkgs.cargo-zigbuild
                pkgs.zig
              ];

              preBuild =
                ''
                  export XDG_CACHE_HOME="$TMPDIR/.cache"
                ''
                # winapi (via reedline and crossterm) links the "synchronization" import library;
                # cargo-zigbuild disables winapi's bundled ones and zig's mingw-w64 lacks it,
                # so synthesize one.
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

              doCheck = false;
            };

          makeCrossArtifacts =
            target:
            craneLib.buildDepsOnly (
              (makeCrossArgs target)
              // {
                extraDummyScript = keepPatchesInDummySrc;
                buildPhaseCargoCommand = "cargo zigbuild --release --target ${target} -p ry-lang";
                checkPhaseCargoCommand = "true";
              }
            );

          buildCrossPackage =
            target:
            craneLib.buildPackage (
              (makeCrossArgs target)
              // {
                cargoArtifacts = makeCrossArtifacts target;
                buildPhaseCargoCommand = ''
                  cargoBuildLog=$(mktemp cargoBuildLogXXXX.json)
                  cargo zigbuild --release --target ${target} --message-format json-render-diagnostics -p ry-lang >"$cargoBuildLog"
                '';
                # A store path in a release binary (ELF interpreter, RUNPATH) fails the build.
                allowedReferences = [ ];
              }
            );
        in
        {
          default = packageNative;
          ry-linux-x86_64 = buildCrossPackage "x86_64-unknown-linux-gnu.2.17";
          ry-macos-aarch64 = buildCrossPackage "aarch64-apple-darwin";
          ry-windows-x86_64 = buildCrossPackage "x86_64-pc-windows-gnu";
        }
      );

      devShells = self.lib.eachSystem (system: {
        default =
          let
            pkgs = self.lib.makePkgs system nixpkgs;
          in
          pkgs.devshell.mkShell {
            motd = "";
            # libR links zlib, bzip2 and xz (crates/libr/build.rs). `packages` only extends PATH,
            # so the linker finds them through LIBRARY_PATH and the loader through LD_LIBRARY_PATH.
            env =
              let
                compressionLibraries = pkgs.lib.makeLibraryPath [
                  pkgs.zlib
                  pkgs.bzip2
                  pkgs.xz
                ];
              in
              [
                {
                  name = "LIBRARY_PATH";
                  prefix = compressionLibraries;
                }
                {
                  name = "LD_LIBRARY_PATH";
                  prefix = compressionLibraries;
                }
              ];
            packages = with pkgs; [
              just
              evcxr
              (radianWrapper.override {
                packages = (self.lib.rpkgs pkgs);
                wrapR = true;
              })
              (self.lib.rustToolchain pkgs)
              gnumake
              cargo-edit
              cargo-insta
              cargo-nextest
              cargo-zigbuild
              zig
              tree-sitter
              bun
              zip
            ];
          };
      });
    };
}
