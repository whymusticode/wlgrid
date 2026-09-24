{
  description = "wlgrid - Wayland grid launcher";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f (import nixpkgs { inherit system; }));

      # Link inputs shared by the package and the dev shell. xkbcommon is
      # linked statically. libwayland must stay shared: Mesa's EGL identifies
      # and drives our wl_display through the same libwayland instance, and a
      # second (static) copy breaks eglGetDisplay. It and libglvnd's
      # libEGL.so.1 (which dispatches to the GPU vendor's driver) are found
      # via a RUNPATH baked in by build.rs, so no LD_LIBRARY_PATH is needed.
      depsFor = pkgs:
        let
          libxkbcommon = pkgs.libxkbcommon.overrideAttrs (old: {
            mesonFlags = (old.mesonFlags or [ ]) ++ [ "-Ddefault_library=static" ];
            doCheck = false;
          });
        in {
          libs = [ libxkbcommon pkgs.wayland ];
          env.WLGRID_RPATH = pkgs.lib.makeLibraryPath [ pkgs.wayland pkgs.libglvnd ];
        };
    in {
      packages = forAllSystems (pkgs:
        let deps = depsFor pkgs; in {
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

            buildInputs = deps.libs;
            env = deps.env;
            # Keep libglvnd in the RUNPATH: it's dlopen'd, not DT_NEEDED, so
            # the default RUNPATH shrinking would drop it.
            dontPatchELF = true;

            meta = {
              description = "Wayland grid launcher";
              platforms = pkgs.lib.platforms.linux;
            };
          };
        });

      devShells = forAllSystems (pkgs:
        let deps = depsFor pkgs; in {
          default = pkgs.mkShell {
            packages = with pkgs; [
              rustc
              cargo
              pkg-config
              mold
              clang
            ] ++ deps.libs;
            env = deps.env;
          };
        });
    };
}
