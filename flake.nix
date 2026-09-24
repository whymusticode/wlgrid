{
  description = "wlgrid - Wayland grid launcher";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f (import nixpkgs { inherit system; }));

      # Link inputs shared by the package and the dev shell. The binary is
      # fully static (glibc included; rendering is CPU/wl_shm and the Wayland
      # protocol is pure Rust, so nothing is dlopen'd), so it can be copied
      # to any x86_64 Linux machine and run without /nix/store paths.
      depsFor = pkgs: {
        libs = [
          (pkgs.libxkbcommon.overrideAttrs (old: {
            mesonFlags = (old.mesonFlags or [ ]) ++ [ "-Ddefault_library=static" ];
            doCheck = false;
          }))
        ];
        env = {
          # Replaces .cargo/config.toml's rustflags, so repeat the mold flag.
          # Static glibc is only on the target's search path: host build
          # scripts still link dynamically.
          RUSTFLAGS = "-C link-arg=-fuse-ld=mold -C target-feature=+crt-static -L native=${pkgs.glibc.static}/lib";
          # An explicit target keeps RUSTFLAGS off build scripts and proc
          # macros (which can't be static). Output: target/<triple>/release.
          CARGO_BUILD_TARGET = pkgs.stdenv.hostPlatform.rust.rustcTarget;
        };
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
