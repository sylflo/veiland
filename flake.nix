# SPDX-License-Identifier: GPL-3.0-or-later
#
# Flake for veiland: builds the locker + plugins, provides the dev shell,
# and exposes checks. `nix flake check` + `nix build` together are the CI.
#
#   nix build            -> veiland-core + reference plugins in ./result/bin
#                           (also runs the test suite in the check phase)
#   nix develop          -> dev shell (Rust toolchain + system libs + tooling)
#   nix flake check      -> fmt + clippy + python (ruff/mypy/pytest)
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
        "veiland-particles"
        "veiland-vignette"
        "veiland-sakura"
        "veiland-snow"
        "veiland-rain"
        "veiland-embers"
        "veiland-fireflies"
        "veiland-gradient"
        "veiland-parallax"
        "veiland-blobs"
        "veiland-raymarcher"
        "veiland-shader"
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

      # The nine PRODUCTION Python widgets, as (installed-name, source-
      # basename) pairs. Installed as `veiland-<name>` executables on PATH so a
      # scene references them by bare name exactly like a compiled Rust plugin
      # (binary = "veiland-weather"); veiland-core execs them off PATH with no
      # idea they are Python. NOT the two battery TEACHING demos (battery.py,
      # battery_cairo.py) — those stay in-tree examples, never installed. See
      # docs/plans/python-distribution.md and project_python_examples_prod_status.
      pythonWidgets = [
        { name = "veiland-now-playing"; src = "now_playing.py"; }
        { name = "veiland-weather"; src = "weather.py"; }
        { name = "veiland-wifi"; src = "wifi.py"; }
        { name = "veiland-ethernet"; src = "ethernet.py"; }
        { name = "veiland-bluetooth"; src = "bluetooth.py"; }
        { name = "veiland-avatar"; src = "avatar.py"; }
        { name = "veiland-markup"; src = "markup.py"; }
        { name = "veiland-shape"; src = "shape.py"; }
        { name = "veiland-battery"; src = "battery_svg.py"; }
      ];

      # The GObject-introspection + drawing stack the installed widgets need at
      # RUNTIME, and the dev shell / python-check need for import parity. These
      # are C libraries + typelibs that `pip` CANNOT supply (librsvg, Pango,
      # HarfBuzz, glib) — the concrete reason the widgets ship as distro
      # packages, not on PyPI (docs/plans/python-distribution.md). Consumed by
      # the package's wrappers and the python-widget-libs check that guards
      # them; the dev shell keeps its own list, so the two can drift.
      # .out explicitly: several of these are multi-output, and their DEFAULT
      # output is NOT the one carrying lib/ or the typelib. pango in particular
      # defaults to its `-bin` output, whose lib/girepository-1.0 is EMPTY — so
      # a bare `pkgs.pango` in a wrapper path silently drops Pango-1.0.typelib
      # and the widget dies with "Namespace Pango not available". Pin `.out`
      # (the typelib + .so live there) on every entry so the wrapper env is
      # correct regardless of a package's default-output choice.
      # gdk-pixbuf is here because librsvg's typelib TRANSITIVELY requires
      # GdkPixbuf-2.0 (Rsvg pulls it in): without it the SVG widgets die with
      # "Typelib file for namespace 'GdkPixbuf' not found". The dev shell masks
      # this via propagation; an isolated wrapper does not, so it must be
      # listed explicitly on both paths.
      pythonWidgetLibs = pkgs: [
        # Every widget needs this one, GI stack or not: the SDK dlopens
        # libgbm.so.1 via ctypes, which gets no RPATH the way a compiled
        # plugin's DT_NEEDED does, so LD_LIBRARY_PATH is all that resolves it.
        pkgs.libgbm.out
        pkgs.librsvg.out
        pkgs.gdk-pixbuf.out
        pkgs.glib.out
        pkgs.pango.out
        pkgs.harfbuzz.out
      ];
      pythonWidgetTypelibs = pkgs: [
        pkgs.librsvg.out
        pkgs.gdk-pixbuf.out
        pkgs.gobject-introspection.out
        pkgs.pango.out
        pkgs.harfbuzz.out
      ];
      # The interpreter the installed widgets run under: stdlib + the four
      # example/companion deps (the SDK itself is stdlib+ctypes and needs none
      # of these; they back the drawing companions + the D-Bus widgets).
      pythonWidgetInterpreter = pkgs:
        pkgs.python3.withPackages (ps: [ ps.pillow ps.pycairo ps.pygobject3 ps.jeepney ]);
    in
    {
      packages = forAllSystems (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "veiland";
          version = "0.3.1";

          src = ./.;

          # Hand the git revision to veiland-core's build.rs for
          # `veiland --version`. A `nix build` copies the working tree WITHOUT
          # `.git/` into the sandbox, so build.rs's `git` fallback finds no repo
          # — but the flake knows its own revision. Shortened to a 7-char hash
          # to match the dev-shell `git rev-parse --short` format. `self.rev` is
          # the clean-tree commit; `self.dirtyRev` is `<hash>-dirty` for a
          # modified tree (shorten the hash, keep the suffix); `""` when neither
          # is known (e.g. a tarball checkout with no flake metadata), which
          # prints the bare version. This is what lets an install off `master`
          # or a pinned commit report exactly which build it is. See
          # veiland-core/build.rs.
          VEILAND_GIT_REV =
            if self ? rev then
              builtins.substring 0 7 self.rev
            else if self ? dirtyRev then
              # dirtyRev is "<40-char-hash>-dirty"; take the first 7 of the hash
              # and re-append the marker so the format stays `abc1234-dirty`.
              "${builtins.substring 0 7 self.dirtyRev}-dirty"
            else
              "";

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
          # system libraries below; makeWrapper generates the Python widget
          # launchers in postInstall (it bakes GI_TYPELIB_PATH / LD_LIBRARY_PATH
          # into each veiland-<name> so the installed widget finds its typelibs
          # the way the dev shell arranges — the wiring pip cannot do).
          nativeBuildInputs = [ pkgs.pkg-config pkgs.makeWrapper ];

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

          # The package's data directory, matching what the .deb, .rpm and
          # PKGBUILD install to /usr/share/veiland. The default scene
          # (compiled into the binary) needs nothing from here — it renders
          # procedurally — but config.example.toml is where the core's
          # "no config file" log line points users to start customising,
          # and the wallpaper serves the sakura gallery scene.
          #
          # config.example.toml, not veiland.example.toml: the other
          # packages rename it on install, and the core's "no config file"
          # log line points users at that name.
          postInstall = ''
            install -Dm0644 packaging/veiland.example.toml \
              "$out/share/veiland/config.example.toml"
            install -Dm0644 docs/examples/assets/sakura-dusk.jpg \
              "$out/share/veiland/sakura-dusk.jpg"

            # Ready-made example scenes, like the other packages install to
            # /usr/share/veiland/examples. The hotplug repro config is a dev
            # tool, not a scene. There is no /usr/share here, so where the
            # FHS packages rewrite the examples' repo-relative asset paths
            # to /usr/share/veiland, this rewrites them to this package's
            # own store share directory. A config copied from here pins that
            # store path — it stays valid until the generation it came from
            # is garbage-collected, and a stale path just means the
            # wallpaper plugin logs it and paints black.
            install -Dm0644 -t "$out/share/veiland/examples" docs/examples/*.toml
            rm "$out/share/veiland/examples/hotplug-repro.toml"
            sed -i "s|docs/examples/assets/|$out/share/veiland/|" \
              "$out/share/veiland/examples/"*.toml

            # --- Python widgets --------------------------------------------
            # Stash the SDK + companions + widget scripts + icons under
            # libexec, PRESERVING the python/examples/ layout: each installed
            # widget script keeps working unmodified, because its own
            # sys.path shim (os.path.dirname(os.path.dirname(__file__)))
            # still resolves to the SDK dir and its ICON_DIR
            # (<script dir>/icons) still resolves to the icons — so no .py
            # source edit is needed. (battery.py / battery_cairo.py are the
            # teaching demos, not shipped: copy only what the widgets import.)
            # (postInstall is one concatenated script, so pydst set here is
            # visible in the per-widget fragments below — no `local`, which is
            # a function-only builtin and errors at top level.)
            pydst="$out/libexec/veiland/python"
            install -dm0755 "$pydst" "$pydst/examples"
            install -Dm0644 -t "$pydst" python/veiland_plugin.py \
              python/veiland_svg.py python/veiland_text.py \
              python/veiland_layout.py python/veiland_dbus.py
            cp -r python/examples/icons "$pydst/examples/icons"

            # Each veiland-<name> is a makeWrapper launcher: it bakes the GI
            # typelib + library paths and the right interpreter, then execs
            # the stashed widget script. The script is installed under its
            # veiland- name (so `ps`/`/proc` and the host's log show the
            # packaged name), inside examples/ so both path shims still fire.
          ''
          + pkgs.lib.concatMapStrings
            (w: ''
              install -Dm0755 "python/examples/${w.src}" \
                "$pydst/examples/${w.name}"
              makeWrapper "${pythonWidgetInterpreter pkgs}/bin/python3" \
                "$out/bin/${w.name}" \
                --add-flags "$pydst/examples/${w.name}" \
                --prefix GI_TYPELIB_PATH : "${pkgs.lib.makeSearchPath "lib/girepository-1.0" (pythonWidgetTypelibs pkgs)}" \
                --prefix LD_LIBRARY_PATH : "${pkgs.lib.makeLibraryPath (pythonWidgetLibs pkgs)}"
            '')
            pythonWidgets
          + ''
            # The example scenes already reference every widget by its bare
            # veiland-<name> (Rust and Python alike), so no binary-line rewrite
            # is needed here — only drop the two teaching-demo scenes, whose
            # widgets (battery.py / battery_cairo.py) are intentionally NOT
            # installed as veiland-* commands (same as hotplug-repro.toml above).
            rm -f "$out/share/veiland/examples/battery_python.toml" \
                  "$out/share/veiland/examples/battery_cairo.toml"
          '';

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

            # Static site generator for the website in site/ (dev-only;
            # CI builds the site with its own pinned zola in
            # .github/workflows/site.yml). `./site/serve.sh` to preview.
            zola

            # cloud-localds, used by scripts/vmtest/*.sh to build the
            # cloud-init seed ISO for the packaging-test VMs. The nixpkgs
            # wrapper bundles genisoimage and qemu-img, so this one package
            # covers the seed step; qemu itself comes from the system.
            cloud-utils

            # Python for the plugin track: drawing libs for the examples --
            # Pillow (the battery examples, PIL upload() convenience), pycairo
            # (the cairo battery rewrite + the now-playing example, drawing
            # zero-copy into buf.map()), pygobject3 (the gi bindings the SVG
            # examples use to render librsvg onto a cairo context via the
            # optional veiland_svg companion, and PangoCairo for now-playing's
            # text), and jeepney (the pure-Python D-Bus client the now-playing
            # example uses to read MPRIS -- its blocking socket fd goes on the
            # pacer's extra_fds, no thread/asyncio); plus pytest + mypy for the
            # Python SDK's codec suite (python/veiland_plugin.py + python/tests).
            # All example/dev-only, never in the package and never imported by
            # the SDK (stdlib + ctypes only). Tooling config (ruff + mypy) lives
            # in python/pyproject.toml.
            (python3.withPackages (
              ps: [ ps.pillow ps.pycairo ps.pytest ps.mypy ps.pygobject3 ps.jeepney ]
            ))

            # librsvg + gobject-introspection back the SVG status-icon examples:
            # veiland_svg loads an SVG through gi.repository.Rsvg and renders it
            # straight onto the example's cairo context (no extra copy). These
            # are C libraries, not python packages, so they live here (and on
            # GI_TYPELIB_PATH / LD_LIBRARY_PATH below), not inside withPackages.
            # Example/companion-only; never an SDK dep.
            librsvg
            gobject-introspection

            # pango + harfbuzz back the now-playing example's text: PangoCairo
            # shapes + end-ellipsizes real titles (long + CJK) onto the cairo
            # context, where cairo's toy show_text cannot. Same C-library story
            # as librsvg above (here + on GI_TYPELIB_PATH / LD_LIBRARY_PATH, not
            # withPackages). Pango's typelib requires HarfBuzz's, so both are
            # needed. Example-only; never an SDK dep.
            pango
            harfbuzz

            # dancing-script: an OFL-1.1 handwriting/cursive font, so the text
            # widgets' font_family key has something fun to resolve to inside the
            # dev shell (the markup demo uses it). fontconfig picks up any font
            # package listed here by scanning its share/fonts dir. Purely a
            # demo/example convenience: the SDK ships no font, font_family falls
            # back to Sans when a family is absent, and nothing at runtime needs
            # this. Add more families here the same way if the examples want them.
            dancing-script

            # google-fonts: the Google Fonts collection, here purely so the Deep
            # Field astronomy scene's clock can resolve "Cormorant Garamond" (the
            # mockup's elegant serif, a variable font with a real Light weight so
            # the thin time reads as designed). A large package, but this is a
            # demo/example dev shell, not a runtime dep -- fontconfig scans its
            # share/fonts like any other font package here. Same role as
            # dancing-script above.
            google-fonts

            # jetbrains-mono: an OFL-1.1 monospace, the exact font the Deep Field
            # mockup uses for its HUD lines (Julian Date, observer tag). The
            # scene's markup widgets set font_family = "JetBrains Mono"; without
            # this they fall back to a generic monospace. Same demo/example role.
            jetbrains-mono

            # ruff: formatter + import-sort + linter for the Python SDK, in
            # one binary (replaces black + isort + flake8). Standalone, so it
            # goes here rather than in the interpreter env above.
            ruff
          ];

          # Let the python demo's ctypes dlopen find libgbm: the shell
          # links it for cargo via inputsFrom, but dlopen doesn't search
          # the Nix store. Same libgbm the workspace builds against, so
          # nothing else in the shell is shadowed. librsvg + glib are here
          # too because the GI loader dlopens librsvg-2.so (and glib's
          # libgobject/libgio) at typelib-load time -- the typelib on
          # GI_TYPELIB_PATH is not enough on its own.
          LD_LIBRARY_PATH =
            pkgs.lib.makeLibraryPath [
              pkgs.libgbm
              pkgs.librsvg
              pkgs.glib
              pkgs.pango
              pkgs.harfbuzz
            ];

          # Point gi.repository at the Rsvg typelib (and the base GObject
          # typelibs), plus Pango/PangoCairo/HarfBuzz for the now-playing text.
          # Without this `gi.require_version("Rsvg", "2.0")` (or "Pango") in the
          # examples throws "Namespace ... not available". NixOS does not
          # populate a default GI search path, so the shell wires it explicitly.
          GI_TYPELIB_PATH = pkgs.lib.makeSearchPath "lib/girepository-1.0" [
            pkgs.librsvg
            pkgs.gobject-introspection
            pkgs.pango
            pkgs.harfbuzz
          ];

          # IN_VEILAND_SHELL is a stable marker any shell can key a prompt
          # off (Nix's own IN_NIX_SHELL doesn't survive `nix develop -c
          # $SHELL`). PS1 tweak below only affects the bash `nix develop`
          # starts; zsh/fish users set their prompt from this var in their
          # own rc. See CONTRIBUTING "Optional git hooks" neighbours.
          shellHook = ''
            export IN_VEILAND_SHELL=1
            PS1="(veiland) $PS1"
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
          # $out here is a marker file, not a directory, so the package's
          # postInstall (which installs data files into $out/share/veiland)
          # would fail on it. The check installs nothing; drop the hook.
          postInstall = "";
          installPhase = ''
            runHook preInstall
            touch "$out"
            runHook postInstall
          '';
        });

        # Python SDK gate: ruff (format + lint) + mypy + pytest over python/.
        # A plain runCommand like the fmt check -- the SDK is stdlib-only, so
        # no build env is needed, just the interpreter + tools. Mirrors the
        # `nix develop` tooling (config in python/pyproject.toml) so CI and the
        # dev shell check the same thing. Pillow + pycairo are in the
        # interpreter env for import parity with the examples (ruff lints
        # them), though the pytest suite itself needs neither.
        #
        # Copy python/ into the writable build dir first: the sources live in
        # the read-only /nix/store, and ruff/mypy/pytest all want to write
        # cache dirs (.ruff_cache, .mypy_cache, .pytest_cache) next to them.
        # Cache flags are belt-and-suspenders on top of the writable copy.
        python = pkgs.runCommand "veiland-python-check"
          {
            nativeBuildInputs = [
              (pkgs.python3.withPackages
                (ps: [ ps.pillow ps.pycairo ps.pytest ps.mypy ps.pygobject3 ps.jeepney ]))
              pkgs.ruff
            ];

            # gi/librsvg wiring for import parity with the dev shell (the
            # same rationale pillow/pycairo are here for). The gate itself --
            # ruff + mypy + pytest -- never imports gi at runtime: mypy is
            # static (veiland_svg.py + the examples ARE in its `files` now,
            # but gi.* is follow_imports=skip in pyproject.toml, so the
            # typelibs are never loaded), ruff doesn't execute code, and
            # pytest only runs tests/ (which don't import gi). So these are
            # parity / future-proofing, not a hard requirement; if the typelib
            # ever misbehaves in the hermetic builder, dropping them keeps CI
            # green.
            GI_TYPELIB_PATH = pkgs.lib.makeSearchPath "lib/girepository-1.0" [
              pkgs.librsvg
              pkgs.gobject-introspection
              pkgs.pango
              pkgs.harfbuzz
            ];
            LD_LIBRARY_PATH =
              pkgs.lib.makeLibraryPath [
                pkgs.librsvg
                pkgs.glib
                pkgs.pango
                pkgs.harfbuzz
              ];
          }
          ''
            cp -r ${./.}/python ./python
            chmod -R u+w ./python
            cd ./python
            ruff format --check .
            ruff check --no-cache .
            mypy --no-incremental --config-file pyproject.toml
            pytest tests -q -p no:cacheprovider
            touch "$out"
          '';

        # Gate on the installed widgets' runtime library path, which the SDK
        # gate above cannot see: it consumes pythonWidgetLibs, so a widget that
        # only runs under `nix develop` fails CI instead of the lock screen.
        # _load_gbm, not GbmDevice -- dlopen is hermetic, a DRM node is not.
        python-widget-libs = pkgs.runCommand "veiland-python-widget-libs-check"
          {
            nativeBuildInputs = [ (pythonWidgetInterpreter pkgs) ];
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (pythonWidgetLibs pkgs);
          }
          ''
            PYTHONDONTWRITEBYTECODE=1 PYTHONPATH=${./python} \
              python3 -c 'import veiland_plugin; veiland_plugin._load_gbm()'
            touch "$out"
          '';
      });

      # NixOS module: `services.veiland.enable = true` installs the
      # package and registers the PAM service. Not per-system — the
      # consuming config supplies its own pkgs/system.
      nixosModules.default = import ./nix/module.nix self;
    };
}
