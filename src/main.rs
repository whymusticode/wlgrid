// Minimal Hyprland/Wayland render test: winit + softbuffer (no wgpu).
use std::num::NonZeroU32;
use winit::event::{Event, WindowEvent};
use winit::event_loop::EventLoop;
use winit::window::WindowAttributes;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::builder().build()?;
    let context = softbuffer::Context::new(event_loop.owned_display_handle())?;
    let mut surface = None::<softbuffer::Surface<_, _>>;

    #[allow(deprecated)]
    event_loop.run(move |ev, elwt| {
        if surface.is_none() {
            let w = elwt
                .create_window(
                    WindowAttributes::default()
                        .with_title("wlgrid")
                        .with_inner_size(winit::dpi::LogicalSize::new(400.0, 300.0)),
                )
                .unwrap();
            let (width, height) = (w.inner_size().width, w.inner_size().height);
            let mut s = softbuffer::Surface::new(&context, w).unwrap();
            if let (Some(w), Some(h)) = (NonZeroU32::new(width), NonZeroU32::new(height)) {
                let _ = s.resize(w, h);
            }
            s.window().request_redraw();
            surface = Some(s);
        }

        let s = surface.as_mut().unwrap();
        if let Event::WindowEvent { window_id, event } = ev {
            if window_id != s.window().id() {
                return;
            }
            match event {
                WindowEvent::Resized(sz) => {
                    if let (Some(w), Some(h)) =
                        (NonZeroU32::new(sz.width), NonZeroU32::new(sz.height))
                    {
                        let _ = s.resize(w, h);
                    }
                }
                WindowEvent::RedrawRequested => {
                    if let Ok(mut buf) = s.buffer_mut() {
                        let w = buf.width().get() as usize;
                        for (i, p) in buf.iter_mut().enumerate() {
                            let x = (i % w) as u8;
                            let y = (i / w) as u8;
                            *p = 0u32 << 24 | (x as u32) << 16 | (y as u32) << 8 | 180u32;
                        }
                        let _ = buf.present();
                    }
                }
                WindowEvent::CloseRequested => elwt.exit(),
                _ => {}
            }
        }
    })?;
    Ok(())
}
