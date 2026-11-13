{
  # Inputs are git URLs, not `github:`, because cloud agent sessions reach GitHub through a proxy
  # that serves git clones of public repositories but refuses the archive tarballs `github:`
  # fetches (.agents/memory/decisions.md, "Flake inputs are git URLs").
  inputs = {
    systems.url = "git+https://github.com/nix-systems/default?shallow=1";
    nixpkgs.url = "git+https://github.com/NixOS/nixpkgs?ref=nixos-unstable&shallow=1";
    rust-overlay = {
      url = "git+https://github.com/oxalica/rust-overlay?shallow=1";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "git+https://github.com/ipetkov/crane?shallow=1";
  };

  # .github/workflows/cachix.yml pushes the x86_64-linux package and dev shell here.
  nixConfig = {
    extra-substituters = [ "https://ry.cachix.org" ];
    extra-trusted-public-keys = [ "ry.cachix.org-1:6OKbqSFUFIsagxwxTo06JWHSEWuEtQp4kgQtR/TuVyY=" ];
  };

  outputs =
    {
      self,
      systems,
      nixpkgs,
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
            overlays = [ (import rust-overlay) ];
          };
        # GNU R's source release that ry's base R is built from, pinned by tools/base-r.
        baseRVersion = nixpkgs.lib.trim (builtins.readFile ./tools/base-r/VERSION);
        baseRSource =
          pkgs:
          pkgs.fetchurl {
            url = "https://cran.r-project.org/src/base/R-${pkgs.lib.versions.major self.lib.baseRVersion}/R-${self.lib.baseRVersion}.tar.gz";
            sha256 = builtins.head (
              pkgs.lib.splitString " " (builtins.readFile ./tools/base-r/SHA256SUMS)
            );
          };
        # GNU R as the oracle recorded it (crates/oracle/cases/gnu-r.txt): the release ry bundles,
        # built the way Posit's ubuntu-2404 package is, so that every numeric digit matches. nixpkgs
        # ships another release with another BLAS, compiler and glibc, and each of those moves digits
        # (.agents/memory/decisions.md, "CI runs in the flake's dev shell").
        gnuR =
          pkgs:
          let
            inherit (pkgs) lib;
            # Ubuntu 24.04's reference LAPACK, the very binary the recordings loaded: La_library()
            # names liblapack.so.3.12.0. It calls BLAS through libblas.so.3, which R's own libRblas
            # shadows once R has loaded it, as on Ubuntu.
            lapack = pkgs.stdenv.mkDerivation {
              pname = "ubuntu-lapack";
              version = "3.12.0-3build1.1";
              srcs = [
                (pkgs.fetchurl {
                  url = "http://archive.ubuntu.com/ubuntu/pool/main/l/lapack/liblapack3_3.12.0-3build1.1_amd64.deb";
                  sha256 = "16ymmshvbx4wcyfn96c0bv4d0zn0ww8i5ivdmvh97lsgkl2dbkh5";
                })
                (pkgs.fetchurl {
                  url = "http://archive.ubuntu.com/ubuntu/pool/main/l/lapack/libblas3_3.12.0-3build1.1_amd64.deb";
                  sha256 = "0n57hdxppb6vazjzs7pa6xhy320pfwimgdssgnzpgzwk12nz03hv";
                })
              ];
              nativeBuildInputs = [
                pkgs.dpkg
                pkgs.autoPatchelfHook
              ];
              buildInputs = [ pkgs.gfortran13.cc.lib ];
              unpackPhase = ''
                for source in $srcs; do dpkg-deb -x "$source" .; done
              '';
              installPhase = ''
                mkdir -p "$out/lib"
                cp -P usr/lib/x86_64-linux-gnu/lapack/liblapack.so.3* usr/lib/x86_64-linux-gnu/blas/libblas.so.3* "$out/lib/"
                ln -s liblapack.so.3 "$out/lib/liblapack.so"
              '';
            };
            # rlang at exactly the release ry's native trace_back follows (FAST_PATHS in
            # crates/interp/src/package_fast_paths.rs), so the rlang oracle cases, the
            # native-package-functions scenario and the trace_back fuzz harness run instead of
            # skipping. It goes into R's own library, as tools/reference-r.sh installs it. Bump it
            # together with that table.
            rlang = pkgs.fetchurl {
              urls = [
                "https://cloud.r-project.org/src/contrib/rlang_1.3.0.tar.gz"
                "https://cloud.r-project.org/src/contrib/Archive/rlang/rlang_1.3.0.tar.gz"
              ];
              sha256 = "1717ckqcn0xm31jhhwp5lp587xca28v8jpm18gs6a8p795ym7al0";
            };
          in
          pkgs.gcc13Stdenv.mkDerivation {
            pname = "gnu-r";
            version = self.lib.baseRVersion;
            src = self.lib.baseRSource pkgs;
            nativeBuildInputs = [
              pkgs.gfortran13
              pkgs.pkg-config
              pkgs.perl
              pkgs.which
              # configure checks date-time conversions against the time zone database.
              pkgs.tzdata
            ];
            # tools/dev-setup.sh's libraries for R; png() draws through cairo only with pango.
            buildInputs = with pkgs; [
              bzip2
              cairo
              curl
              icu74
              libdeflate
              libjpeg
              libpng
              libtiff
              libx11
              libxt
              ncurses
              pango
              pcre2
              readline
              xz
              zlib
              zstd
              lapack
            ];
            # tools/reference-r.sh's configure line, which the recordings were made with.
            configureFlags = [
              "--enable-R-shlib"
              "--enable-memory-profiling"
              "--with-x"
              "--without-recommended-packages"
              "--disable-java"
              "CC=${pkgs.gcc13}/bin/gcc"
              "CXX=${pkgs.gcc13}/bin/g++"
              "FC=${pkgs.gfortran13}/bin/gfortran"
              # glibc 2.41 added these, and R calls glibc's where it finds them; the recordings'
              # glibc 2.39 had none, so R used its own, which round differently.
              "ac_cv_func_atanpi=no"
              "ac_cv_func_atan2pi=no"
              "ac_cv_func_cospi=no"
              "ac_cv_func_sinpi=no"
              "ac_cv_func_tanpi=no"
              "ac_cv_func_pown=no"
            ];
            enableParallelBuilding = true;
            postInstall = ''
              HOME="$TMPDIR" "$out/bin/R" CMD INSTALL ${rlang}
            '';
            meta = {
              mainProgram = "R";
              platforms = lib.platforms.linux;
            };
          };
        # The compiler rustup builds with, from rust-toolchain.toml, plus what nix alone needs: the
        # release cross targets and rust-analyzer for the dev shell.
        rustToolchain =
          pkgs:
          let
            pinned = (builtins.fromTOML (builtins.readFile ./rust-toolchain.toml)).toolchain;
          in
          pkgs.rust-bin.fromRustupToolchain (
            pinned
            // {
              targets = (pinned.targets or [ ]) ++ [
                "x86_64-unknown-linux-gnu"
                "x86_64-pc-windows-gnu"
                "aarch64-apple-darwin"
              ];
              components = (pinned.components or [ ]) ++ [ "rust-analyzer" ];
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
              # Non-Rust files that `commonCargoSources` drops: libR's C sources and headers, the
              # stubs and fixtures read by `include_str!` and tests, and the release metadata.
              ./crates
              ./types
              ./editors/code/package.json
              ./tools/base-r
            ];
          };

          baseRSource = self.lib.baseRSource pkgs;

          commonArgs = {
            inherit src;
            pname = "ry";
            strictDeps = true;
          };

          # libR links zlib, bzip2 and xz (crates/libr/build.rs).
          commonArgsNative = commonArgs // {
            cargoExtraArgs = "-p ry-lang -p libr -p rcmd";
            buildInputs = [
              pkgs.zlib
              pkgs.bzip2
              pkgs.xz
            ];
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
          packageUnwrapped = craneLib.buildPackage (
            commonArgsNative
            // {
              cargoArtifacts = cargoArtifactsNative;
              # libr's own suites drive GNU R binaries and the corpus, which the sandbox lacks. The
              # docs examples test reads docs/, which the source set leaves out so that a docs edit
              # does not rebuild the package; `cargo test` runs it everywhere else.
              checkPhaseCargoCommand = "cargoWithProfile test -p ry-lang -- --skip docs_examples_hold";
              # GNU R lets the CLI's runtime and `ry R` tests run instead of skipping.
              nativeCheckInputs = [ pkgs.R ];
              # Stub materialization writes to the user cache; the sandbox's HOME is read-only.
              preCheck = ''
                export XDG_CACHE_HOME="$TMPDIR/.cache"
              '';
              # `ry run` finds ry's runtime at ../lib/ry/libR.so relative to the executable, and the
              # name must stay libR.so, the soname compiled packages record. `ry load` looks for
              # ry-rcmd beside the runtime to compile a package's src/. Base R is built with them
              # into ../lib/ry/base-r, where `ry` looks for it.
              postInstall = ''
                mkdir -p "$out/lib/ry"
                mv "$out"/lib/libR${pkgs.stdenv.hostPlatform.extensions.sharedLibrary} "$out/lib/ry/"
                mv "$out"/bin/ry-rcmd "$out/lib/ry/"
                rm -f "$out"/lib/libR.a "$out"/lib/*.rlib
                R_SOURCE_TARBALL=${baseRSource} R_SOURCE_CACHE="$TMPDIR/base-r-source" \
                  bash tools/base-r/build.sh "$out/lib/ry/base-r" "$out/lib/ry/ry-rcmd" \
                  "$out/lib/ry/libR${pkgs.stdenv.hostPlatform.extensions.sharedLibrary}"
              '';
              meta.mainProgram = "ry";
            }
          );

          # `ry R` runs GNU R, and a Nix-built binary cannot load a libR built against another
          # distribution's libraries, so nixpkgs' R comes first on PATH. ry's runtime uses its own
          # base R unless the user sets R_HOME.
          packageNative =
            pkgs.runCommand "ry-${packageUnwrapped.version}"
              {
                nativeBuildInputs = [ pkgs.makeWrapper ];
                meta.mainProgram = "ry";
              }
              ''
                mkdir -p "$out/bin" "$out/lib"
                ln -s ${packageUnwrapped}/lib/ry "$out/lib/ry"
                makeWrapper ${packageUnwrapped}/bin/ry "$out/bin/ry" \
                  --prefix PATH : ${pkgs.R}/bin
              '';

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
          ry = packageNative;
          ry-unwrapped = packageUnwrapped;
          ry-linux-x86_64 = buildCrossPackage "x86_64-unknown-linux-gnu.2.17";
          ry-macos-aarch64 = buildCrossPackage "aarch64-apple-darwin";
          ry-windows-x86_64 = buildCrossPackage "x86_64-pc-windows-gnu";
        }
      );

      # `ci` holds everything CI's test job runs on (`nix develop .#ci -c just ci`), and `default`
      # adds the tools people and agents develop with, so both test against the same GNU R,
      # compiler, glibc and locales.
      devShells = self.lib.eachSystem (
        system:
        let
          pkgs = self.lib.makePkgs system nixpkgs;
          gnuR = self.lib.gnuR pkgs;
          # GCC 13, the compiler of the ubuntu-2404 GNU R the oracle recorded, so that C an oracle
          # case compiles orders NA and NaN as there.
          makeShell =
            rustToolchain: packages:
            (pkgs.mkShell.override { stdenv = pkgs.gcc13Stdenv; }) {
              # libR links zlib, bzip2 and xz (crates/libr/build.rs).
              buildInputs = [
                pkgs.zlib
                pkgs.bzip2
                pkgs.xz
              ];
              packages = [
                rustToolchain
                gnuR
                # tools/base-r/lapack.sh compiles LAPACK's Fortran.
                pkgs.gfortran13
                pkgs.just
                pkgs.python3
                pkgs.glibc.bin
              ]
              ++ packages;
              # Nix's glibc looks for time zones in its own store path, which has none.
              TZDIR = "${pkgs.tzdata}/share/zoneinfo";
              # The locales oracle cases switch to (`# oracle: requires locale`), which
              # tools/oracle-locales.sh checks for.
              LOCALE_ARCHIVE = "${
                pkgs.glibcLocales.override {
                  allLocales = false;
                  locales = [
                    "C.UTF-8/UTF-8"
                    "en_US.UTF-8/UTF-8"
                    "en_US/ISO-8859-1"
                  ];
                }
              }/lib/locale/locale-archive";
            };
        in
        {
          ci = makeShell (pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml) [ ];
          default = makeShell (self.lib.rustToolchain pkgs) (
            with pkgs;
            [
              evcxr
              # radian runs the shell's GNU R, with renv and devtools built for it on its own
              # library path, so `R` and `Rscript` stay the ones CI runs.
              (radianWrapper.override {
                R = gnuR;
                recommendedPackages = [ ];
                packages = with (rPackages.override { R = gnuR; }); [
                  renv
                  devtools
                ];
              })
              cargo-edit
              cargo-insta
              cargo-nextest
              cargo-zigbuild
              zig
              tree-sitter
              bun
              zip
            ]
          );
        }
      );

      # `nix flake check` builds the package, which runs ry-lang's tests, and the dev shells.
      checks = self.lib.eachSystem (system: {
        ry = self.packages.${system}.ry;
        dev-shell = self.devShells.${system}.default;
      });
    };
}
