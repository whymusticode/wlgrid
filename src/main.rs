use std::{env, num::NonZeroU32};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ── 1. Environment snapshot ──
    eprintln!("=== wlgrid wayland render probe ===");
    for k in [
        "XDG_SESSION_TYPE", "XDG_CURRENT_DESKTOP", "WAYLAND_DISPLAY", "DISPLAY",
        "WINIT_UNIX_BACKEND", "HYPRLAND_INSTANCE_SIGNATURE", "XDG_RUNTIME_DIR",
        "GDK_BACKEND", "QT_QPA_PLATFORM", "SDL_VIDEODRIVER",
        "LIBGL_ALWAYS_SOFTWARE", "WLR_RENDERER", "WLR_BACKENDS",
        "MESA_LOADER_DRIVER_OVERRIDE", "GALLIUM_DRIVER",
        "VK_ICD_FILENAMES", "VK_DRIVER_FILES",
        "LIBVA_DRIVER_NAME", "DBUS_SESSION_BUS_ADDRESS",
    ] {
        eprintln!("  {k} = {}", env::var(k).unwrap_or_else(|_| "<unset>".into()));
    }

    // ── 2. Probe shared libraries ──
    eprintln!("\n=== shared library probes ===");
    for lib in [
        "libwayland-client.so.0", "libwayland-client.so",
        "libwayland-egl.so.1", "libwayland-cursor.so.0",
        "libEGL.so.1", "libEGL.so",
        "libGLESv2.so.2", "libGL.so.1", "libGL.so",
        "libvulkan.so.1", "libvulkan.so",
        "libX11.so.6", "libX11-xcb.so.1",
        "libxcb.so.1", "libxkbcommon.so.0",
        "libdrm.so.2", "libgbm.so.1",
    ] {
        let ok = unsafe { libloading::Library::new(lib).is_ok() };
        eprintln!("  {lib:36} {}", if ok { "OK" } else { "MISSING" });
    }

    // ── 3. Check XDG_RUNTIME_DIR/wayland socket ──
    eprintln!("\n=== wayland socket ===");
    if let (Ok(rd), Ok(wd)) = (env::var("XDG_RUNTIME_DIR"), env::var("WAYLAND_DISPLAY")) {
        let sock = std::path::PathBuf::from(&rd).join(&wd);
        eprintln!("  socket path: {}", sock.display());
        eprintln!("  exists: {}", sock.exists());
        if sock.exists() {
            if let Ok(m) = std::fs::metadata(&sock) {
                eprintln!("  file_type: {:?}  len: {}", m.file_type(), m.len());
            }
        }
    } else {
        eprintln!("  XDG_RUNTIME_DIR or WAYLAND_DISPLAY not set");
    }

    // ── 4. Wayland fallback logic ──
    let on_wayland = env::var("XDG_SESSION_TYPE").is_ok_and(|v| v.eq_ignore_ascii_case("wayland"))
        || env::var("WAYLAND_DISPLAY").is_ok();
    let wl_lib = unsafe {
        libloading::Library::new("libwayland-client.so.0").is_ok()
            || libloading::Library::new("libwayland-client.so").is_ok()
    };
    eprintln!("\n=== backend decision ===");
    eprintln!("  on_wayland={on_wayland}  wl_lib_loadable={wl_lib}");
    if on_wayland && !wl_lib {
        eprintln!("  -> forcing X11 backend (XWayland)");
        unsafe {
            env::set_var("WINIT_UNIX_BACKEND", "x11");
            env::set_var("XDG_SESSION_TYPE", "x11");
            env::remove_var("WAYLAND_DISPLAY");
        }
    }

    // ── 5. Try winit EventLoop ──
    eprintln!("\n=== creating EventLoop ===");
    let event_loop = match winit::event_loop::EventLoop::builder().build() {
        Ok(el) => { eprintln!("  EventLoop created OK"); el }
        Err(e) => { eprintln!("  EventLoop FAILED: {e}"); return Err(e.into()); }
    };

    // ── 6. Try softbuffer Context ──
    eprintln!("=== creating softbuffer::Context ===");
    let context = match softbuffer::Context::new(event_loop.owned_display_handle()) {
        Ok(c) => { eprintln!("  Context created OK"); c }
        Err(e) => { eprintln!("  Context FAILED: {e}"); return Err(e.into()); }
    };

    // ── 7. Run event loop (with ControlFlow::Wait to avoid busy-spin) ──
    eprintln!("=== entering event loop ===\n");
    let mut surface = None::<softbuffer::Surface<_, _>>;

    #[allow(deprecated)]
    event_loop.run(move |ev, elwt| {
        elwt.set_control_flow(winit::event_loop::ControlFlow::Wait);

        if surface.is_none() {
            eprintln!("  creating window...");
            let w = elwt.create_window(
                winit::window::WindowAttributes::default()
                    .with_title("wlgrid probe")
                    .with_inner_size(winit::dpi::LogicalSize::new(400.0, 300.0)),
            ).unwrap();
            let sz = w.inner_size();
            eprintln!("  window created {}x{}", sz.width, sz.height);
            let mut s = softbuffer::Surface::new(&context, w).unwrap();
            eprintln!("  surface created OK");
            if let (Some(w), Some(h)) = (NonZeroU32::new(sz.width), NonZeroU32::new(sz.height)) {
                let _ = s.resize(w, h);
            }
            s.window().request_redraw();
            surface = Some(s);
        }

        let s = surface.as_mut().unwrap();
        if let winit::event::Event::WindowEvent { window_id, event } = ev {
            if window_id != s.window().id() { return; }
            match event {
                winit::event::WindowEvent::Resized(sz) => {
                    if let (Some(w), Some(h)) = (NonZeroU32::new(sz.width), NonZeroU32::new(sz.height)) {
                        let _ = s.resize(w, h);
                    }
                }
                winit::event::WindowEvent::RedrawRequested => {
                    if let Ok(mut buf) = s.buffer_mut() {
                        let w = buf.width().get() as usize;
                        for (i, p) in buf.iter_mut().enumerate() {
                            let (x, y) = ((i % w) as u32, (i / w) as u32);
                            *p = (x % 256) << 16 | (y % 256) << 8 | 180;
                        }
                        if let Err(e) = buf.present() {
                            eprintln!("  present error: {e}");
                        } else {
                            eprintln!("  frame presented OK");
                        }
                    }
                }
                winit::event::WindowEvent::CloseRequested => elwt.exit(),
                _ => {}
            }
        }
    })?;
    Ok(())
}
