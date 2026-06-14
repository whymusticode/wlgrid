use std::env;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;
use std::io::Write;
use std::thread;
use serde::{Deserialize, Serialize};
use ab_glyph::{Font, FontVec, ScaleFont, point};
mod gpu_gl;

// Pure-logic helpers shared with benches
use wlgrid::{
    dlog,
    Icon, Fonts, is_nerd_symbol,
    compute_checksum, load_cache, save_cache,
    load_desktop_entries, make_placeholder_icon,
};

// ── config ──

#[derive(Deserialize, Default)]
struct Config {
    #[serde(default)]
    width: Option<usize>,
    #[serde(default)]
    height: Option<usize>,
    #[serde(default)]
    icon_size: Option<f32>,
    #[serde(default)]
    bottom_bar: Option<BottomBar>,
    #[serde(default)]
    search_engines: Option<String>,
    #[serde(default)]
    search: Option<String>,
    #[serde(default)]
    start_col: Option<usize>,
    #[serde(default)]
    start_row: Option<usize>,
    #[serde(default)]
    tile_color: Option<String>,
    #[serde(default)]
    dim: Option<f32>,
    #[serde(default)]
    corner_radius: Option<u32>,
    #[serde(default)]
    accent_hue_delta: Option<f32>,
    #[serde(default)]
    accent_amount: Option<f32>,
    #[serde(default)]
    panel_color: Option<String>,
    #[serde(default)]
    panel_alpha: Option<f32>,
    #[serde(default)]
    tile_alpha: Option<f32>,
    #[serde(default)]
    border_color: Option<String>,
    #[serde(default)]
    border_alpha: Option<f32>,
    #[serde(default)]
    show_tile_outlines: Option<bool>,
}

/// Parse "#RRGGBB" or "RRGGBB" into [R, G, B]. Returns None on bad input.
fn parse_hex_color(s: &str) -> Option<[u8; 3]> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 { return None; }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some([r, g, b])
}

#[derive(Clone, Copy)]
struct Theme {
    dim: f32,
    radius: f32,
    accent_hue_delta: f32,
    accent_amount: f32,
    panel: [u8; 3],
    panel_a: f32,
    tile: [u8; 3],
    tile_a: f32,
    border: [u8; 3],
    border_a: f32,
    show_tile_outlines: bool,
}

impl Theme {
    fn from_config(c: &Config) -> Self {
        let hex = |s: &Option<String>, d: [u8; 3]| s.as_deref().and_then(parse_hex_color).unwrap_or(d);
        Theme {
            dim: c.dim.unwrap_or(0.18).clamp(0.0, 1.0),
            radius: c.corner_radius.unwrap_or(8) as f32,
            accent_hue_delta: c.accent_hue_delta.unwrap_or(18.0),
            accent_amount: c.accent_amount.unwrap_or(0.45).clamp(0.0, 1.0),
            panel: hex(&c.panel_color, [0x10, 0x13, 0x1c]),
            panel_a: c.panel_alpha.unwrap_or(0.50),
            tile: hex(&c.tile_color, [0xff, 0xff, 0xff]),
            tile_a: c.tile_alpha.unwrap_or(0.075),
            border: hex(&c.border_color, [0xff, 0xff, 0xff]),
            border_a: c.border_alpha.unwrap_or(0.12),
            show_tile_outlines: c.show_tile_outlines.unwrap_or(true),
        }
    }
}

fn rgb_to_hsl(c: [u8; 3]) -> (f32, f32, f32) {
    let (r, g, b) = (c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d < 1e-6 {
        return (0.0, 0.0, l);
    }
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [u8; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let f = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    [f(r), f(g), f(b)]
}

fn accent_delta(base: [u8; 3], base_a: f32, hue_delta: f32, amount: f32) -> ([u8; 3], f32) {
    let (h, s, l) = rgb_to_hsl(base);
    let rgb = hsl_to_rgb(
        h + hue_delta,
        (s + amount * 0.25).clamp(0.0, 1.0),
        (l + amount * 0.35).clamp(0.0, 1.0),
    );
    (rgb, (base_a + amount * 0.30).clamp(0.0, 1.0))
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum SearchType {
    Folders,  // zoxide directories
    Desktop,  // desktop entries
}

fn parse_search_config(config_str: &str) -> Vec<SearchType> {
    // Parse format like "[folders,desktop]" or "folders,desktop"
    let cleaned = config_str.trim().trim_start_matches('[').trim_end_matches(']');
    cleaned
        .split(',')
        .filter_map(|s| {
            match s.trim().to_lowercase().as_str() {
                "folders" => Some(SearchType::Folders),
                "desktop" => Some(SearchType::Desktop),
                _ => None,
            }
        })
        .collect()
}

fn default_search_types() -> Vec<SearchType> {
    vec![SearchType::Folders, SearchType::Desktop]
}

#[derive(Clone)]
enum SearchMatch {
    None,
    Folder(String),
    Desktop(Vec<usize>),  // indices into icons vec
}

#[derive(Clone)]
struct SearchEngine {
    name: String,
    url_template: String, // use {} for query placeholder
}

fn default_search_engines() -> Vec<SearchEngine> {
    vec![
        SearchEngine { name: "Brave".into(), url_template: "https://search.brave.com/search?q={}".into() },
        SearchEngine { name: "Claude".into(), url_template: "https://claude.ai/new?q={}".into() },
        SearchEngine { name: "Wikipedia".into(), url_template: "https://en.wikipedia.org/wiki/Special:Search?search={}".into() },
        SearchEngine { name: "GitHub".into(), url_template: "https://github.com/search?q={}".into() },
        SearchEngine { name: "YouTube".into(), url_template: "https://www.youtube.com/results?search_query={}".into() },
    ]
}

fn parse_search_engines(config_str: &str) -> Vec<SearchEngine> {
    config_str
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() { return None; }
            let (name, url) = line.split_once('=')?;
            Some(SearchEngine {
                name: name.trim().to_string(),
                url_template: url.trim().to_string(),
            })
        })
        .collect()
}

#[derive(Deserialize, Default, Clone)]
struct BottomBar {
    #[serde(default)]
    font: Option<f32>,
    #[serde(default)]
    options: String,
}

#[derive(Clone)]
struct BarItem {
    name: String,
    exec: String,
}

fn parse_bottom_bar_options(options: &str) -> Vec<BarItem> {
    options
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() { return None; }
            let (name, exec) = line.split_once('=')?;
            Some(BarItem {
                name: name.trim().to_string(),
                exec: exec.trim().to_string(),
            })
        })
        .collect()
}

fn load_config() -> Config {
    if let Some(home) = env::var("HOME").ok() {
        let dir = format!("{home}/.config/wlgrid");
        let dst = format!("{dir}/config.toml");
        if !std::path::Path::new(&dst).exists() {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(&dst, include_str!("../config.toml.default"));
        }
    }

    let config_paths = [
        env::var("HOME").ok().map(|h| format!("{h}/.config/wlgrid/config.toml")),
        Some("/etc/wlgrid/config.toml".to_string()),
    ];

    for path in config_paths.into_iter().flatten() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            match toml::from_str(&content) {
                Ok(config) => {
                    dlog!("  loaded config from {}", path);
                    return config;
                }
                Err(e) => dlog!("  config parse error in {}: {}", path, e),
            }
        }
    }

    dlog!("  no config found, using defaults");
    Config::default()
}

// ── state persistence ──

#[derive(Serialize, Deserialize, Default)]
struct AppState {
    /// Maps tile index to desktop entry name (for matching on reload)
    tiles: Vec<Option<String>>,
}

fn state_path() -> Option<PathBuf> {
    env::var("HOME").ok().map(|h| PathBuf::from(format!("{h}/.config/wlgrid/state.json")))
}

fn load_state() -> AppState {
    if let Some(path) = state_path() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(state) = serde_json::from_str(&content) {
                dlog!("  loaded state from {}", path.display());
                return state;
            }
        }
    }
    dlog!("  no saved state, starting fresh");
    AppState::default()
}

fn save_state(tiles: &[Option<usize>], icons: &[Icon]) {
    let state = AppState {
        tiles: tiles.iter().map(|opt| {
            opt.and_then(|idx| icons.get(idx).map(|i| i.name.clone()))
        }).collect(),
    };

    if let Some(path) = state_path() {
        // Ensure directory exists
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::File::create(&path) {
            Ok(mut file) => {
                if let Ok(json) = serde_json::to_string_pretty(&state) {
                    let _ = file.write_all(json.as_bytes());
                    dlog!("  saved state to {}", path.display());
                }
            }
            Err(e) => dlog!("  failed to save state: {}", e),
        }
    }
}

// ── text rendering ──

struct FontsWithPaths {
    fonts: Fonts,
    text_path: PathBuf,
    symbols_path: Option<PathBuf>,
}

/// Load fonts from cached paths (fast path)
fn load_fonts_from_paths(text_path: &str, symbols_path: Option<&str>) -> Option<Fonts> {
    let text_bytes = std::fs::read(text_path).ok()?;
    let text_font = FontVec::try_from_vec(text_bytes).ok()?;
    dlog!("  cache: loaded text font from {}", text_path);

    let symbols_font = symbols_path.and_then(|p| {
        let bytes = std::fs::read(p).ok()?;
        let font = FontVec::try_from_vec(bytes).ok()?;
        dlog!("  cache: loaded symbols font from {}", p);
        Some(font)
    });

    Some(Fonts { text: text_font, symbols: symbols_font })
}

/// Search for fonts (slow path, used when cache is invalid)
fn load_fonts_with_search() -> Option<FontsWithPaths> {
    // Common font directories on Linux/NixOS
    let font_dirs = [
        "/run/current-system/sw/share/X11/fonts",
        "/run/current-system/sw/share/fonts",
        "/usr/share/fonts",
        "/usr/local/share/fonts",
    ];

    // Also check user font dirs
    let home = env::var("HOME").ok();
    let user_font_dirs: Vec<String> = home.iter().flat_map(|h| [
        format!("{h}/.local/share/fonts"),
        format!("{h}/.fonts"),
    ]).collect();

    let all_dirs: Vec<&str> = font_dirs.iter().copied()
        .chain(user_font_dirs.iter().map(|s| s.as_str()))
        .collect();

    // Collect all TTF files
    let mut all_fonts: Vec<PathBuf> = Vec::new();
    for dir in &all_dirs {
        for entry in walkdir::WalkDir::new(dir).into_iter().filter_map(Result::ok) {
            let path = entry.path();
            if path.extension().map_or(false, |e| e == "ttf" || e == "otf") {
                all_fonts.push(path.to_path_buf());
            }
        }
    }
    dlog!("  found {} font files", all_fonts.len());

    // Find text font (prefer DejaVu, Liberation, or any sans)
    let text_patterns = ["DejaVuSans", "LiberationSans", "NotoSans", "Ubuntu", "Roboto"];
    let mut text_font = None;
    let mut text_path = None;
    for pattern in text_patterns {
        for path in &all_fonts {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.contains(pattern) && !name.contains("Nerd") && !name.contains("Bold") && !name.contains("Italic") {
                if let Ok(bytes) = std::fs::read(path) {
                    if let Ok(font) = FontVec::try_from_vec(bytes) {
                        dlog!("  loaded text font: {}", path.display());
                        text_font = Some(font);
                        text_path = Some(path.clone());
                        break;
                    }
                }
            }
        }
        if text_font.is_some() { break; }
    }

    // Fallback: any regular-looking font
    if text_font.is_none() {
        for path in &all_fonts {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.contains("Nerd") && !name.contains("Symbol") && !name.contains("Bold") && !name.contains("Italic") {
                if let Ok(bytes) = std::fs::read(path) {
                    if let Ok(font) = FontVec::try_from_vec(bytes) {
                        dlog!("  loaded text font (fallback): {}", path.display());
                        text_font = Some(font);
                        text_path = Some(path.clone());
                        break;
                    }
                }
            }
        }
    }

    let text_font = text_font?;
    let text_path = text_path?;

    // Find nerd symbols font
    let mut symbols_font = None;
    let mut symbols_path = None;
    for path in &all_fonts {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.contains("NerdFont") && name.contains("Symbol") {
            if let Ok(bytes) = std::fs::read(path) {
                if let Ok(font) = FontVec::try_from_vec(bytes) {
                    dlog!("  loaded symbols font: {}", path.display());
                    symbols_font = Some(font);
                    symbols_path = Some(path.clone());
                    break;
                }
            }
        }
    }

    Some(FontsWithPaths {
        fonts: Fonts { text: text_font, symbols: symbols_font },
        text_path,
        symbols_path,
    })
}

/// Rasterise `text` into a tight RGBA coverage buffer (white pixels, alpha =
/// glyph coverage) for upload as a GL texture, tinted at draw time. The internal
/// baseline sits `ascent` px from the top (returned as the 4th element), so a
/// caller wanting a baseline at `baseline_y` draws the buffer at
/// `y = baseline_y - ascent`. Returns None for empty / zero-width text.
fn rasterize_text(fonts: &Fonts, text: &str, size: f32) -> Option<(Vec<u8>, u32, u32, i32)> {
    if size <= 0.0 {
        return None;
    }
    let w = text_width(fonts, text, size).ceil() as u32;
    if w == 0 {
        return None;
    }
    let base = fonts.text.as_scaled(size);
    let ascent = base.ascent();
    let h = (base.ascent() - base.descent()).ceil().max(1.0) as u32;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut pen_x = 0.0f32;
    for c in text.chars() {
        let font = if is_nerd_symbol(c) {
            fonts.symbols.as_ref().unwrap_or(&fonts.text)
        } else {
            &fonts.text
        };
        let sf = font.as_scaled(size);
        let gid = font.glyph_id(c);
        if let Some(o) = font.outline_glyph(gid.with_scale_and_position(size, point(pen_x, ascent))) {
            let b = o.px_bounds();
            o.draw(|gx, gy, cov| {
                let px = b.min.x as i32 + gx as i32;
                let py = b.min.y as i32 + gy as i32;
                if px < 0 || py < 0 || px >= w as i32 || py >= h as i32 {
                    return;
                }
                let alpha = (cov * 255.0) as u8;
                if alpha == 0 {
                    return;
                }
                let idx = ((py as u32 * w + px as u32) * 4) as usize;
                // Glyphs shouldn't overlap, but if they do keep the strongest
                // coverage rather than letting later glyphs erase earlier ones.
                if alpha > buf[idx + 3] {
                    buf[idx] = 0xFF;
                    buf[idx + 1] = 0xFF;
                    buf[idx + 2] = 0xFF;
                    buf[idx + 3] = alpha;
                }
            });
        }
        pen_x += sf.h_advance(gid);
    }
    Some((buf, w, h, ascent.round() as i32))
}

/// Pack an RGB colour + alpha into GL's straight-alpha `[f32; 4]`.
fn rgba3(c: [u8; 3], a: f32) -> [f32; 4] {
    [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, a.clamp(0.0, 1.0)]
}

/// Pack an RGBA8 colour into GL's straight-alpha `[f32; 4]`.
fn rgba4(c: [u8; 4]) -> [f32; 4] {
    [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, c[3] as f32 / 255.0]
}

fn text_width(fonts: &Fonts, text: &str, size: f32) -> f32 {
    text.chars().map(|c| {
        let font = if is_nerd_symbol(c) {
            fonts.symbols.as_ref().unwrap_or(&fonts.text)
        } else {
            &fonts.text
        };
        font.as_scaled(size).h_advance(font.glyph_id(c))
    }).sum()
}

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, delegate_shm, delegate_touch,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
        touch::TouchHandler,
        Capability, SeatHandler, SeatState,
    },
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler,
            LayerSurface, LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{Shm, ShmHandler},
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface, wl_touch},
    Connection, Proxy, QueueHandle,
};
use wayland_cursor::CursorTheme;

// ── desktop entry + icon loading ──

/// Load recent directories from zoxide
fn load_zoxide_dirs() -> Vec<String> {
    let output = Command::new("zoxide")
        .args(["query", "-l"])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let dirs: Vec<String> = String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(|s| s.to_string())
                .collect();
            dlog!("  zoxide: loaded {} directories", dirs.len());
            dirs
        }
        _ => {
            dlog!("  zoxide: failed to load (is zoxide installed?)");
            Vec::new()
        }
    }
}

/// Launch an application from its Exec string (without shell - secure)
/// Parse a shell-like command string, respecting quoted arguments.
fn parse_exec_args(exec: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut chars = exec.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '\'' if !in_double_quote => {
                in_single_quote = !in_single_quote;
            }
            '"' if !in_single_quote => {
                in_double_quote = !in_double_quote;
            }
            '\\' if in_double_quote || (!in_single_quote && !in_double_quote) => {
                // Handle escape sequences
                if let Some(&next) = chars.peek() {
                    chars.next();
                    current.push(next);
                }
            }
            ' ' | '\t' if !in_single_quote && !in_double_quote => {
                if !current.is_empty() {
                    // Skip desktop entry field codes (%f, %F, %u, %U, etc.)
                    if !current.starts_with('%') || current.len() != 2 {
                        args.push(current.clone());
                    }
                    current.clear();
                }
            }
            _ => {
                current.push(c);
            }
        }
    }

    // Don't forget the last argument
    if !current.is_empty() && (!current.starts_with('%') || current.len() != 2) {
        args.push(current);
    }

    args
}

fn launch_exec(exec: &str, name: &str) {
    dlog!("  launch: raw exec = '{}'", exec);

    let args = parse_exec_args(exec);

    if args.is_empty() {
        dlog!("  launch: empty command for {}", name);
        return;
    }

    let program = &args[0];
    let cmd_args = &args[1..];

    dlog!("  launch: {} -> '{}' {:?}", name, program, cmd_args);

    // Spawn detached process directly (no shell)
    match Command::new(program)
        .args(cmd_args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_) => dlog!("  launch: spawned successfully"),
        Err(e) => dlog!("  launch: failed: {}", e),
    }
}

/// Acquire a single-instance lock on `$XDG_RUNTIME_DIR/wlgrid.lock`.
/// Returns the held `File` on success (drop to release), or `None` if another
/// instance already holds the lock. The kernel releases flock automatically
/// when the process exits, so crashes don't leave stale locks.
fn acquire_instance_lock() -> Option<std::fs::File> {
    let runtime_dir = env::var("XDG_RUNTIME_DIR").ok()?;
    let path = format!("{runtime_dir}/wlgrid.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .ok()?;

    use std::os::unix::io::AsRawFd;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        return None;
    }
    Some(file)
}

fn main() {
    // Probe mode is a one-shot diagnostic that exits quickly; allow it to run
    // even if another instance holds the lock. Detected early so the lock
    // file handling stays simple.
    let probe_mode = env::args().any(|a| a == "--ci-probe");

    // Refuse to start a second instance. Silent exit — intentional for hotkey use.
    let _instance_lock = if probe_mode {
        None
    } else {
        match acquire_instance_lock() {
            Some(f) => Some(f),
            None => return,
        }
    };

    let startup_time = Instant::now();

    // ── env probe ──
    dlog!("=== wlgrid layer-shell probe ===");
    for k in [
        "XDG_SESSION_TYPE", "WAYLAND_DISPLAY", "XDG_RUNTIME_DIR",
        "HYPRLAND_INSTANCE_SIGNATURE",
    ] {
        dlog!("  {k} = {}", env::var(k).unwrap_or("<unset>".into()));
    }
    for lib in ["libwayland-client.so.0", "libwayland-egl.so.1", "libxkbcommon.so.0"] {
        let ok = unsafe { libloading::Library::new(lib).is_ok() };
        dlog!("  {lib:36} {}", if ok { "OK" } else { "MISSING" });
    }

    // ── connect to wayland ──
    let conn = Connection::connect_to_env().unwrap();
    let (globals, mut event_queue) = registry_queue_init(&conn).unwrap();
    let qh = event_queue.handle();
    dlog!("  wayland connection OK");

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor missing");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("layer shell missing");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm missing");
    dlog!("  compositor + layer_shell + shm bound");

    // Load config
    let config = load_config();

    // Grid configuration from config
    let grid_w: usize = config.width.unwrap_or(6);
    let grid_h: usize = config.height.unwrap_or(4);
    // Icon pixel size (for desktop entry loading + placeholder generation).
    // Config stores it as f32 for user convenience; we round to u32.
    let icon_size: u32 = config.icon_size
        .map(|s| s.round().max(1.0) as u32)
        .unwrap_or(wlgrid::DEFAULT_ICON_SIZE);
    let tile_size: u32 = ((icon_size as f32) * (64.0 / wlgrid::DEFAULT_ICON_SIZE as f32))
        .round()
        .max(icon_size as f32 + 8.0) as u32;
    let tile_gap: u32 = ((icon_size as f32) * (8.0 / wlgrid::DEFAULT_ICON_SIZE as f32))
        .round()
        .max(4.0) as u32;
    let num_tiles = grid_w * grid_h;

    // Load bottom bar items from config
    let dock: Vec<DockEntry> = config.bottom_bar
        .as_ref()
        .map(|bb| parse_bottom_bar_options(&bb.options))
        .unwrap_or_default()
        .into_iter()
        .map(|item| {
            dlog!("  bar item: {} -> {}", item.name, item.exec);
            DockEntry {
                name: item.name,
                exec: item.exec,
            }
        })
        .collect();
    dlog!("  loaded {} bottom bar items", dock.len());

    // Get dock font size from config
    let dock_font_size = config.bottom_bar.as_ref()
        .and_then(|bb| bb.font)
        .unwrap_or(16.0);
    dlog!("  dock font size: {}, icon size: {}", dock_font_size, icon_size);

    // Calculate surface size (grid + dock bar if dock has items)
    let surface_w = 16 + grid_w as u32 * tile_size + (grid_w - 1) as u32 * tile_gap;
    let grid_height = 16 + grid_h as u32 * tile_size + (grid_h - 1) as u32 * tile_gap;
    let surface_h = if dock.is_empty() {
        grid_height
    } else {
        grid_height + DOCK_HEIGHT
    };

    // ── create layer surface ──
    let surface = compositor.create_surface(&qh);
    let layer = layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("wlgrid"), None);
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    layer.set_exclusive_zone(-1); // don't push other surfaces
    layer.commit();
    dlog!("  layer surface (fullscreen), content {}x{}, waiting for configure...", surface_w, surface_h);

    // Load cursor theme lazily on first pointer enter
    let cursor_theme = None;
    let cursor_surface = compositor.create_surface(&qh);
    dlog!("  cursor theme deferred");

    // Try to load from cache first (fast path). Reject it if the cached
    // icon dimensions don't match the configured icon_size — the user may
    // have changed `icon_size` in config.toml since the cache was written.
    let cached = load_cache().filter(|cache| {
        cache.icons.iter().all(|i| i.width == icon_size && i.height == icon_size)
    });
    let mut cache_write_paths: Option<(Option<PathBuf>, Option<PathBuf>)> = None;
    let (icons, fonts) = if let Some(cache) = cached {
        // Load icons from cache
        let icons: Vec<Icon> = cache.icons.into_iter().map(|ci| Icon {
            name_lower: ci.name.to_lowercase(),
            name: ci.name,
            exec: ci.exec,
            pixels: ci.pixels,
            width: ci.width,
            height: ci.height,
        }).collect();

        // ab_glyph parses lazily (no up-front glyph outlining), so loading the
        // fonts here is a couple of ms — fine to do synchronously.
        let fonts = cache.text_font_path.as_ref().and_then(|tp| {
            load_fonts_from_paths(tp, cache.symbols_font_path.as_deref())
        });

        dlog!("  cache: loaded {} icons", icons.len());
        (icons, fonts)
    } else {
        // Cache miss - do full load (slow path)
        dlog!("  cache: miss, doing full load");

        // Load fonts first so entries without an icon can render their name.
        let (fonts, paths) = match load_fonts_with_search() {
            Some(fp) => (Some(fp.fonts), (Some(fp.text_path), fp.symbols_path)),
            None => (None, (None, None)),
        };
        cache_write_paths = Some(paths);

        // Load desktop entries
        let icons = load_desktop_entries(icon_size, fonts.as_ref());
        dlog!("  loaded {} desktop entries", icons.len());

        (icons, fonts)
    };

    dlog!("  icons + fonts ready at {:.2}ms", startup_time.elapsed().as_secs_f64() * 1000.0);

    let Some(fonts) = fonts else { eprintln!("wlgrid: no usable font found; install a sans + a Nerd Font"); return; };

    // Load saved state and restore tiles
    let saved_state = load_state();
    let tiles: Vec<Option<usize>> = (0..num_tiles)
        .map(|i| {
            saved_state.tiles.get(i).and_then(|opt_name| {
                opt_name.as_ref().and_then(|name| {
                    icons.iter().position(|icon| &icon.name == name)
                })
            })
        })
        .collect();
    let restored_count = tiles.iter().filter(|t| t.is_some()).count();
    dlog!("  restored {} tiles from saved state", restored_count);

    let mut app = App {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        shm,
        exit: false,
        first_configure: true,
        width: surface_w,
        height: surface_h,
        content_w: surface_w,
        content_h: surface_h,
        grid_offset: (0, 0),
        layer,
        keyboard: None,
        pointer: None,
        touch: None,
        grid_w,
        grid_h,
        tile_size,
        tile_gap,
        tiles,
        icons,
        icon_size,
        icons_checksum: compute_checksum(),
        pointer_pos: (0.0, 0.0),
        hovered_tile: Some({
            let col = config.start_col.unwrap_or(grid_w / 2).min(grid_w.saturating_sub(1));
            let row = config.start_row.unwrap_or(grid_h / 2).min(grid_h.saturating_sub(1));
            row * grid_w + col
        }),
        press_start: None,
        drag_from: None,
        drag_threshold: 5.0,
        dirty: true,
        frame_pending: false,
        first_frame_presented: false,
        icons_at_first_frame: 0,
        picker_target: None,
        picker_scroll: 0,
        picker_hovered: None,
        picker_search: String::new(),
        dock,
        hovered_dock: None,
        dock_font_size,
        fonts,
        search_query: String::new(),
        zoxide_dirs: std::sync::OnceLock::new(),
        search_engines: config.search_engines
            .as_ref()
            .map(|s| parse_search_engines(s))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(default_search_engines),
        hovered_search_engine: None,
        theme: Theme::from_config(&config),
        scale: 1,
        pad: 8,
        dock_h: DOCK_HEIGHT,
        base_tile_size: tile_size,
        base_tile_gap: tile_gap,
        base_pad: 8,
        base_dock_font: dock_font_size,
        gl_renderer: None,
        search_types: config.search
            .as_ref()
            .map(|s| parse_search_config(s))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(default_search_types),
        cursor_theme,
        cursor_surface,
        pointer_enter_serial: 0,
        startup_time,
        cache_write_paths,
        probe_draw_ms: Vec::new(),
    };

    // Probe mode: run a polling event loop, exit after quiescence with metrics.
    let exit_code = if probe_mode {
        run_probe_loop(&conn, &mut event_queue, &mut app, startup_time)
    } else {
        loop {
            event_queue.blocking_dispatch(&mut app).unwrap();
            if app.exit { break; }
        }
        0
    };

    // Save state before exiting
    save_state(&app.tiles, &app.icons);
    dlog!("  exiting");

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

/// Drive the event loop in probe mode: dispatch Wayland events via a
/// poll-with-timeout loop so we can detect when the app has settled into a
/// steady state (no pending draws, no pending frame callbacks, no inbound
/// events for a quiescence window). Prints timing metrics to stdout and
/// returns an exit code.
///
/// Used by CI (`wlgrid --ci-probe` under a headless compositor) to catch
/// regressions in the startup / first-frame pipeline that pure criterion
/// benches can't see — e.g. "first committed frame was empty because icons
/// hadn't loaded yet".
fn run_probe_loop(
    conn: &Connection,
    event_queue: &mut wayland_client::EventQueue<App>,
    app: &mut App,
    startup_time: Instant,
) -> i32 {
    use std::os::fd::AsRawFd;
    use std::time::Duration;

    const QUIESCE_MS: u64 = 50;
    const HARD_TIMEOUT: Duration = Duration::from_secs(10);

    let mut last_activity = Instant::now();
    let mut time_to_first_frame: Option<Duration> = None;

    loop {
        // Flush any pending requests to the compositor.
        if conn.flush().is_err() {
            eprintln!("probe: flush error");
            return 1;
        }

        // Dispatch any events already queued (non-blocking).
        let dispatched = event_queue.dispatch_pending(app).unwrap_or(0);

        if app.exit { break; }

        if time_to_first_frame.is_none() && app.first_frame_presented {
            time_to_first_frame = Some(startup_time.elapsed());
        }

        let busy = app.dirty || app.frame_pending || dispatched > 0;
        if busy {
            last_activity = Instant::now();
        }

        // Quiesced: the first frame has been presented and the app has been
        // idle for at least QUIESCE_MS.
        let idle_for = last_activity.elapsed();
        if !busy
            && time_to_first_frame.is_some()
            && idle_for.as_millis() as u64 >= QUIESCE_MS
        {
            break;
        }

        if startup_time.elapsed() > HARD_TIMEOUT {
            eprintln!("probe: hard timeout reached ({:?})", HARD_TIMEOUT);
            return 2;
        }

        // Prepare to read events from the Wayland fd, polling with a timeout
        // so the quiescence check runs periodically even when nothing arrives.
        let Some(guard) = conn.prepare_read() else {
            // Another read is already in progress — retry dispatch.
            continue;
        };

        let fd = guard.connection_fd();
        let remaining = Duration::from_millis(QUIESCE_MS).saturating_sub(idle_for);
        let timeout_ms = remaining.as_millis().min(50) as i32;

        let mut pfd = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let n = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };

        if n > 0 && (pfd.revents & libc::POLLIN) != 0 {
            // Data arrived — read it into the queue.
            if guard.read().is_err() {
                eprintln!("probe: read error");
                return 1;
            }
        } else {
            // Timeout (or signal) — drop the guard without reading.
            drop(guard);
        }
    }

    // Quiesced. Emit metrics.
    let tiles_filled = app.tiles.iter()
        .filter_map(|t| *t)
        .filter(|idx| app.icons.get(*idx).is_some())
        .count();
    let first_frame_ms = time_to_first_frame
        .map(|d| d.as_millis())
        .unwrap_or(0);

    // Count how many icons are the procedural "?" placeholder vs real.
    // We re-generate the placeholder and byte-compare against each icon's
    // pixels — `make_placeholder_icon` is deterministic, so any icon with
    // matching pixels came from the fallback path (icon file missing or
    // unreadable), not from a real .desktop icon.
    let (placeholder_pixels, _, _) = make_placeholder_icon(app.icon_size);
    let placeholder_icons = app.icons.iter()
        .filter(|i| i.pixels == placeholder_pixels)
        .count();
    let real_icons = app.icons.len() - placeholder_icons;
    let steady_ms = startup_time.elapsed().as_millis();
    let steady_minus_quiesce_ms = steady_ms.saturating_sub(QUIESCE_MS as u128);
    let draw_count = app.probe_draw_ms.len();
    let draw_avg_ms = if draw_count == 0 {
        0.0
    } else {
        app.probe_draw_ms.iter().copied().map(|v| v as f64).sum::<f64>() / draw_count as f64
    };
    let draw_p95_ms = if draw_count == 0 {
        0.0
    } else {
        let mut s = app.probe_draw_ms.clone();
        s.sort_by(|a, b| a.total_cmp(b));
        let idx = (((s.len() - 1) as f32) * 0.95).round() as usize;
        s[idx] as f64
    };

    println!("probe: time_to_first_frame_ms={}", first_frame_ms);
    println!("probe: time_to_steady_state_ms={}", steady_ms);
    println!("probe: time_to_steady_state_minus_quiesce_ms={}", steady_minus_quiesce_ms);
    println!("probe: quiesce_ms={}", QUIESCE_MS);
    println!("probe: icons_loaded={}", app.icons.len());
    println!("probe: real_icons_loaded={}", real_icons);
    println!("probe: placeholder_icons_loaded={}", placeholder_icons);
    println!("probe: icons_at_first_frame={}", app.icons_at_first_frame);
    println!("probe: tiles_filled={}/{}", tiles_filled, app.tiles.len());
    println!("probe: draw_count={}", draw_count);
    println!("probe: draw_avg_ms={:.2}", draw_avg_ms);
    println!("probe: draw_p95_ms={:.2}", draw_p95_ms);

    // Regression check: if the final icon count is > 0 but the first-frame
    // snapshot was 0, that means icons loaded *after* the first draw — the
    // exact ordering bug we care about catching. Fail loudly.
    if app.icons.len() > 0 && app.icons_at_first_frame == 0 {
        eprintln!(
            "probe: FAIL — first frame was drawn with 0 icons but {} icons \
             were loaded by steady state. Icon loading happened after the \
             first draw; this will show empty tiles to the user.",
            app.icons.len()
        );
        return 3;
    }

    0
}

struct DockEntry {
    name: String,
    exec: String,
}

const DOCK_HEIGHT: u32 = 64;

/// A single UI primitive collected while building a frame, in z-order. Text is
/// kept as an index into a side buffer of rasterised glyph masks (see `draw`),
/// since the masks must outlive the borrow used to build the GL command list.
enum DrawItem {
    Rect { x: i32, y: i32, w: i32, h: i32, radius: f32, fill: [f32; 4], border: [f32; 4], border_w: f32 },
    Icon { key: u64, idx: usize, x: i32, y: i32, w: i32, h: i32 },
    Text { buf: usize, x: i32, y: i32, tint: [f32; 4] },
}

struct App {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    exit: bool,
    first_configure: bool,
    width: u32,
    height: u32,
    content_w: u32,   // grid+dock area width (the old surface size)
    content_h: u32,   // grid+dock area height
    grid_offset: (i32, i32), // offset to center content in full-screen surface
    layer: LayerSurface,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    touch: Option<wl_touch::WlTouch>,
    // Grid config
    grid_w: usize,
    grid_h: usize,
    tile_size: u32,
    tile_gap: u32,
    // Grid state: which tiles have icons (index into icons vec)
    tiles: Vec<Option<usize>>,
    icons: Vec<Icon>,
    icon_size: u32,       // configured icon pixel size (used on reload)
    icons_checksum: u64,  // checksum when icons were loaded
    // Input state (shared by pointer and touch)
    pointer_pos: (f64, f64),
    hovered_tile: Option<usize>,  // shared by mouse, touch, and keyboard
    press_start: Option<(f64, f64, usize)>, // (x, y, tile_index) when press/touch began
    drag_from: Option<usize>,
    drag_threshold: f64,
    // Rendering state
    dirty: bool,
    frame_pending: bool,
    first_frame_presented: bool,  // set true when the first frame callback fires (probe)
    icons_at_first_frame: usize,  // snapshot of icons.len() at first frame time (probe)
    // Picker state
    picker_target: Option<usize>,    // which tile we're picking for (None = closed)
    picker_scroll: usize,            // scroll offset in picker list
    picker_hovered: Option<usize>,   // which picker item is hovered (visual index)
    picker_search: String,           // search filter for picker
    // Dock state
    dock: Vec<DockEntry>,
    hovered_dock: Option<usize>,
    dock_font_size: f32,
    // Required: wlgrid exits at startup if no usable font is found.
    fonts: Fonts,
    // Appearance
    theme: Theme,
    scale: i32,
    pad: i32,
    dock_h: u32,
    base_tile_size: u32,
    base_tile_gap: u32,
    base_pad: i32,
    base_dock_font: f32,
    gl_renderer: Option<gpu_gl::GlRenderer>,
    // Search state (zoxide directories)
    search_query: String,
    zoxide_dirs: std::sync::OnceLock<Vec<String>>,
    search_engines: Vec<SearchEngine>,
    hovered_search_engine: Option<usize>,
    search_types: Vec<SearchType>,
    // Cursor
    cursor_theme: Option<CursorTheme>,
    cursor_surface: wl_surface::WlSurface,
    pointer_enter_serial: u32,
    // Startup timing
    startup_time: Instant,
    cache_write_paths: Option<(Option<PathBuf>, Option<PathBuf>)>,
    probe_draw_ms: Vec<f32>,
}

impl App {
    fn record_draw_ms(&mut self, ms: f32) {
        if self.probe_draw_ms.len() >= 2048 {
            let _ = self.probe_draw_ms.remove(0);
        }
        self.probe_draw_ms.push(ms);
    }

    fn ensure_gl_renderer(&mut self, conn: &Connection) -> Result<(), String> {
        if self.gl_renderer.is_some() {
            return Ok(());
        }
        let display_id = conn.display().id();
        let surface_id = self.layer.wl_surface().id();
        let r = gpu_gl::GlRenderer::new(display_id, surface_id, self.width as i32, self.height as i32)?;
        self.gl_renderer = Some(r);
        Ok(())
    }

    /// Rasterise `text` and append it to the frame's draw list as a tinted
    /// sprite whose baseline lands at `baseline_y`. The glyph mask is pushed
    /// into `bufs` (referenced by index) so it outlives command-list building.
    fn push_text(
        &self,
        bufs: &mut Vec<(Vec<u8>, u32, u32)>,
        items: &mut Vec<DrawItem>,
        x: i32,
        baseline_y: i32,
        text: &str,
        size: f32,
        color: [u8; 4],
    ) {
        let fonts = &self.fonts;
        if let Some((buf, w, h, ascent)) = rasterize_text(fonts, text, size) {
            bufs.push((buf, w, h));
            items.push(DrawItem::Text {
                buf: bufs.len() - 1,
                x,
                y: baseline_y - ascent,
                tint: rgba4(color),
            });
        }
    }

    /// Build the frame as an ordered list of GL primitives and submit it. All
    /// scaling (icons, text, layout) happens here at draw time — there is no
    /// pre-scaled icon cache — so only the handful of on-screen icons are ever
    /// resized, and the GPU does it for free via LINEAR sampling.
    fn draw(&mut self, qh: &QueueHandle<Self>) {
        let t0 = Instant::now();
        self.dirty = false;

        if self.gl_renderer.is_none() {
            return;
        }

        // Always centre content for the current surface size, so a frame can
        // never render with a stale offset (e.g. before the final configure).
        self.grid_offset = (
            (self.width as i32 - self.content_w as i32) / 2,
            (self.height as i32 - self.content_h as i32) / 2,
        );

        let t = self.theme;
        let sc = self.scale.max(1) as f32;
        let (sw, sh) = (self.width as i32, self.height as i32);
        // Effective on-screen icon size for the grid; source icons stay at
        // their base resolution and the GPU scales them when drawn.
        let eff = (self.icon_size as f32 * sc).round().max(1.0) as i32;

        let mut bufs: Vec<(Vec<u8>, u32, u32)> = Vec::new();
        let mut items: Vec<DrawItem> = Vec::new();

        // Panel backing the whole grid.
        items.push(DrawItem::Rect {
            x: self.grid_offset.0,
            y: self.grid_offset.1,
            w: self.content_w as i32,
            h: self.content_h as i32,
            radius: t.radius * sc,
            fill: rgba3(t.panel, t.panel_a),
            border: rgba3(t.border, (t.border_a * 0.9).min(1.0)),
            border_w: 1.0 * sc,
        });

        // Grid tiles + their icons.
        let num_tiles = self.grid_w * self.grid_h;
        for i in 0..num_tiles {
            let (tx, ty, tw, th) = self.tile_rect(i);
            let (fill_c, fa) = if self.hovered_tile == Some(i) {
                accent_delta(t.tile, t.tile_a, t.accent_hue_delta, t.accent_amount)
            } else {
                (t.tile, t.tile_a)
            };
            items.push(DrawItem::Rect {
                x: tx, y: ty, w: tw as i32, h: th as i32,
                radius: t.radius * sc,
                fill: rgba3(fill_c, fa),
                border: rgba3(t.border, t.border_a),
                border_w: if t.show_tile_outlines { 1.0 * sc } else { 0.0 },
            });
            if self.drag_from != Some(i) {
                if let Some(idx) = self.tiles.get(i).and_then(|s| *s) {
                    if self.icons.get(idx).is_some() {
                        let ix = tx + (tw as i32 - eff) / 2;
                        let iy = ty + (th as i32 - eff) / 2;
                        items.push(DrawItem::Icon { key: idx as u64 + 1, idx, x: ix, y: iy, w: eff, h: eff });
                    }
                }
            }
        }

        // Dock bar.
        if !self.dock.is_empty() {
            let dock_y = self.dock_bar_y();
            let dock_ox = self.grid_offset.0;
            let dock_cw = self.content_w;
            items.push(DrawItem::Rect {
                x: dock_ox, y: dock_y, w: dock_cw as i32, h: (1.0 * sc).max(1.0) as i32,
                radius: 0.0, fill: rgba3(t.border, t.border_a), border: [0.0; 4], border_w: 0.0,
            });
            let item_count = self.dock.len() as u32;
            let item_spacing = dock_cw / item_count.max(1);
            let font_size = self.dock_font_size;
            for (i, entry) in self.dock.iter().enumerate() {
                let item_start_x = dock_ox + (i as u32 * item_spacing) as i32;
                let center_x = item_start_x + item_spacing as i32 / 2;
                let is_hovered = self.hovered_dock == Some(i);
                if is_hovered {
                    let (fc, fa) = accent_delta(t.tile, t.tile_a, t.accent_hue_delta, t.accent_amount);
                    items.push(DrawItem::Rect {
                        x: item_start_x, y: dock_y, w: item_spacing as i32, h: self.dock_h as i32,
                        radius: t.radius * sc, fill: rgba3(fc, fa), border: [0.0; 4], border_w: 0.0,
                    });
                }
                let color = if is_hovered { [0xFF, 0xFF, 0xFF, 0xFF] } else { [0xAA, 0xAA, 0xAA, 0xFF] };
                let tw = text_width(&self.fonts, &entry.name, font_size);
                let text_x = center_x - tw as i32 / 2;
                let text_y = dock_y + (self.dock_h as i32 / 2) + (font_size as i32 / 3);
                self.push_text(&mut bufs, &mut items, text_x, text_y, &entry.name, font_size, color);
            }
        }

        // Drag overlay: drop-target outline + the dragged icon under the cursor.
        if let Some(from) = self.drag_from {
            if let Some(to) = self.hovered_tile.filter(|&to| to != from) {
                let (tx, ty, tw, th) = self.tile_rect(to);
                items.push(DrawItem::Rect {
                    x: tx, y: ty, w: tw as i32, h: th as i32, radius: t.radius * sc,
                    fill: [0.0; 4], border: [0.0, 1.0, 0.0, 1.0], border_w: (2.0 * sc).max(1.0),
                });
            }
            if let Some(idx) = self.tiles.get(from).and_then(|s| *s) {
                if self.icons.get(idx).is_some() {
                    let x = self.pointer_pos.0 as i32 - eff / 2;
                    let y = self.pointer_pos.1 as i32 - eff / 2;
                    items.push(DrawItem::Icon { key: (idx as u64 + 1) | (1u64 << 63), idx, x, y, w: eff, h: eff });
                }
            }
        }

        // Icon picker overlay.
        if self.picker_target.is_some() {
            items.push(DrawItem::Rect { x: 0, y: 0, w: sw, h: sh, radius: 0.0, fill: [0.0, 0.0, 0.0, 0.5], border: [0.0; 4], border_w: 0.0 });
            let (px, py, pw, ph) = self.picker_rect();
            items.push(DrawItem::Rect {
                x: px, y: py, w: pw as i32, h: ph as i32, radius: t.radius * sc,
                fill: rgba4([0x1A, 0x1A, 0x1A, 0xFF]), border: rgba4([0x55, 0x55, 0x55, 0xFF]), border_w: 2.0 * sc,
            });
            let search_h = (Self::PICKER_SEARCH_HEIGHT as f32 * sc) as i32;
            let search_box_x = px + (8.0 * sc) as i32;
            let search_box_y = py + (8.0 * sc) as i32;
            let search_box_w = pw as i32 - (16.0 * sc) as i32;
            items.push(DrawItem::Rect {
                x: search_box_x, y: search_box_y, w: search_box_w, h: search_h, radius: 4.0 * sc,
                fill: rgba4([0x2D, 0x2D, 0x2D, 0xFF]), border: rgba4([0x55, 0x55, 0x55, 0xFF]), border_w: 1.0,
            });
            let search_text = if self.picker_search.is_empty() { "Type to search...".to_string() } else { self.picker_search.clone() };
            let search_color = if self.picker_search.is_empty() { [0x88, 0x88, 0x88, 0xFF] } else { [0xFF, 0xFF, 0xFF, 0xFF] };
            self.push_text(&mut bufs, &mut items, px + (12.0 * sc) as i32, search_box_y + (22.0 * sc) as i32, &search_text, 16.0 * sc, search_color);

            let filtered = self.filtered_icon_indices();
            let visible_count = Self::PICKER_COLS * Self::PICKER_VISIBLE_ROWS;
            let name_font_size = 10.0 * sc;
            let mut hovered_name: Option<(usize, (i32, i32, u32, u32))> = None;
            for vis in 0..visible_count {
                let fidx = self.picker_scroll + vis;
                if fidx >= filtered.len() { break; }
                let icon_idx = filtered[fidx];
                let (ix, iy, iw, ih) = self.picker_item_rect(vis);
                let bg = if self.picker_hovered == Some(vis) { [0x46, 0x46, 0x46, 0xFF] } else { [0x2D, 0x2D, 0x2D, 0xFF] };
                items.push(DrawItem::Rect { x: ix, y: iy, w: iw as i32, h: ih as i32, radius: 4.0 * sc, fill: rgba4(bg), border: [0.0; 4], border_w: 0.0 });
                if self.icons.get(icon_idx).is_some() {
                    // Scale picker icons with the display like the grid does.
                    let icx = ix + (iw as i32 - eff) / 2;
                    let icy = iy + (4.0 * sc) as i32;
                    items.push(DrawItem::Icon { key: icon_idx as u64 + 1, idx: icon_idx, x: icx, y: icy, w: eff, h: eff });
                }
                {
                    let fonts = &self.fonts;
                    let name = &self.icons[icon_idx].name;
                    let max_chars = 10;
                    let display_name: String = if name.chars().count() > max_chars {
                        format!("{}…", name.chars().take(max_chars - 1).collect::<String>())
                    } else {
                        name.clone()
                    };
                    let tw = text_width(fonts, &display_name, name_font_size);
                    let text_x = ix + (iw as i32 - tw as i32) / 2;
                    let text_y = iy + ih as i32 - (4.0 * sc) as i32;
                    self.push_text(&mut bufs, &mut items, text_x, text_y, &display_name, name_font_size, [0xCC, 0xCC, 0xCC, 0xFF]);
                    if self.picker_hovered == Some(vis) && name.chars().count() > max_chars {
                        hovered_name = Some((icon_idx, (ix, iy, iw, ih)));
                    }
                }
            }
            if let Some((icon_idx, (ix, iy, iw, _ih))) = hovered_name {
                let name = self.icons[icon_idx].name.clone();
                {
                    let fonts = &self.fonts;
                    let ttf = 12.0 * sc;
                    let tw = text_width(fonts, &name, ttf) as i32;
                    let padding = (6.0 * sc) as i32;
                    let tooltip_w = tw + padding * 2;
                    let tooltip_h = (20.0 * sc) as i32;
                    let mut tx = ix + (iw as i32 - tooltip_w) / 2;
                    let mut ty = iy - tooltip_h - (4.0 * sc) as i32;
                    tx = tx.max(px + 4).min(px + pw as i32 - tooltip_w - 4);
                    ty = ty.max(py + 4);
                    items.push(DrawItem::Rect { x: tx, y: ty, w: tooltip_w, h: tooltip_h, radius: 4.0 * sc, fill: rgba4([0x00, 0x00, 0x00, 0xEE]), border: rgba4([0x88, 0x88, 0x88, 0xFF]), border_w: 1.0 });
                    self.push_text(&mut bufs, &mut items, tx + padding, ty + (15.0 * sc) as i32, &name, ttf, [0xFF, 0xFF, 0xFF, 0xFF]);
                }
            }
        }

        // Search box (shown while a query is being typed).
        if !self.search_query.is_empty() {
            {
                let fonts = &self.fonts;
                let best = self.find_best_search_match();
                let (display_text, has_match, show_engines, first_icon_idx) = match &best {
                    SearchMatch::Folder(dir) => {
                        let short = dir.rsplit('/').next().unwrap_or(dir);
                        (format!("{} → {}", self.search_query, short), true, false, None)
                    }
                    SearchMatch::Desktop(indices) => {
                        let first = indices.first().copied();
                        let name = first.and_then(|i| self.icons.get(i)).map(|ic| ic.name.as_str()).unwrap_or("");
                        (format!("{} → {}", self.search_query, name), true, false, first)
                    }
                    SearchMatch::None => (self.search_query.clone(), false, true, None),
                };
                let font_size = 24.0 * sc;
                let btn_font_size = 14.0 * sc;
                let btn_gap = (8.0 * sc) as i32;
                let tw = text_width(fonts, &display_text, font_size) as u32;
                let btn_widths: Vec<u32> = if show_engines && !self.search_engines.is_empty() {
                    self.search_engines.iter().map(|e| text_width(fonts, &e.name, btn_font_size) as u32 + (16.0 * sc) as u32).collect()
                } else {
                    vec![]
                };
                let buttons_total_w = if btn_widths.is_empty() {
                    0
                } else {
                    btn_widths.iter().sum::<u32>() + (btn_widths.len() as u32 - 1) * btn_gap as u32
                };
                let icon_px = (32.0 * sc) as u32;
                let icon_padding = if first_icon_idx.is_some() { icon_px + (8.0 * sc) as u32 } else { 0 };
                let pad = (32.0 * sc) as u32;
                let box_w = (tw + icon_padding).max((200.0 * sc) as u32).max(buttons_total_w + pad) + pad;
                let box_h = if show_engines { (88.0 * sc) as u32 } else { (40.0 * sc) as u32 };
                let box_x = (sw - box_w as i32) / 2;
                let box_y = self.grid_offset.1 + (8.0 * sc) as i32;
                items.push(DrawItem::Rect {
                    x: box_x, y: box_y, w: box_w as i32, h: box_h as i32, radius: 6.0 * sc,
                    fill: rgba4([0x18, 0x14, 0x10, 0xEE]), border: rgba4([0x60, 0x60, 0x80, 0xFF]), border_w: (1.0 * sc).max(1.0),
                });
                if let Some(icon_idx) = first_icon_idx {
                    if self.icons.get(icon_idx).is_some() {
                        let icon_x = box_x + 8;
                        let icon_y = box_y + (box_h as i32 - icon_px as i32) / 2;
                        items.push(DrawItem::Icon { key: icon_idx as u64 + 1, idx: icon_idx, x: icon_x, y: icon_y, w: icon_px as i32, h: icon_px as i32 });
                    }
                }
                let text_x = box_x + (16.0 * sc) as i32 + icon_padding as i32;
                let text_y = box_y + (28.0 * sc) as i32;
                let tcolor = if has_match { [0xFF, 0xFF, 0xFF, 0xFF] } else { [0xCC, 0xCC, 0xCC, 0xFF] };
                self.push_text(&mut bufs, &mut items, text_x, text_y, &display_text, font_size, tcolor);
                if show_engines && !btn_widths.is_empty() {
                    let btn_y = box_y + (48.0 * sc) as i32;
                    let btn_h = (28.0 * sc) as i32;
                    let mut btn_x = box_x + (box_w as i32 - buttons_total_w as i32) / 2;
                    for (i, engine) in self.search_engines.iter().enumerate() {
                        let btn_w = btn_widths[i];
                        let is_hovered = self.hovered_search_engine.unwrap_or(0) == i;
                        let bg = if is_hovered { [0x60, 0x50, 0x40, 0xFF] } else { [0x30, 0x28, 0x20, 0xFF] };
                        let border = if is_hovered { rgba4([0xFF, 0xFF, 0xFF, 0xFF]) } else { [0.0; 4] };
                        items.push(DrawItem::Rect { x: btn_x, y: btn_y, w: btn_w as i32, h: btn_h, radius: 4.0 * sc, fill: rgba4(bg), border, border_w: if is_hovered { 1.0 } else { 0.0 } });
                        let txt_color = if is_hovered { [0xFF, 0xFF, 0xFF, 0xFF] } else { [0xBB, 0xBB, 0xBB, 0xFF] };
                        self.push_text(&mut bufs, &mut items, btn_x + (8.0 * sc) as i32, btn_y + (19.0 * sc) as i32, &engine.name, btn_font_size, txt_color);
                        btn_x += btn_w as i32 + btn_gap;
                    }
                }
            }
        }

        // Translate the z-ordered item list into GL commands. Icon sprites
        // borrow `self.icons`; text sprites borrow the stable `bufs`.
        let mut cmds: Vec<gpu_gl::GlCmd> = Vec::with_capacity(items.len());
        for it in &items {
            match it {
                DrawItem::Rect { x, y, w, h, radius, fill, border, border_w } => {
                    cmds.push(gpu_gl::GlCmd::Rect(gpu_gl::GlRect {
                        x: *x, y: *y, w: *w, h: *h, radius: *radius, fill: *fill, border: *border, border_w: *border_w,
                    }));
                }
                DrawItem::Icon { key, idx, x, y, w, h } => {
                    if let Some(icon) = self.icons.get(*idx) {
                        cmds.push(gpu_gl::GlCmd::Sprite(gpu_gl::GlSprite {
                            key: *key, x: *x, y: *y, w: *w, h: *h,
                            pixels: &icon.pixels, src_w: icon.width as i32, src_h: icon.height as i32, tint: [1.0; 4],
                        }));
                    }
                }
                DrawItem::Text { buf, x, y, tint } => {
                    let (b, bw, bh) = &bufs[*buf];
                    cmds.push(gpu_gl::GlCmd::Sprite(gpu_gl::GlSprite {
                        key: 0, x: *x, y: *y, w: *bw as i32, h: *bh as i32,
                        pixels: b, src_w: *bw as i32, src_h: *bh as i32, tint: *tint,
                    }));
                }
            }
        }

        let render_res = self.gl_renderer.as_mut().map(|r| r.render(t.dim, &cmds));
        drop(cmds);
        drop(bufs);
        if let Some(Err(e)) = render_res {
            eprintln!("wlgrid: gl render failed: {e}");
        }

        self.layer.wl_surface().damage_buffer(0, 0, sw, sh);
        self.layer.wl_surface().frame(qh, self.layer.wl_surface().clone());
        self.layer.commit();
        self.frame_pending = true;
        self.record_draw_ms(t0.elapsed().as_secs_f64() as f32 * 1000.0);
    }

    /// Recompute layout dimensions for a new display scale. Icons are not
    /// touched — they stay at their base resolution and are scaled at draw time.
    fn apply_scale(&mut self, new_scale: i32) {
        let s = new_scale.max(1);
        if s == self.scale {
            return;
        }
        self.scale = s;
        self.pad = self.base_pad * s;
        self.dock_h = DOCK_HEIGHT * s as u32;
        self.tile_size = self.base_tile_size * s as u32;
        self.tile_gap = self.base_tile_gap * s as u32;
        self.dock_font_size = self.base_dock_font * s as f32;
        let (cw, ch) = self.required_size();
        self.content_w = cw;
        self.content_h = if self.dock.is_empty() { ch } else { ch + self.dock_h };
        self.grid_offset = (
            (self.width as i32 - self.content_w as i32) / 2,
            (self.height as i32 - self.content_h as i32) / 2,
        );
    }

    fn required_size(&self) -> (u32, u32) {
        let p = 2 * self.pad as u32;
        let w = p + self.grid_w as u32 * self.tile_size + self.grid_w.saturating_sub(1) as u32 * self.tile_gap;
        let h = p + self.grid_h as u32 * self.tile_size + self.grid_h.saturating_sub(1) as u32 * self.tile_gap;
        (w, h)
    }

    fn tile_rect(&self, index: usize) -> (i32, i32, u32, u32) {
        let col = index % self.grid_w;
        let row = index / self.grid_w;
        let x = self.grid_offset.0 + self.pad + (col as u32 * (self.tile_size + self.tile_gap)) as i32;
        let y = self.grid_offset.1 + self.pad + (row as u32 * (self.tile_size + self.tile_gap)) as i32;
        (x, y, self.tile_size, self.tile_size)
    }

    fn tile_at(&self, x: f64, y: f64) -> Option<usize> {
        for i in 0..(self.grid_w * self.grid_h) {
            let (tx, ty, tw, th) = self.tile_rect(i);
            if x >= tx as f64 && x < (tx + tw as i32) as f64 &&
               y >= ty as f64 && y < (ty + th as i32) as f64 {
                return Some(i);
            }
        }
        None
    }

    fn point_in_content_bounds(&self, x: f64, y: f64) -> bool {
        let ox = self.grid_offset.0 as f64;
        let oy = self.grid_offset.1 as f64;
        x >= ox && x < (ox + self.content_w as f64)
            && y >= oy && y < (oy + self.content_h as f64)
    }

    /// Unified press handler for both pointer and touch.
    /// Returns true if a redraw is needed.
    fn handle_picker_press(&mut self, x: f64, y: f64) -> bool {
        if let Some(visual_idx) = self.picker_item_at(x, y) {
            let filtered = self.filtered_icon_indices();
            let filtered_idx = self.picker_scroll + visual_idx;
            if filtered_idx < filtered.len() {
                let icon_idx = filtered[filtered_idx];
                if let Some(target) = self.picker_target {
                    self.tiles[target] = Some(icon_idx);
                    dlog!("  picker: assigned icon {} to tile {}", self.icons[icon_idx].name, target);
                }
            }
        } else {
            dlog!("  picker: closed (pressed outside)");
        }
        self.picker_target = None;
        self.picker_hovered = None;
        self.picker_search.clear();
        true
    }

    /// Unified press handler for normal (non-picker) interactions.
    /// Returns true if a redraw is needed.
    fn handle_press(&mut self, x: f64, y: f64) -> bool {
        if let Some(engine_idx) = self.search_engine_at(x, y) {
            if let Some(engine) = self.search_engines.get(engine_idx) {
                let query = self.search_query.replace(' ', "+");
                let url = engine.url_template.replace("{}", &query);
                let _ = Command::new("xdg-open")
                    .arg(&url)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
                dlog!("  search {} for: {}", engine.name, self.search_query);
                self.exit = true;
            }
        } else if let Some(tile) = self.tile_at(x, y) {
            self.press_start = Some((x, y, tile));
            self.hovered_tile = Some(tile);
            return true;
        } else if let Some(dock_idx) = self.dock_item_at(x, y) {
            if let Some(entry) = self.dock.get(dock_idx) {
                launch_exec(&entry.exec, &entry.name);
                self.exit = true;
            }
        } else if !self.point_in_content_bounds(x, y) {
            dlog!("  closed (pressed outside grid)");
            self.exit = true;
        }
        false
    }

    /// Unified release handler for both pointer and touch.
    /// Returns true if a redraw is needed.
    fn handle_release(&mut self, x: f64, y: f64) -> bool {
        let mut needs_redraw = false;
        if let Some(from) = self.drag_from.take() {
            if let Some(to) = self.tile_at(x, y) {
                if from != to {
                    self.tiles.swap(from, to);
                    dlog!("  swapped tile {} <-> {}", from, to);
                }
            }
            needs_redraw = true;
        } else if let Some((_, _, tile)) = self.press_start.take() {
            if self.tiles[tile].is_none() {
                self.refresh_icons_if_needed();
                dlog!("  picker: opening for tile {}", tile);
                self.picker_target = Some(tile);
                self.picker_scroll = 0;
                self.picker_hovered = None;
                self.picker_search.clear();
                needs_redraw = true;
            } else if let Some(icon_idx) = self.tiles[tile] {
                if let Some(icon) = self.icons.get(icon_idx) {
                    launch_exec(&icon.exec, &icon.name);
                    self.exit = true;
                }
            }
        }
        self.press_start = None;
        needs_redraw
    }

    /// Check if motion should start a drag. Returns true if a redraw is needed.
    fn handle_drag_motion(&mut self, x: f64, y: f64) -> bool {
        let mut needs_redraw = false;
        if let Some((px, py, tile)) = self.press_start {
            let dx = x - px;
            let dy = y - py;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist > self.drag_threshold && self.drag_from.is_none() {
                self.drag_from = Some(tile);
                self.press_start = None;
                dlog!("  drag start from tile {}", tile);
                needs_redraw = true;
            }
        }
        if self.drag_from.is_some() {
            self.pointer_pos = (x, y);
            if let Some(tile) = self.tile_at(x, y) {
                if self.hovered_tile != Some(tile) {
                    self.hovered_tile = Some(tile);
                }
            }
            needs_redraw = true;
        }
        needs_redraw
    }

    fn dock_bar_y(&self) -> i32 {
        // Dock starts after the grid
        let grid_h = 2 * self.pad as u32 + self.grid_h as u32 * self.tile_size + self.grid_h.saturating_sub(1) as u32 * self.tile_gap;
        self.grid_offset.1 + grid_h as i32
    }

    fn dock_item_at(&self, x: f64, y: f64) -> Option<usize> {
        if self.dock.is_empty() {
            return None;
        }
        let dock_y = self.dock_bar_y();
        if y < dock_y as f64 || y >= (dock_y as f64 + self.dock_h as f64) {
            return None;
        }
        // Smaller hitboxes (60% of spacing, centered) within content area
        let ox = self.grid_offset.0 as u32;
        let item_spacing = self.content_w / self.dock.len() as u32;
        let hitbox_width = (item_spacing as f64 * 0.6) as u32;
        let hitbox_margin = (item_spacing - hitbox_width) / 2;
        for i in 0..self.dock.len() {
            let start_x = ox + i as u32 * item_spacing + hitbox_margin;
            let end_x = start_x + hitbox_width;
            if x >= start_x as f64 && x < end_x as f64 {
                return Some(i);
            }
        }
        None
    }

    fn request_frame(&mut self, qh: &QueueHandle<Self>) {
        if !self.frame_pending {
            self.layer.wl_surface().frame(qh, self.layer.wl_surface().clone());
            self.layer.commit();
            self.frame_pending = true;
            dlog!("  request_frame: scheduled");
        }
    }

    /// Check if desktop entries have changed and reload icons if so.
    /// Returns true if icons were reloaded.
    fn refresh_icons_if_needed(&mut self) -> bool {
        let current_checksum = compute_checksum();
        if current_checksum == self.icons_checksum {
            return false;
        }

        dlog!("  picker: desktop entries changed, reloading icons");

        // Save current tile -> name mappings before reload
        let tile_names: Vec<Option<String>> = self.tiles.iter()
            .map(|opt| opt.and_then(|idx| self.icons.get(idx).map(|i| i.name.clone())))
            .collect();

        // Reload desktop entries
        self.icons = load_desktop_entries(self.icon_size, Some(&self.fonts));
        dlog!("  picker: reloaded {} icons", self.icons.len());

        // Remap tiles by name
        for (i, opt_name) in tile_names.iter().enumerate() {
            self.tiles[i] = opt_name.as_ref().and_then(|name| {
                self.icons.iter().position(|icon| &icon.name == name)
            });
        }

        // Update stored checksum
        self.icons_checksum = current_checksum;

        // Save new cache (without font paths since we don't have them here)
        save_cache(&self.icons, None, None);

        true
    }

    // Picker constants
    const PICKER_ITEM_WIDTH: u32 = 80;   // wider to fit text
    const PICKER_ITEM_HEIGHT: u32 = 72;  // taller for icon + text
    const PICKER_ITEM_GAP: u32 = 8;
    const PICKER_COLS: usize = 6;
    const PICKER_VISIBLE_ROWS: usize = 5;
    const PICKER_SEARCH_HEIGHT: u32 = 32;

    fn picker_rect(&self) -> (i32, i32, u32, u32) {
        // Center the picker in the surface, scaled to the display scale.
        let s = self.scale.max(1) as u32;
        let cols = Self::PICKER_COLS as u32;
        let rows = Self::PICKER_VISIBLE_ROWS as u32;
        let pw = (16 + cols * Self::PICKER_ITEM_WIDTH + (cols - 1) * Self::PICKER_ITEM_GAP) * s;
        let ph = (16 + Self::PICKER_SEARCH_HEIGHT + 8 + rows * Self::PICKER_ITEM_HEIGHT + (rows - 1) * Self::PICKER_ITEM_GAP) * s;
        let px = (self.width as i32 - pw as i32) / 2;
        let py = (self.height as i32 - ph as i32) / 2;
        (px, py, pw, ph)
    }

    fn picker_items_y(&self) -> i32 {
        let s = self.scale.max(1);
        let (_, py, _, _) = self.picker_rect();
        py + (8 + Self::PICKER_SEARCH_HEIGHT as i32 + 8) * s
    }

    fn picker_item_rect(&self, index: usize) -> (i32, i32, u32, u32) {
        let s = self.scale.max(1) as u32;
        let (px, _, _, _) = self.picker_rect();
        let items_y = self.picker_items_y();
        let col = index % Self::PICKER_COLS;
        let row = index / Self::PICKER_COLS;
        let x = px + (8 * s) as i32 + (col as u32 * (Self::PICKER_ITEM_WIDTH + Self::PICKER_ITEM_GAP) * s) as i32;
        let y = items_y + (row as u32 * (Self::PICKER_ITEM_HEIGHT + Self::PICKER_ITEM_GAP) * s) as i32;
        (x, y, Self::PICKER_ITEM_WIDTH * s, Self::PICKER_ITEM_HEIGHT * s)
    }

    /// Returns indices of icons matching the picker search filter
    fn filtered_icon_indices(&self) -> Vec<usize> {
        if self.picker_search.is_empty() {
            (0..self.icons.len()).collect()
        } else {
            let query = self.picker_search.to_lowercase();
            self.icons.iter()
                .enumerate()
                .filter(|(_, icon)| icon.name_lower.contains(&query))
                .map(|(i, _)| i)
                .collect()
        }
    }

    fn picker_item_at(&self, x: f64, y: f64) -> Option<usize> {
        let (px, py, pw, ph) = self.picker_rect();
        // Check if inside picker bounds
        if x < px as f64 || x >= (px + pw as i32) as f64 ||
           y < py as f64 || y >= (py + ph as i32) as f64 {
            return None;
        }
        // Check each visible item
        let filtered = self.filtered_icon_indices();
        let visible_count = Self::PICKER_COLS * Self::PICKER_VISIBLE_ROWS;
        for i in 0..visible_count {
            let filtered_idx = self.picker_scroll + i;
            if filtered_idx >= filtered.len() { break; }
            let (ix, iy, iw, ih) = self.picker_item_rect(i);
            if x >= ix as f64 && x < (ix + iw as i32) as f64 &&
               y >= iy as f64 && y < (iy + ih as i32) as f64 {
                return Some(i);  // Return visual index, not icon index
            }
        }
        None
    }

    fn find_best_zoxide_match(&self) -> Option<&str> {
        // Only search folders if enabled in config
        if !self.search_types.contains(&SearchType::Folders) {
            return None;
        }
        if self.search_query.is_empty() {
            return None;
        }
        let query = self.search_query.to_lowercase();

        // Search by directory name (last component) or full path
        // First try exact match on directory name
        let zoxide_dirs = self.zoxide_dirs.get_or_init(load_zoxide_dirs);
        if let Some(dir) = zoxide_dirs.iter().find(|d| {
            d.rsplit('/').next().unwrap_or(d).to_lowercase().starts_with(&query)
        }) {
            return Some(dir);
        }

        // Then try contains match on full path
        zoxide_dirs.iter().find(|d| d.to_lowercase().contains(&query)).map(|s| s.as_str())
    }

    /// Find desktop entries matching the search query (for main search bar)
    fn find_desktop_matches(&self) -> Vec<usize> {
        // Only search desktop entries if enabled in config
        if !self.search_types.contains(&SearchType::Desktop) {
            return vec![];
        }
        if self.search_query.is_empty() {
            return vec![];
        }
        let query = self.search_query.to_lowercase();

        // First: exact prefix matches on name
        let mut matches = Vec::new();
        let mut is_prefix = vec![false; self.icons.len()];
        for (i, icon) in self.icons.iter().enumerate() {
            if icon.name_lower.starts_with(&query) {
                matches.push(i);
                is_prefix[i] = true;
            }
        }

        // Then: contains matches (excluding already matched)
        for (i, icon) in self.icons.iter().enumerate() {
            if !is_prefix[i] && icon.name_lower.contains(&query) {
                matches.push(i);
            }
        }
        matches
    }

    /// Get the best search result based on configured priority
    fn find_best_search_match(&self) -> SearchMatch {
        if self.search_query.is_empty() {
            return SearchMatch::None;
        }

        // Iterate through search types in configured priority order
        for search_type in &self.search_types {
            match search_type {
                SearchType::Folders => {
                    if let Some(dir) = self.find_best_zoxide_match() {
                        return SearchMatch::Folder(dir.to_string());
                    }
                }
                SearchType::Desktop => {
                    let matches = self.find_desktop_matches();
                    if !matches.is_empty() {
                        return SearchMatch::Desktop(matches);
                    }
                }
            }
        }

        SearchMatch::None
    }

    fn search_engines_active(&self) -> bool {
        !self.search_query.is_empty()
            && matches!(self.find_best_search_match(), SearchMatch::None)
            && !self.search_engines.is_empty()
    }

    fn search_engine_at(&self, x: f64, y: f64) -> Option<usize> {
        // Only active when search query exists and no zoxide match
        if self.search_query.is_empty() || self.find_best_zoxide_match().is_some() {
            return None;
        }

        let fonts = &self.fonts;

        let s = self.scale.max(1) as f32;
        let font_size = 24.0 * s;
        let tw = text_width(fonts, &self.search_query, font_size) as u32;
        let box_w = tw.max((200.0 * s) as u32) + (32.0 * s) as u32;
        let box_x = (self.width as i32 - box_w as i32) / 2;
        let box_y = self.grid_offset.1 + (8.0 * s) as i32;

        let btn_font_size = 14.0 * s;
        let btn_y = box_y + (45.0 * s) as i32;
        let btn_h = (28.0 * s) as i32;
        let btn_gap = (8.0 * s) as i32;

        // Check if y is in button row
        if y < btn_y as f64 || y >= (btn_y + btn_h) as f64 {
            return None;
        }

        // Calculate button positions
        let btn_widths: Vec<u32> = self.search_engines.iter()
            .map(|e| text_width(fonts, &e.name, btn_font_size) as u32 + (16.0 * s) as u32)
            .collect();
        let total_w: i32 = btn_widths.iter().map(|w| *w as i32 + btn_gap).sum::<i32>() - btn_gap;
        let mut btn_x = box_x + (box_w as i32 - total_w) / 2;

        for (i, btn_w) in btn_widths.iter().enumerate() {
            if x >= btn_x as f64 && x < (btn_x + *btn_w as i32) as f64 {
                return Some(i);
            }
            btn_x += *btn_w as i32 + btn_gap;
        }
        None
    }

    fn open_directory(&self, dir: &str) {
        // Open with vscodium/code (preferred for dirs and text files)
        let editors = ["codium", "code", "vscodium"];

        for editor in editors {
            if Command::new(editor)
                .arg(dir)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .is_ok()
            {
                dlog!("  opening {} with {}", dir, editor);
                return;
            }
        }

        // Fallback to xdg-open
        let _ = Command::new("xdg-open")
            .arg(dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        dlog!("  opening {} with xdg-open", dir);
    }

}

// ── handler impls (mostly stubs) ──

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _conn: &Connection, qh: &QueueHandle<Self>, surface: &wl_surface::WlSurface, new_factor: i32) {
        if new_factor < 1 || new_factor == self.scale {
            return;
        }
        dlog!("  scale_factor_changed: {} -> {}", self.scale, new_factor);
        surface.set_buffer_scale(new_factor);
        self.width = self.width / self.scale as u32 * new_factor as u32;
        self.height = self.height / self.scale as u32 * new_factor as u32;
        self.apply_scale(new_factor);
        // Deliberately do NOT create the renderer here: before the first
        // configure we don't yet know the real surface size, and creating a
        // wl_egl_window at the wrong size then resizing it before its first
        // buffer isn't reliably honored (the first frame lands in the
        // top-left). configure() creates it once, at the correct size.
        if let Some(r) = self.gl_renderer.as_mut() {
            r.resize(self.width as i32, self.height as i32);
        }
        self.dirty = true;
        // If we've already presented a frame, set_buffer_scale has just made
        // the currently-attached buffer render at the wrong size (e.g. a 1x
        // buffer shown at 2x lands in the top-left quarter). Repaint a fresh,
        // correctly-sized buffer now rather than committing the stale one via
        // request_frame and waiting for the next frame callback / input event.
        if !self.first_configure && self.gl_renderer.is_some() {
            self.draw(qh);
        } else {
            self.request_frame(qh);
        }
    }
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.frame_pending = false;
        if !self.first_frame_presented {
            self.first_frame_presented = true;
            self.icons_at_first_frame = self.icons.len();
            if let Some((text_path, symbols_path)) = self.cache_write_paths.take() {
                let icons = self.icons.clone();
                thread::spawn(move || {
                    save_cache(
                        &icons,
                        text_path.as_deref(),
                        symbols_path.as_deref(),
                    );
                });
            }
        }
        if self.dirty {
            self.draw(qh);
        }
    }
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState { &mut self.output_state }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) { self.exit = true; }
    fn configure(&mut self, conn: &Connection, qh: &QueueHandle<Self>, _: &LayerSurface, cfg: LayerSurfaceConfigure, _: u32) {
        let old_w = self.width;
        let old_h = self.height;
        if cfg.new_size.0 != 0 { self.width = cfg.new_size.0 * self.scale as u32; }
        if cfg.new_size.1 != 0 { self.height = cfg.new_size.1 * self.scale as u32; }

        // GL is the only renderer: make sure it exists, then size it to the
        // surface. Created here (not at startup) because it needs the surface.
        if self.gl_renderer.is_none() {
            let gl_t = Instant::now();
            if let Err(e) = self.ensure_gl_renderer(conn) {
                eprintln!("wlgrid: gl init failed: {e}");
            }
            dlog!("  gl init: {:.2}ms (elapsed {:.2}ms)",
                gl_t.elapsed().as_secs_f64() * 1000.0,
                self.startup_time.elapsed().as_secs_f64() * 1000.0);
        }
        let size_changed = self.width != old_w || self.height != old_h;
        if size_changed {
            self.grid_offset = (
                (self.width as i32 - self.content_w as i32) / 2,
                (self.height as i32 - self.content_h as i32) / 2,
            );
            dlog!("  grid_offset: ({}, {})", self.grid_offset.0, self.grid_offset.1);
        }
        if let Some(r) = self.gl_renderer.as_mut() {
            r.resize(self.width as i32, self.height as i32);
        }

        if self.first_configure {
            self.first_configure = false;
            dlog!("  configured {}x{}, content {}x{}, drawing first frame", self.width, self.height, self.content_w, self.content_h);
            self.draw(qh);
            dlog!("  Time to interactive: {:.2}ms", self.startup_time.elapsed().as_secs_f64() * 1000.0);
        } else if size_changed {
            // A later configure changed our size (final dimensions): recentre
            // and repaint immediately, otherwise the stale (now wrong-sized)
            // buffer lingers until the next input event.
            self.dirty = true;
            self.draw(qh);
        }
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState { &mut self.seat_state }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Keyboard && self.keyboard.is_none() {
            self.keyboard = Some(self.seat_state.get_keyboard(qh, &seat, None).expect("keyboard"));
        }
        if cap == Capability::Pointer && self.pointer.is_none() {
            self.pointer = Some(self.seat_state.get_pointer(qh, &seat).expect("pointer"));
        }
        if cap == Capability::Touch && self.touch.is_none() {
            self.touch = Some(self.seat_state.get_touch(qh, &seat).expect("touch"));
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Keyboard { self.keyboard.take().map(|k| k.release()); }
        if cap == Capability::Pointer { self.pointer.take().map(|p| p.release()); }
        if cap == Capability::Touch { self.touch.take().map(|t| t.release()); }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for App {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {}
    fn press_key(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        dlog!("  key: {:?}", event.keysym);

        if event.keysym == Keysym::Escape {
            if !self.search_query.is_empty() {
                // Clear search first
                self.search_query.clear();
                self.hovered_search_engine = None;
                self.dirty = true;
                self.request_frame(qh);
            } else if self.picker_target.is_some() {
                // Close picker
                dlog!("  picker: closed (escape)");
                self.picker_target = None;
                self.picker_hovered = None;
                self.picker_search.clear();
                self.dirty = true;
                self.request_frame(qh);
            } else {
                // Exit app
                self.exit = true;
            }
        } else if event.keysym == Keysym::BackSpace {
            if self.picker_target.is_some() && !self.picker_search.is_empty() {
                self.picker_search.pop();
                self.picker_scroll = 0;  // Reset scroll when search changes
                self.picker_hovered = Some(0);
                dlog!("  picker search: '{}'", self.picker_search);
                self.dirty = true;
                self.request_frame(qh);
            } else if !self.search_query.is_empty() {
                self.search_query.pop();
                if self.search_engines_active() {
                    self.hovered_search_engine = Some(0);
                } else {
                    self.hovered_search_engine = None;
                }
                dlog!("  search: '{}'", self.search_query);
                self.dirty = true;
                self.request_frame(qh);
            }
        } else if event.keysym == Keysym::Delete {
            // Delete key removes the focused tile's entry (same as right-click)
            if self.picker_target.is_none() {
                if let Some(tile) = self.hovered_tile {
                    if self.tiles[tile].is_some() {
                        self.tiles[tile] = None;
                        dlog!("  removed tile {} (delete key)", tile);
                        self.dirty = true;
                        self.request_frame(qh);
                    }
                }
            }
        } else if event.keysym == Keysym::Left {
            if self.picker_target.is_some() {
                // Navigate picker left
                let filtered_len = self.filtered_icon_indices().len();
                let visible_count = Self::PICKER_COLS * Self::PICKER_VISIBLE_ROWS;
                let max_idx = visible_count.min(filtered_len.saturating_sub(self.picker_scroll));
                if let Some(idx) = self.picker_hovered {
                    if idx % Self::PICKER_COLS > 0 {
                        self.picker_hovered = Some(idx - 1);
                        self.dirty = true;
                        self.request_frame(qh);
                    }
                } else if max_idx > 0 {
                    self.picker_hovered = Some(0);
                    self.dirty = true;
                    self.request_frame(qh);
                }
            } else if let Some(di) = self.hovered_dock {
                if di > 0 {
                    self.hovered_dock = Some(di - 1);
                    self.dirty = true;
                    self.request_frame(qh);
                }
            } else if self.search_engines_active() {
                let n = self.search_engines.len();
                let cur = self.hovered_search_engine.unwrap_or(0).min(n.saturating_sub(1));
                self.hovered_search_engine = Some(cur.saturating_sub(1));
                self.dirty = true;
                self.request_frame(qh);
            } else if let Some(idx) = self.hovered_tile {
                // Move tile focus left
                let col = idx % self.grid_w;
                if col > 0 {
                    self.hovered_tile = Some(idx - 1);
                    self.dirty = true;
                    self.request_frame(qh);
                }
            }
        } else if event.keysym == Keysym::Right {
            if self.picker_target.is_some() {
                // Navigate picker right
                let filtered_len = self.filtered_icon_indices().len();
                let visible_count = Self::PICKER_COLS * Self::PICKER_VISIBLE_ROWS;
                let max_idx = visible_count.min(filtered_len.saturating_sub(self.picker_scroll));
                if let Some(idx) = self.picker_hovered {
                    if idx % Self::PICKER_COLS < Self::PICKER_COLS - 1 && idx + 1 < max_idx {
                        self.picker_hovered = Some(idx + 1);
                        self.dirty = true;
                        self.request_frame(qh);
                    }
                } else if max_idx > 0 {
                    self.picker_hovered = Some(0);
                    self.dirty = true;
                    self.request_frame(qh);
                }
            } else if let Some(di) = self.hovered_dock {
                if di + 1 < self.dock.len() {
                    self.hovered_dock = Some(di + 1);
                    self.dirty = true;
                    self.request_frame(qh);
                }
            } else if self.search_engines_active() {
                let n = self.search_engines.len();
                let cur = self.hovered_search_engine.unwrap_or(0).min(n.saturating_sub(1));
                self.hovered_search_engine = Some((cur + 1).min(n.saturating_sub(1)));
                self.dirty = true;
                self.request_frame(qh);
            } else if let Some(idx) = self.hovered_tile {
                // Move tile focus right
                let col = idx % self.grid_w;
                let num_tiles = self.grid_w * self.grid_h;
                if col < self.grid_w - 1 && idx + 1 < num_tiles {
                    self.hovered_tile = Some(idx + 1);
                    self.dirty = true;
                    self.request_frame(qh);
                }
            }
        } else if event.keysym == Keysym::Up {
            if self.picker_target.is_some() {
                // Navigate picker up (or scroll)
                if let Some(idx) = self.picker_hovered {
                    if idx >= Self::PICKER_COLS {
                        self.picker_hovered = Some(idx - Self::PICKER_COLS);
                        self.dirty = true;
                        self.request_frame(qh);
                    } else if self.picker_scroll > 0 {
                        // Scroll up
                        self.picker_scroll = self.picker_scroll.saturating_sub(Self::PICKER_COLS);
                        self.dirty = true;
                        self.request_frame(qh);
                    }
                } else {
                    self.picker_hovered = Some(0);
                    self.dirty = true;
                    self.request_frame(qh);
                }
            } else if let Some(di) = self.hovered_dock {
                // Ascend out of the dock back into the grid's bottom row.
                let col = (di * self.grid_w / self.dock.len().max(1)).min(self.grid_w - 1);
                self.hovered_dock = None;
                self.hovered_tile = Some((self.grid_h - 1) * self.grid_w + col);
                self.dirty = true;
                self.request_frame(qh);
            } else if let Some(idx) = self.hovered_tile {
                // Move tile focus up
                if idx >= self.grid_w {
                    self.hovered_tile = Some(idx - self.grid_w);
                    self.dirty = true;
                    self.request_frame(qh);
                }
            }
        } else if event.keysym == Keysym::Down {
            if self.picker_target.is_some() {
                // Navigate picker down (or scroll)
                let filtered_len = self.filtered_icon_indices().len();
                let visible_count = Self::PICKER_COLS * Self::PICKER_VISIBLE_ROWS;
                let max_idx = visible_count.min(filtered_len.saturating_sub(self.picker_scroll));
                let max_scroll = filtered_len.saturating_sub(visible_count);
                if let Some(idx) = self.picker_hovered {
                    if idx + Self::PICKER_COLS < max_idx {
                        self.picker_hovered = Some(idx + Self::PICKER_COLS);
                        self.dirty = true;
                        self.request_frame(qh);
                    } else if self.picker_scroll < max_scroll {
                        // Scroll down
                        self.picker_scroll = (self.picker_scroll + Self::PICKER_COLS).min(max_scroll);
                        self.dirty = true;
                        self.request_frame(qh);
                    }
                } else if max_idx > 0 {
                    self.picker_hovered = Some(0);
                    self.dirty = true;
                    self.request_frame(qh);
                }
            } else if let Some(idx) = self.hovered_tile {
                // Move tile focus down
                let num_tiles = self.grid_w * self.grid_h;
                if idx + self.grid_w < num_tiles {
                    self.hovered_tile = Some(idx + self.grid_w);
                    self.dirty = true;
                    self.request_frame(qh);
                } else if !self.dock.is_empty() {
                    // Bottom row: descend into the dock bar, keeping the column.
                    let col = idx % self.grid_w;
                    self.hovered_dock = Some((col * self.dock.len() / self.grid_w).min(self.dock.len() - 1));
                    self.hovered_tile = None;
                    self.dirty = true;
                    self.request_frame(qh);
                }
            }
        } else if event.keysym == Keysym::Tab {
            // Tab completion for paths
            if self.search_query.contains('/') || self.search_query.starts_with('~') {
                // Expand tilde to home directory
                let expanded_path = if self.search_query.starts_with('~') {
                    if let Some(home) = std::env::var_os("HOME") {
                        self.search_query.replacen('~', &home.to_string_lossy(), 1)
                    } else {
                        self.search_query.clone()
                    }
                } else {
                    self.search_query.clone()
                };

                // Find parent directory and prefix
                if let Some(last_slash) = expanded_path.rfind('/') {
                    let parent = if last_slash == 0 { "/" } else { &expanded_path[..last_slash] };
                    let prefix = &expanded_path[last_slash + 1..];

                    if let Ok(entries) = std::fs::read_dir(parent) {
                        let mut matches: Vec<String> = entries
                            .filter_map(|e| e.ok())
                            .filter_map(|e| {
                                let name = e.file_name().to_string_lossy().to_string();
                                if name.starts_with(prefix) {
                                    let full_path = if parent == "/" {
                                        format!("/{}", name)
                                    } else {
                                        format!("{}/{}", parent, name)
                                    };
                                    // Add trailing slash for directories
                                    if e.path().is_dir() {
                                        Some(format!("{}/", full_path))
                                    } else {
                                        Some(full_path)
                                    }
                                } else {
                                    None
                                }
                            })
                            .collect();

                        matches.sort();
                        if let Some(first_match) = matches.first() {
                            // Keep the ~ prefix if original had it
                            self.search_query = if self.search_query.starts_with('~') {
                                if let Some(home) = std::env::var_os("HOME") {
                                    first_match.replacen(&home.to_string_lossy().to_string(), "~", 1)
                                } else {
                                    first_match.clone()
                                }
                            } else {
                                first_match.clone()
                            };
                            dlog!("  tab complete: '{}'", self.search_query);
                            self.dirty = true;
                            self.request_frame(qh);
                        }
                    }
                }
            }
        } else if event.keysym == Keysym::Return {
            // Open best matching directory/app, or use first search engine if no match
            if !self.search_query.is_empty() {
                // If it looks like a path (contains / or starts with ~), open it directly
                if self.search_query.contains('/') || self.search_query.starts_with('~') {
                    // Expand tilde to home directory
                    let path = if self.search_query.starts_with('~') {
                        if let Some(home) = std::env::var_os("HOME") {
                            self.search_query.replacen('~', &home.to_string_lossy(), 1)
                        } else {
                            self.search_query.clone()
                        }
                    } else {
                        self.search_query.clone()
                    };
                    // Add to zoxide
                    let _ = Command::new("zoxide")
                        .args(["add", &path])
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn();
                    dlog!("  zoxide add: {}", path);
                    // Open with preferred editor (same as zoxide matches)
                    self.open_directory(&path);
                } else {
                    // Use unified search with priority order from config
                    match self.find_best_search_match() {
                        SearchMatch::Folder(dir) => {
                            self.open_directory(&dir);
                        }
                        SearchMatch::Desktop(indices) => {
                            // Launch first matching desktop entry
                            if let Some(&icon_idx) = indices.first() {
                                if let Some(icon) = self.icons.get(icon_idx) {
                                    launch_exec(&icon.exec, &icon.name);
                                }
                            }
                        }
                        SearchMatch::None => {
                            // No match - use selected search engine (defaults to first)
                            let idx = self.hovered_search_engine.unwrap_or(0);
                            if let Some(engine) = self.search_engines.get(idx) {
                                let query = self.search_query.replace(' ', "+");
                                let url = engine.url_template.replace("{}", &query);
                                let _ = Command::new("xdg-open")
                                    .arg(&url)
                                    .stdin(std::process::Stdio::null())
                                    .stdout(std::process::Stdio::null())
                                    .stderr(std::process::Stdio::null())
                                    .spawn();
                                dlog!("  search {} for: {}", engine.name, self.search_query);
                            }
                        }
                    }
                }
                self.exit = true;
            } else if let Some(target) = self.picker_target {
                // Picker is open - select hovered item
                if let Some(visual_idx) = self.picker_hovered {
                    let filtered = self.filtered_icon_indices();
                    let filtered_idx = self.picker_scroll + visual_idx;
                    if filtered_idx < filtered.len() {
                        let icon_idx = filtered[filtered_idx];
                        self.tiles[target] = Some(icon_idx);
                        dlog!("  picker: selected {} for tile {}", self.icons[icon_idx].name, target);
                    }
                }
                self.picker_target = None;
                self.picker_hovered = None;
                self.picker_search.clear();
                self.dirty = true;
                self.request_frame(qh);
            } else if let Some(tile_idx) = self.hovered_tile {
                // No search query, no picker - check tile
                if let Some(Some(icon_idx)) = self.tiles.get(tile_idx) {
                    // Tile has icon - launch it
                    if let Some(icon) = self.icons.get(*icon_idx) {
                        launch_exec(&icon.exec, &icon.name);
                        self.exit = true;
                    }
                } else {
                    // Empty tile - open picker
                    self.refresh_icons_if_needed();
                    dlog!("  picker: opening for tile {}", tile_idx);
                    self.picker_target = Some(tile_idx);
                    self.picker_scroll = 0;
                    self.picker_hovered = Some(0);  // Start with first item selected
                    self.picker_search.clear();
                    self.dirty = true;
                    self.request_frame(qh);
                }
            } else if let Some(di) = self.hovered_dock {
                // Dock item focused via keyboard - launch it.
                if di < self.dock.len() {
                    launch_exec(&self.dock[di].exec, &self.dock[di].name);
                    self.exit = true;
                }
            }
        } else if let Some(c) = event.utf8.as_ref().and_then(|s| s.chars().next()) {
            // Printable character - add to search
            if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' || c == '/' || c == '.' || c == '~' {
                if self.picker_target.is_some() {
                    // Picker is open - filter by name
                    self.picker_search.push(c);
                    self.picker_scroll = 0;  // Reset scroll when search changes
                    self.picker_hovered = Some(0);
                    dlog!("  picker search: '{}'", self.picker_search);
                } else {
                    self.search_query.push(c);
                    if self.search_engines_active() {
                        self.hovered_search_engine = Some(0);
                    } else {
                        self.hovered_search_engine = None;
                    }
                    dlog!("  search: '{}'", self.search_query);
                }
                self.dirty = true;
                self.request_frame(qh);
            }
        }
    }
    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: Modifiers, _: u32) {}
}

impl PointerHandler for App {
    fn pointer_frame(&mut self, conn: &Connection, qh: &QueueHandle<Self>, pointer: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        let t0 = Instant::now();
        let mut needs_redraw = false;
        let event_count = events.len();

        for ev in events {
            if &ev.surface != self.layer.wl_surface() { continue; }
            let ev_pos = (ev.position.0 * self.scale as f64, ev.position.1 * self.scale as f64);

            // Handle pointer enter - set cursor to make it visible
            if let PointerEventKind::Enter { serial } = ev.kind {
                self.pointer_enter_serial = serial;
                if self.cursor_theme.is_none() {
                    self.cursor_theme = CursorTheme::load(conn, self.shm.wl_shm().clone(), 24).ok();
                    dlog!("  cursor theme loaded lazily: {}", self.cursor_theme.is_some());
                }
                if let Some(ref mut cursor_theme) = self.cursor_theme {
                    if let Some(cursor) = cursor_theme.get_cursor("default") {
                        let image = &cursor[0];
                        let (hx, hy) = image.hotspot();
                        let (w, h) = image.dimensions();
                        self.cursor_surface.attach(Some(image), 0, 0);
                        self.cursor_surface.damage_buffer(0, 0, w as i32, h as i32);
                        self.cursor_surface.commit();
                        pointer.set_cursor(serial, Some(&self.cursor_surface), hx as i32, hy as i32);
                        dlog!("  cursor: set via wl_pointer.set_cursor");
                    }
                }
            }

            // If picker is open, handle picker interactions
            if self.picker_target.is_some() {
                match ev.kind {
                    PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                        self.pointer_pos = ev_pos;
                        let new_hovered = self.picker_item_at(ev_pos.0, ev_pos.1);
                        if new_hovered != self.picker_hovered {
                            self.picker_hovered = new_hovered;
                            needs_redraw = true;
                        }
                    }
                    PointerEventKind::Press { button, .. } if button == 0x110 => {
                        needs_redraw |= self.handle_picker_press(ev_pos.0, ev_pos.1);
                    }
                    PointerEventKind::Axis { vertical, .. } => {
                        // Scroll in picker (mouse-only)
                        let scroll_dir = if vertical.absolute > 0.0 { 1 } else if vertical.absolute < 0.0 { -1 } else { 0 };
                        if scroll_dir != 0 {
                            let filtered_len = self.filtered_icon_indices().len();
                            let max_scroll = filtered_len.saturating_sub(Self::PICKER_COLS * Self::PICKER_VISIBLE_ROWS);
                            if scroll_dir > 0 {
                                self.picker_scroll = (self.picker_scroll + Self::PICKER_COLS).min(max_scroll);
                            } else {
                                self.picker_scroll = self.picker_scroll.saturating_sub(Self::PICKER_COLS);
                            }
                            dlog!("  picker: scroll to {}/{}", self.picker_scroll, filtered_len);
                            needs_redraw = true;
                        }
                    }
                    _ => {}
                }
                continue;
            }

            // Normal grid interactions (picker closed)
            match ev.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    self.pointer_pos = ev_pos;
                    let (x, y) = ev_pos;

                    needs_redraw |= self.handle_drag_motion(x, y);

                    // Update grid hover state (pointer-specific: sticky hover)
                    if let Some(tile) = self.tile_at(x, y) {
                        if self.hovered_tile != Some(tile) {
                            self.hovered_tile = Some(tile);
                            needs_redraw = true;
                        }
                    }

                    // Update dock hover state (pointer-only)
                    let new_dock_hovered = self.dock_item_at(x, y);
                    if new_dock_hovered != self.hovered_dock {
                        self.hovered_dock = new_dock_hovered;
                        needs_redraw = true;
                    }

                    // Update search engine hover state (pointer-only)
                    let new_search_engine_hovered = self.search_engine_at(x, y);
                    if new_search_engine_hovered != self.hovered_search_engine {
                        self.hovered_search_engine = new_search_engine_hovered;
                        needs_redraw = true;
                    }
                }
                PointerEventKind::Leave { .. } => {
                    self.hovered_dock = None;
                    self.hovered_search_engine = None;
                    needs_redraw = true;
                }
                PointerEventKind::Press { button, .. } => {
                    if button == 0x110 {
                        needs_redraw |= self.handle_press(ev_pos.0, ev_pos.1);
                    }
                }
                PointerEventKind::Release { button, .. } => {
                    if button == 0x110 {
                        needs_redraw |= self.handle_release(ev_pos.0, ev_pos.1);
                    } else if button == 0x111 { // BTN_RIGHT (mouse-only)
                        if let Some(tile) = self.tile_at(ev_pos.0, ev_pos.1) {
                            self.tiles[tile] = None;
                            dlog!("  removed tile {}", tile);
                            needs_redraw = true;
                        }
                        self.press_start = None;
                    }
                }
                _ => {}
            }
        }

        let t1 = Instant::now();
        if needs_redraw {
            dlog!("pointer_frame: {} events, process {:.2}ms, marking dirty", event_count, (t1 - t0).as_secs_f64() * 1000.0);
            self.dirty = true;
            self.request_frame(qh);
        }
    }
}

impl TouchHandler for App {
    fn down(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_touch::WlTouch, _serial: u32, _time: u32, _surface: wl_surface::WlSurface, _id: i32, position: (f64, f64)) {
        let position = (position.0 * self.scale as f64, position.1 * self.scale as f64);
        dlog!("  touch down at ({:.0}, {:.0})", position.0, position.1);
        let mut needs_redraw = false;

        if self.picker_target.is_some() {
            needs_redraw |= self.handle_picker_press(position.0, position.1);
        } else {
            needs_redraw |= self.handle_press(position.0, position.1);
        }

        if needs_redraw {
            self.dirty = true;
            self.request_frame(qh);
        }
    }

    fn up(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_touch::WlTouch, _serial: u32, _time: u32, _id: i32) {
        dlog!("  touch up");
        // For touch release, use hovered_tile position for drag swap (no position in up event)
        let release_pos = self.pointer_pos;
        let needs_redraw = self.handle_release(release_pos.0, release_pos.1);
        if needs_redraw {
            self.dirty = true;
            self.request_frame(qh);
        }
    }

    fn motion(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_touch::WlTouch, _time: u32, _id: i32, position: (f64, f64)) {
        let position = (position.0 * self.scale as f64, position.1 * self.scale as f64);
        if self.handle_drag_motion(position.0, position.1) {
            self.dirty = true;
            self.request_frame(qh);
        }
    }

    fn cancel(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_touch::WlTouch) {
        dlog!("  touch cancel");
        self.press_start = None;
        self.drag_from = None;
    }

    fn shape(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_touch::WlTouch, _id: i32, _major: f64, _minor: f64) {}
    fn orientation(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_touch::WlTouch, _id: i32, _orientation: f64) {}
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm { &mut self.shm }
}

delegate_compositor!(App);
delegate_output!(App);
delegate_shm!(App);
delegate_seat!(App);
delegate_keyboard!(App);
delegate_pointer!(App);
delegate_touch!(App);
delegate_layer!(App);
delegate_registry!(App);

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState { &mut self.registry_state }
    registry_handlers![OutputState, SeatState];
}
