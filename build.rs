// Nix builds set WLGRID_RPATH (see flake.nix) so the binary finds
// libwayland-client and libglvnd's libEGL.so.1 without LD_LIBRARY_PATH.
// The executable's RUNPATH also covers our dlopen of libEGL.so.1.
fn main() {
    println!("cargo:rerun-if-env-changed=WLGRID_RPATH");
    if let Ok(rpath) = std::env::var("WLGRID_RPATH") {
        println!("cargo:rustc-link-arg-bins=-Wl,-rpath,{rpath}");
    }
}
