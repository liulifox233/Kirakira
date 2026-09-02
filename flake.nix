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
            (with pkgs; lib.optionals stdenv.hostPlatform.isLinux [
              alsa-lib
              libpulseaudio
            ])
            ++ (with pkgs; lib.optionals stdenv.hostPlatform.isDarwin [ libiconv ]);

          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
          shellHook = ''
            export CARGO_NET_GIT_FETCH_WITH_CLI=true
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
