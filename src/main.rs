use std::{num::NonZeroU32, sync::Arc};
use softbuffer::{Context, Surface};
use winit::{application::ApplicationHandler, event::WindowEvent, event_loop::{ActiveEventLoop, EventLoop}, raw_window_handle::HasDisplayHandle, window::Window};

struct App { c: Option<Context<winit::raw_window_handle::DisplayHandle<'static>>>, w: Option<Arc<Window>>, s: Option<Surface<winit::raw_window_handle::DisplayHandle<'static>, Arc<Window>>> }
impl ApplicationHandler for App {
    fn resumed(&mut self, e: &ActiveEventLoop) { let w = Arc::new(e.create_window(Window::default_attributes().with_title("wlgrid")).unwrap()); let c = Context::new(unsafe { std::mem::transmute(e.display_handle().unwrap()) }).unwrap(); let s = Surface::new(&c, w.clone()).unwrap(); self.c = Some(c); self.s = Some(s); self.w = Some(w); self.w.as_ref().unwrap().request_redraw(); }
    fn window_event(&mut self, e: &ActiveEventLoop, _: winit::window::WindowId, ev: WindowEvent) { match ev { WindowEvent::CloseRequested => e.exit(), WindowEvent::Resized(z) => if let (Some(w), Some(h), Some(s)) = (NonZeroU32::new(z.width), NonZeroU32::new(z.height), self.s.as_mut()) { s.resize(w, h).unwrap(); self.w.as_ref().unwrap().request_redraw(); }, WindowEvent::RedrawRequested => if let Some(s) = self.s.as_mut() { let mut b = s.buffer_mut().unwrap(); for (i, p) in b.iter_mut().enumerate() { let x = i as u32 % b.width(); let y = i as u32 / b.width(); *p = (x ^ y) | 0x00ff00; } self.w.as_ref().unwrap().pre_present_notify(); b.present().unwrap(); }, _ => {} } }
}

fn main() { let mut a = App { c: None, w: None, s: None }; EventLoop::new().unwrap().run_app(&mut a).unwrap(); }
