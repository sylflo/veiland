# SPDX-License-Identifier: GPL-3.0-or-later
#
# Flake for veiland: builds the locker + plugins, provides the dev shell,
# and exposes checks. `nix flake check` + `nix build` together are the CI.
#
#   nix build            -> veiland-core + reference plugins in ./result/bin
#                           (also runs the test suite in the check phase)
#   nix develop          -> dev shell (Rust toolchain + system libs + tooling)
#   nix flake check      -> fmt + clippy
{
  description = "Wayland screen locker with process-isolated GPU plugins";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    # Nightly Rust toolchains for the fuzz dev shell only. cargo-fuzz
    # needs nightly (sanitizer instrumentation + `-Z build-std`); the
    # package build and default dev shell stay on nixpkgs' stable rustc.
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, fenix }:
    let
      # DMA-BUF / GBM are Linux-only, so we only target Linux arches.
      systems = [ "x86_64-linux" "aarch64-linux" ];

      # Map a function-of-pkgs over every supported system, producing
      # the `{ <system> = ...; }` attrset shape every flake output needs.
      # `genAttrs` is plain nixpkgs.lib — no extra flake input required.
      forAllSystems = f:
        nixpkgs.lib.genAttrs systems
          (system: f (import nixpkgs { inherit system; }));

      # The shipped crate set: the locker plus every real plugin. Used
      # by the package build and the test phase, so the list lives in
      # exactly one place. The stress test plugin is deliberately not
      # built or installed by the package; the clippy check runs
      # workspace-wide, so stress is still linted and cannot bitrot
      # invisibly.
      realCrates = [
        "veiland-core"
        "veiland-wallpaper"
        "veiland-clock"
        "veiland-particles"
        "veiland-vignette"
        "veiland-label"
        "veiland-sakura"
        "veiland-snow"
        "veiland-rain"
        "veiland-embers"
        "veiland-fireflies"
        "veiland-gradient"
        "veiland-parallax"
        "veiland-blobs"
        "veiland-raymarcher"
      ];
      crateFlags = nixpkgs.lib.concatMap (c: [ "-p" c ]) realCrates;

      # Workspace library crates. Their code is compiled into every
      # binary above, but `cargo test -p` only runs the tests of the
      # packages it names — dependency crates' suites are skipped. So
      # the test phase must name them explicitly or their tests never
      # run anywhere (veiland-protocol is the untrusted-input codec,
      # and its suite is where fuzz crashes get promoted to regression
      # tests). Test phase only: nothing extra is built or installed.
      libCrates = [
        "veiland-protocol"
        "veiland-plugin"
        "veiland-text"
      ];
      testCrateFlags =
        nixpkgs.lib.concatMap (c: [ "-p" c ]) (realCrates ++ libCrates);
    in
    {
      packages = forAllSystems (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "veiland";
          version = "0.1.0";

          src = ./.;

          # Reproducible dep fetch straight from the committed lockfile —
          # no cargoHash to maintain, no network in the sandbox.
          cargoLock.lockFile = ./Cargo.lock;

          # Build only the real set (see `realCrates` above).
          cargoBuildFlags = crateFlags;

          # Test the shipped set plus the library crates compiled into
          # it (see `libCrates` above). Only the stress test plugin
          # stays out. The GPU-requiring veiland-plugin fence test
          # (tests/sync.rs) is #[ignore]d and self-excludes.
          cargoTestFlags = testCrateFlags;

          # `spawn_true_exits_zero` shells out to `/bin/true`, which the
          # hermetic Nix build sandbox does not provide (no /bin, no
          # /usr/bin, no system profile). Skip just that test here; it
          # still runs under `cargo test` on a normal filesystem.
          checkFlags = [ "--skip=plugin::spawn::tests::spawn_true_exits_zero" ];

          # pkg-config lets the -sys crates' build scripts locate the
          # system libraries below.
          nativeBuildInputs = [ pkgs.pkg-config ];

          # Linked libraries. Maps 1:1 to the -sys crates:
          #   linux-pam    -> pam-sys2
          #   libGL/mesa   -> khronos-egl (static EGL), gbm-sys
          #   libdrm       -> drm-sys
          #   wayland      -> wayland-sys
          #   libxkbcommon -> xkbcommon
          buildInputs = with pkgs; [
            linux-pam
            libGL
            libgbm
            libdrm
            wayland
            libxkbcommon
          ];

          meta = {
            description = "Wayland screen locker with process-isolated GPU plugins";
            homepage = "https://github.com/sylflo/veiland";
            license = pkgs.lib.licenses.gpl3Plus;
            platforms = pkgs.lib.platforms.linux;
            mainProgram = "veiland";
          };
        };
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          name = "veiland-dev";

          # Inherit the package's build + link inputs (pkg-config, mesa,
          # libpam, wayland, ...) so the dependency list lives in exactly
          # one place — the package derivation.
          inputsFrom = [ self.packages.${pkgs.stdenv.hostPlatform.system}.default ];

          # Tools the package build doesn't need but a developer does:
          # a Rust toolchain on PATH, plus recording tooling (dev-only,
          # never in the package).
          packages = with pkgs; [
            rustc
            cargo
            clippy
            rustfmt
            rust-analyzer

            # Capture the animated plugin scenes for the README gallery and
            # launch GIFs: wf-recorder grabs a Wayland output to mp4, ffmpeg
            # converts mp4 -> GIF (palettegen/paletteuse for clean colors).
            # Every scene animates, so a still screenshot won't do.
            wf-recorder
            ffmpeg

            # GitHub CLI for cutting releases (gh release create ...) and
            # other repo operations without leaving the shell.
            gh

            # cloud-localds, used by scripts/vmtest/*.sh to build the
            # cloud-init seed ISO for the packaging-test VMs. The nixpkgs
            # wrapper bundles genisoimage and qemu-img, so this one package
            # covers the seed step; qemu itself comes from the system.
            cloud-utils
          ];

          shellHook = ''
            echo "veiland dev shell"
          '';
        };

        # Fuzzing shell: `nix develop .#fuzz`, then
        #   cargo fuzz run client_decode
        # from veiland-protocol/fuzz/. cargo-fuzz drives a nightly rustc
        # under the hood, so we put a nightly toolchain (with rust-src,
        # needed for its `-Z build-std`) plus cargo-fuzz on PATH. The
        # system libs the protocol crate links come from the package via
        # inputsFrom, same as the default shell.
        fuzz =
          let
            system = pkgs.stdenv.hostPlatform.system;
            # Nightly with rust-src: cargo-fuzz rebuilds std with the
            # sanitizer via `-Z build-std`, which needs the std source.
            toolchain = fenix.packages.${system}.complete.withComponents [
              "cargo"
              "rustc"
              "rust-src"
              "clippy"
              "rustfmt"
              "rust-analyzer"
            ];
          in
          pkgs.mkShell {
            name = "veiland-fuzz";

            # Same system libs as the package (the protocol crate itself
            # links nothing, but veiland-protocol builds clean inside the
            # workspace env and this keeps parity with the default shell).
            inputsFrom = [ self.packages.${system}.default ];

            packages = [
              toolchain
              pkgs.cargo-fuzz
            ];

            # cargo-fuzz's instrumented binaries link the C++ sanitizer
            # runtime (libclang_rt, libstdc++) dynamically, and on NixOS
            # those aren't on any default loader path. Point the loader at
            # the compiler's own runtime libs so `cargo fuzz run` doesn't
            # die with a missing-.so error at launch.
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [
              pkgs.stdenv.cc.cc.lib
            ];

            shellHook = ''
              echo "veiland fuzz shell (nightly + cargo-fuzz)"
              echo "  cd veiland-protocol/fuzz"
              echo "  cargo fuzz run client_decode"
            '';
          };
      });

      checks = forAllSystems (pkgs: {
        # Formatting: cheap, no compilation. Just needs rustfmt + source.
        fmt = pkgs.runCommand "veiland-fmt-check"
          { nativeBuildInputs = [ pkgs.rustfmt pkgs.cargo ]; }
          ''
            cd ${./.}
            cargo fmt --all -- --check
            touch "$out"
          '';

        # Clippy type-checks the whole workspace, so it needs the package's
        # full build environment (system libs + vendored deps). Derive it
        # from the package via overrideAttrs and swap the build for clippy,
        # so the dependency list stays defined in exactly one place.
        clippy = self.packages.${pkgs.stdenv.hostPlatform.system}.default.overrideAttrs (old: {
          pname = "veiland-clippy-check";
          nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [ pkgs.clippy ];
          # Replace build + install with a single workspace-wide clippy
          # invocation, denying on any warning. --workspace rather than
          # the package's `realCrates`: with -p, cargo lints only the
          # named packages, so the library crates (veiland-protocol,
          # veiland-plugin, veiland-text) and the stress plugin were
          # compiled as dependencies or not at all and never linted.
          # Workspace-wide also makes CONTRIBUTING's plain-cargo
          # equivalent (`cargo clippy --all-targets`) lint the same set
          # as CI. Skip the test phase; tests run in the package.
          buildPhase = ''
            runHook preBuild
            cargo clippy --workspace --all-targets -- -D warnings
            runHook postBuild
          '';
          doCheck = false;
          installPhase = ''
            runHook preInstall
            touch "$out"
            runHook postInstall
          '';
        });
      });

      # NixOS module: `services.veiland.enable = true` installs the
      # package and registers the PAM service. Not per-system — the
      # consuming config supplies its own pkgs/system.
      nixosModules.default = import ./nix/module.nix self;
    };
}
