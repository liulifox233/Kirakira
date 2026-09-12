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
      });
}
