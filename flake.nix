{
  description = "wlgrid - Wayland grid launcher";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f system);
    in {
      packages = forAllSystems (system:
        let
          pkgs = import nixpkgs { inherit system; };
          runtimeLibs = with pkgs; [
            wayland
            libxkbcommon
            mesa
            # libglvnd provides the dlopen'd EGL/GLES dispatcher
            # (libEGL.so.1 / libGLESv2.so.2); mesa only ships the vendor ICD.
            libglvnd
          ];
        in {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "wlgrid";
            version = "0.1.0";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;

            nativeBuildInputs = with pkgs; [
              pkg-config
              mold
              clang
            ];

            buildInputs = runtimeLibs;

            postFixup = ''
              patchelf --set-rpath "${pkgs.lib.makeLibraryPath runtimeLibs}" $out/bin/wlgrid
            '';

            meta = {
              description = "Wayland grid launcher";
              platforms = pkgs.lib.platforms.linux;
            };
          };
        });

      devShells = forAllSystems (system:
        let
          pkgs = import nixpkgs { inherit system; };
        in {
          default = pkgs.mkShell {
            packages = with pkgs; [
              rustc
              cargo
              pkg-config
              mold
              clang
              wayland
              libxkbcommon
              mesa
              libglvnd
            ];
            # Prepend the system's GL driver path so glvnd finds the running
            # vendor ICD (mesa), and add libglvnd so the dlopen'd libEGL.so.1 /
            # libGLESv2.so.2 dispatchers resolve at runtime under `cargo run`.
            LD_LIBRARY_PATH = "/run/opengl-driver/lib:" + pkgs.lib.makeLibraryPath [
              pkgs.wayland
              pkgs.libxkbcommon
              pkgs.mesa
              pkgs.libglvnd
            ];
          };
        });
    };
}
