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
            libx11
            libxcursor
            libxi
            libxrandr
            mesa
          ];
        in {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "wlgrid-layer";
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
              patchelf --set-rpath "${pkgs.lib.makeLibraryPath runtimeLibs}" $out/bin/wlgrid-layer
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
              libx11
              libxcursor
              libxi
              libxrandr
              mesa
            ];
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [
              pkgs.wayland
              pkgs.libxkbcommon
              pkgs.libx11
              pkgs.libxcursor
              pkgs.libxi
              pkgs.libxrandr
              pkgs.mesa
            ];
          };
        });
    };
}
