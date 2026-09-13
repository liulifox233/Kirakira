{
  description = "Kirakira reproducible development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          config.allowUnfree = true;
        };
        rust = pkgs.rustc;

        # The desktop binary dlopens its windowing and graphics libraries at
        # startup: winit needs libwayland-client.so.0, libwayland-cursor.so.0
        # and libxkbcommon.so.0, and wgpu (Vulkan-only on Linux) needs
        # libvulkan.so.1.  dlopen resolves through LD_LIBRARY_PATH and the
        # calling binary's RUNPATH, and buildInputs only bakes a RUNPATH entry
        # for libraries that are linked in — never for these — so the shell
        # exports them below.  The loader discovers its drivers through the
        # host's Vulkan ICD directories (NixOS serves those from
        # /run/opengl-driver, others from /usr/share/vulkan), so no driver
        # package is needed here.
        linuxGraphics = with pkgs; [
          wayland
          libxkbcommon
          vulkan-loader
        ];

        # Wine is deliberately not part of the default shell: its closure is
        # large (see scripts/wine/run.sh) and only the missions that run
        # reference Windows binaries — verifying a written save against the
        # real engine, double-checking plugin behaviour — need it.  `nix
        # develop .#wine` gets that separate shell; scripts/wine/run.sh drives
        # everything in it from a plain non-shell session.
        #
        # wineWow64Packages (not wine64) is required: the official krkrz
        # Windows releases are 32-bit, and only the wow64 build runs them.
        # The X11 entries are there so the engine can run on a virtual display
        # (Xvfb) with no physical screen: xdotool drives it, ImageMagick takes
        # the screenshots, xdpyinfo/xwininfo are what one inspects the display
        # with when a window misbehaves, mesa-demos is the glxinfo the GL path
        # gets diagnosed with, and p7zip unpacks the engine release.
        #
        # noto-fonts-cjk-sans is what makes the games' Chinese/Japanese text
        # legible: Wine starts from a Latin-only font set, so every CJK glyph
        # otherwise renders as a box.  scripts/wine/run.sh copies the font
        # into the prefix and points Wine's font substitutions at it.
        wineTools = with pkgs; [
          wineWow64Packages.stable
          xvfb
          xdotool
          xwininfo
          xdpyinfo
          mesa-demos
          imagemagick
          p7zip
          curl
          noto-fonts-cjk-sans
        ];
      in {
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            rust
            cargo
            rustfmt
            clippy
            wasm-bindgen-cli
            wasm-pack
            binaryen
            lld
            nodejs
            pnpm
            pkg-config
            cmake
            python3
            # The embedded-FFmpeg feature (`cargo build -p krkr-video
            # --features ffmpeg`) compiles FFmpeg from source through
            # ffmpeg-sys-next: nasm assembles FFmpeg's x86 SIMD kernels, and
            # the build script's bindgen run needs libclang — LIBCLANG_PATH
            # below points it at this same store path.  (gcc, make, git and
            # perl are already reachable from the shell.)
            nasm
            llvmPackages.libclang
          ];

          # Native audio and image crates use these libraries through
          # pkg-config. Keeping them in the shell makes tests reproducible on
          # NixOS and macOS without changing Cargo manifests per host.
          buildInputs =
            (with pkgs; [ libopus ])
            ++ (with pkgs; lib.optionals stdenv.hostPlatform.isLinux
              ([ alsa-lib libpulseaudio ] ++ linuxGraphics))
            ++ (with pkgs; lib.optionals stdenv.hostPlatform.isDarwin [ libiconv ]);

          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";

          # ffmpeg-sys-next's bindgen run finds libclang through this; the
          # libclang package above only supplies it in the store, not on
          # PATH.
          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

          # libclang never reads the compiler wrappers' NIX_CFLAGS_COMPILE,
          # so bindgen gets the C library headers explicitly on Linux; without
          # this the libavutil `#include <errno.h>` makes the bindgen step
          # panic.
          BINDGEN_EXTRA_CLANG_ARGS =
            pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux
              "-isystem ${pkgs.stdenv.cc.libc.dev}/include";

          shellHook = ''
            export CARGO_NET_GIT_FETCH_WITH_CLI=true
            ${pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
              # Already-linked binaries dlopen their window system and Vulkan
              # libraries, so put them on the loader path for every run inside
              # the shell, not only for freshly linked ones.
              export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath linuxGraphics}''${LD_LIBRARY_PATH:+:''${LD_LIBRARY_PATH}}"
            ''}
            echo "Kirakira dev shell: $(rustc --version) / $(node --version)"
            kirakira_wasm_target_libdir="$(rustc --target wasm32-unknown-unknown --print target-libdir 2>/dev/null || true)"
            if [ ! -d "$kirakira_wasm_target_libdir" ]; then
              echo "error: the selected nixpkgs Rust toolchain does not provide wasm32-unknown-unknown" >&2
              exit 1
            fi
            echo "wasm target: $kirakira_wasm_target_libdir"
          '';
        };

        devShells.wine = pkgs.mkShell {
          packages = wineTools;

          # scripts/wine/run.sh copies these fonts into the prefix's
          # drive_c/windows/Fonts; Wine itself starts with a Latin-only set.
          KIRA_WINE_FONT_DIR = "${pkgs.noto-fonts-cjk-sans}/share/fonts";

          shellHook = ''
            echo "Kirakira wine shell: $(wine --version)"
            echo "  WINEARCH is not set here; scripts/wine/run.sh keeps the"
            echo "  prefix (and the game scratch tree) under a scratch root so"
            echo "  nothing in the checkout is written."
          '';
        };
      });
}
