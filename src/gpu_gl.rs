use glow::HasContext;
use khronos_egl as egl;
use std::collections::HashMap;
use wayland_client::backend::ObjectId;
use wayland_egl::WlEglSurface;

/// A rounded rectangle with an optional border, drawn via an SDF shader.
/// `radius` and `border_w` are in pixels; pass `border = [0.0; 4]` for no border
/// and `fill = [.., 0.0]` for an outline-only rect (e.g. a drop target).
#[derive(Clone, Copy)]
pub struct GlRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub radius: f32,
    pub fill: [f32; 4],
    pub border: [f32; 4],
    pub border_w: f32,
}

/// A textured quad. `tint` multiplies the sampled texel — use `[1.0; 4]` for
/// icons, or a colour for text (whose texture is a white coverage mask). `key`
/// caches the uploaded texture across frames; `key == 0` is ephemeral: the
/// texture is uploaded, drawn, and discarded (used for transient text).
pub struct GlSprite<'a> {
    pub key: u64,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub pixels: &'a [u8],
    pub src_w: i32,
    pub src_h: i32,
    pub tint: [f32; 4],
}

/// One drawing command. Commands execute in submission order so UI layers
/// compose correctly (e.g. the picker draws over the grid).
pub enum GlCmd<'a> {
    Rect(GlRect),
    Sprite(GlSprite<'a>),
}

struct TextureEntry {
    tex: glow::NativeTexture,
    w: i32,
    h: i32,
}

pub struct GlRenderer {
    egl: egl::DynamicInstance<egl::EGL1_4>,
    display: egl::Display,
    context: egl::Context,
    surface: egl::Surface,
    egl_window: WlEglSurface,
    gl: glow::Context,
    tex_program: glow::NativeProgram,
    rect_program: glow::NativeProgram,
    vao: glow::NativeVertexArray,
    vbo: glow::NativeBuffer,
    width: i32,
    height: i32,
    texture_cache: HashMap<u64, TextureEntry>,
}

impl GlRenderer {
    pub fn new(
        display_id: ObjectId,
        surface_id: ObjectId,
        width: i32,
        height: i32,
    ) -> Result<Self, String> {
        // Prefer the versioned runtime SONAME (`libEGL.so.1`); the unversioned
        // `libEGL.so` only ships with the `-dev` package, so the default
        // `load_required()` fails on systems that have just the runtime.
        let egl = unsafe { egl::DynamicInstance::<egl::EGL1_4>::load_required_from_filename("libEGL.so.1") }
            .or_else(|_| unsafe { egl::DynamicInstance::<egl::EGL1_4>::load_required_from_filename("libEGL.so") })
            .map_err(|e| format!("egl load: {e}"))?;
        let display_ptr = display_id.as_ptr() as *mut core::ffi::c_void;
        let display = unsafe { egl.get_display(display_ptr) }.ok_or("egl get_display failed")?;
        egl.initialize(display).map_err(|e| format!("egl initialize: {e:?}"))?;
        egl.bind_api(egl::OPENGL_ES_API).map_err(|e| format!("egl bind_api: {e:?}"))?;

        let config_attribs = [
            egl::RED_SIZE, 8,
            egl::GREEN_SIZE, 8,
            egl::BLUE_SIZE, 8,
            egl::ALPHA_SIZE, 8,
            egl::SURFACE_TYPE, egl::WINDOW_BIT,
            egl::RENDERABLE_TYPE, egl::OPENGL_ES2_BIT,
            egl::NONE,
        ];
        let config = egl
            .choose_first_config(display, &config_attribs)
            .map_err(|e| format!("egl choose config: {e:?}"))?
            .ok_or("no egl config")?;
        let context_attribs = [egl::CONTEXT_CLIENT_VERSION, 2, egl::NONE];
        let context = egl
            .create_context(display, config, None, &context_attribs)
            .map_err(|e| format!("egl create_context: {e:?}"))?;

        let egl_window =
            WlEglSurface::new(surface_id, width.max(1), height.max(1)).map_err(|e| format!("wl_egl_window: {e}"))?;
        let surface = unsafe {
            egl.create_window_surface(display, config, egl_window.ptr() as *mut _, None)
        }
        .map_err(|e| format!("egl create_window_surface: {e:?}"))?;

        egl.make_current(display, Some(surface), Some(surface), Some(context))
            .map_err(|e| format!("egl make_current: {e:?}"))?;

        let gl = unsafe {
            glow::Context::from_loader_function(|s| {
                egl.get_proc_address(s)
                    .map(|f| f as *const _)
                    .unwrap_or(core::ptr::null())
            })
        };

        unsafe {
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::CULL_FACE);
            gl.enable(glow::BLEND);
            // Separate alpha blending: colour composites straight-over, but the
            // destination ALPHA accumulates with ONE (not SRC_ALPHA) so drawing
            // a translucent layer over an opaque one keeps alpha at 1.0.
            // Otherwise a 0.075-alpha tile over the opaque panel would drop the
            // panel's alpha to ~0.93 and let the desktop bleed through.
            gl.blend_func_separate(
                glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA,
                glow::ONE, glow::ONE_MINUS_SRC_ALPHA,
            );
            gl.viewport(0, 0, width, height);
        }

        let tex_program = make_program(&gl, VS_SRC, TEX_FS_SRC)?;
        let rect_program = make_program(&gl, VS_SRC, RECT_FS_SRC)?;
        let vao = unsafe { gl.create_vertex_array().map_err(|e| format!("create vao: {e}"))? };
        let vbo = unsafe { gl.create_buffer().map_err(|e| format!("create vbo: {e}"))? };
        unsafe {
            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_size(glow::ARRAY_BUFFER, 6 * 4 * std::mem::size_of::<f32>() as i32, glow::DYNAMIC_DRAW);
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 4 * 4, 0);
            gl.enable_vertex_attrib_array(1);
            gl.vertex_attrib_pointer_f32(1, 2, glow::FLOAT, false, 4 * 4, 2 * 4);
            gl.bind_vertex_array(None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
        }

        Ok(Self {
            egl,
            display,
            context,
            surface,
            egl_window,
            gl,
            tex_program,
            rect_program,
            vao,
            vbo,
            width,
            height,
            texture_cache: HashMap::new(),
        })
    }

    pub fn resize(&mut self, width: i32, height: i32) {
        self.width = width.max(1);
        self.height = height.max(1);
        self.egl_window.resize(self.width, self.height, 0, 0);
        unsafe {
            self.gl.viewport(0, 0, self.width, self.height);
        }
    }

    /// Clear to a dim black veil then execute the command list in order.
    pub fn render(&mut self, dim: f32, cmds: &[GlCmd<'_>]) -> Result<(), String> {
        unsafe {
            self.gl.clear_color(0.0, 0.0, 0.0, dim.clamp(0.0, 1.0));
            self.gl.clear(glow::COLOR_BUFFER_BIT);
            self.gl.bind_vertex_array(Some(self.vao));
        }
        for cmd in cmds {
            match cmd {
                GlCmd::Rect(r) => self.draw_rect(*r),
                GlCmd::Sprite(s) => self.draw_sprite(s)?,
            }
        }
        unsafe {
            self.gl.bind_vertex_array(None);
        }
        self.egl
            .swap_buffers(self.display, self.surface)
            .map_err(|e| format!("egl swap_buffers: {e:?}"))
    }

    fn draw_rect(&self, r: GlRect) {
        if r.w <= 0 || r.h <= 0 {
            return;
        }
        let half = [r.w as f32 / 2.0, r.h as f32 / 2.0];
        let radius = r.radius.clamp(0.0, half[0].min(half[1]));
        unsafe {
            let p = self.rect_program;
            self.gl.use_program(Some(p));
            set_uniform_2f(&self.gl, p, "u_screen", self.width as f32, self.height as f32);
            set_uniform_2f(&self.gl, p, "u_half", half[0], half[1]);
            set_uniform_1f(&self.gl, p, "u_radius", radius);
            set_uniform_1f(&self.gl, p, "u_border_w", r.border_w.max(0.0));
            set_uniform_4f(&self.gl, p, "u_fill", r.fill);
            set_uniform_4f(&self.gl, p, "u_border", r.border);
        }
        self.draw_quad(r.x as f32, r.y as f32, r.w as f32, r.h as f32);
    }

    fn draw_sprite(&mut self, s: &GlSprite<'_>) -> Result<(), String> {
        if s.w <= 0 || s.h <= 0 || s.src_w <= 0 || s.src_h <= 0 {
            return Ok(());
        }
        let (tex, ephemeral) = if s.key == 0 {
            (self.upload_texture(s.pixels, s.src_w, s.src_h)?, true)
        } else {
            (self.ensure_texture(s.key, s.pixels, s.src_w, s.src_h)?, false)
        };
        unsafe {
            let p = self.tex_program;
            self.gl.use_program(Some(p));
            set_uniform_2f(&self.gl, p, "u_screen", self.width as f32, self.height as f32);
            set_uniform_1i(&self.gl, p, "u_tex", 0);
            set_uniform_4f(&self.gl, p, "u_color", s.tint);
            self.gl.active_texture(glow::TEXTURE0);
            self.gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        }
        self.draw_quad(s.x as f32, s.y as f32, s.w as f32, s.h as f32);
        if ephemeral {
            unsafe { self.gl.delete_texture(tex); }
        }
        Ok(())
    }

    fn upload_texture(&self, pixels: &[u8], w: i32, h: i32) -> Result<glow::NativeTexture, String> {
        unsafe {
            let tex = self.gl.create_texture().map_err(|e| format!("create texture: {e}"))?;
            self.gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            self.gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
            self.gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
            self.gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
            self.gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
            self.gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA as i32,
                w,
                h,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(pixels)),
            );
            Ok(tex)
        }
    }

    fn ensure_texture(&mut self, key: u64, pixels: &[u8], w: i32, h: i32) -> Result<glow::NativeTexture, String> {
        if let Some(t) = self.texture_cache.get(&key) {
            if t.w == w && t.h == h {
                return Ok(t.tex);
            }
        }
        let tex = self.upload_texture(pixels, w, h)?;
        if let Some(old) = self.texture_cache.insert(key, TextureEntry { tex, w, h }) {
            unsafe { self.gl.delete_texture(old.tex); }
        }
        Ok(tex)
    }

    fn draw_quad(&self, x: f32, y: f32, w: f32, h: f32) {
        let x0 = x;
        let y0 = y;
        let x1 = x + w;
        let y1 = y + h;
        let verts: [f32; 24] = [
            x0, y0, 0.0, 0.0,
            x1, y0, 1.0, 0.0,
            x1, y1, 1.0, 1.0,
            x0, y0, 0.0, 0.0,
            x1, y1, 1.0, 1.0,
            x0, y1, 0.0, 1.0,
        ];
        unsafe {
            self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            self.gl.buffer_sub_data_u8_slice(glow::ARRAY_BUFFER, 0, bytemuck::cast_slice(&verts));
            self.gl.draw_arrays(glow::TRIANGLES, 0, 6);
        }
    }
}

impl Drop for GlRenderer {
    fn drop(&mut self) {
        unsafe {
            for (_, t) in self.texture_cache.drain() {
                self.gl.delete_texture(t.tex);
            }
            self.gl.delete_buffer(self.vbo);
            self.gl.delete_vertex_array(self.vao);
            self.gl.delete_program(self.tex_program);
            self.gl.delete_program(self.rect_program);
        }
        let _ = self
            .egl
            .make_current(self.display, None, None, None);
        let _ = self.egl.destroy_surface(self.display, self.surface);
        let _ = self.egl.destroy_context(self.display, self.context);
        let _ = self.egl.terminate(self.display);
    }
}

fn set_uniform_2f(gl: &glow::Context, p: glow::NativeProgram, name: &str, a: f32, b: f32) {
    unsafe {
        if let Some(loc) = gl.get_uniform_location(p, name) {
            gl.uniform_2_f32(Some(&loc), a, b);
        }
    }
}
fn set_uniform_1f(gl: &glow::Context, p: glow::NativeProgram, name: &str, v: f32) {
    unsafe {
        if let Some(loc) = gl.get_uniform_location(p, name) {
            gl.uniform_1_f32(Some(&loc), v);
        }
    }
}
fn set_uniform_1i(gl: &glow::Context, p: glow::NativeProgram, name: &str, v: i32) {
    unsafe {
        if let Some(loc) = gl.get_uniform_location(p, name) {
            gl.uniform_1_i32(Some(&loc), v);
        }
    }
}
fn set_uniform_4f(gl: &glow::Context, p: glow::NativeProgram, name: &str, v: [f32; 4]) {
    unsafe {
        if let Some(loc) = gl.get_uniform_location(p, name) {
            gl.uniform_4_f32(Some(&loc), v[0], v[1], v[2], v[3]);
        }
    }
}

const VS_SRC: &str = r#"#version 100
attribute vec2 a_pos;
attribute vec2 a_uv;
uniform vec2 u_screen;
varying vec2 v_uv;
void main() {
    vec2 ndc = vec2((a_pos.x / u_screen.x) * 2.0 - 1.0, 1.0 - (a_pos.y / u_screen.y) * 2.0);
    gl_Position = vec4(ndc, 0.0, 1.0);
    v_uv = a_uv;
}"#;

const TEX_FS_SRC: &str = r#"#version 100
precision mediump float;
varying vec2 v_uv;
uniform sampler2D u_tex;
uniform vec4 u_color;
void main() {
    gl_FragColor = texture2D(u_tex, v_uv) * u_color;
}"#;

// Rounded-rect SDF with optional border. v_uv spans 0..1 across the rect, so
// `p` is the pixel offset from the rect centre. `dist` is the signed distance
// to the rounded edge (negative inside); a 1px coverage band gives cheap AA.
const RECT_FS_SRC: &str = r#"#version 100
precision mediump float;
varying vec2 v_uv;
uniform vec2 u_half;
uniform float u_radius;
uniform float u_border_w;
uniform vec4 u_fill;
uniform vec4 u_border;
void main() {
    vec2 p = (v_uv - 0.5) * 2.0 * u_half;
    vec2 d2 = abs(p) - (u_half - vec2(u_radius));
    float dist = length(max(d2, 0.0)) + min(max(d2.x, d2.y), 0.0) - u_radius;
    float outer = clamp(0.5 - dist, 0.0, 1.0);
    float inner = clamp(0.5 - (dist + u_border_w), 0.0, 1.0);
    vec4 col = mix(u_border, u_fill, inner);
    float a = col.a * outer;
    if (a <= 0.0) discard;
    gl_FragColor = vec4(col.rgb, a);
}"#;

fn make_program(gl: &glow::Context, vs_src: &str, fs_src: &str) -> Result<glow::NativeProgram, String> {
    unsafe {
        let program = gl.create_program().map_err(|e| format!("create program: {e}"))?;
        let vs = gl.create_shader(glow::VERTEX_SHADER).map_err(|e| format!("create vs: {e}"))?;
        gl.shader_source(vs, vs_src);
        gl.compile_shader(vs);
        if !gl.get_shader_compile_status(vs) {
            return Err(format!("vs compile: {}", gl.get_shader_info_log(vs)));
        }
        let fs = gl.create_shader(glow::FRAGMENT_SHADER).map_err(|e| format!("create fs: {e}"))?;
        gl.shader_source(fs, fs_src);
        gl.compile_shader(fs);
        if !gl.get_shader_compile_status(fs) {
            return Err(format!("fs compile: {}", gl.get_shader_info_log(fs)));
        }
        gl.attach_shader(program, vs);
        gl.attach_shader(program, fs);
        gl.bind_attrib_location(program, 0, "a_pos");
        gl.bind_attrib_location(program, 1, "a_uv");
        gl.link_program(program);
        gl.delete_shader(vs);
        gl.delete_shader(fs);
        if !gl.get_program_link_status(program) {
            return Err(format!("program link: {}", gl.get_program_info_log(program)));
        }
        Ok(program)
    }
}
