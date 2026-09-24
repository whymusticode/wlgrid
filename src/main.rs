use std::borrow::Cow;
use std::env;
use std::process::Command;
use std::time::Instant;
use std::thread;
use serde::{Deserialize, Serialize};
use ab_glyph::{Font, ScaleFont, point};
mod gpu_gl;
use gpu_gl::{GlCmd, GlRect, GlSprite};

// Pure-logic helpers shared with benches
use wlgrid::{dlog, Icon, IconCache, Fonts, load_entries, make_placeholder_icon};

// ── config ──

#[derive(Deserialize, Default)]
#[serde(default)]
struct Config {
    width: Option<usize>,
    height: Option<usize>,
    icon_size: Option<f32>,
    extra_entries: ExtraEntries,
    start_col: Option<usize>,
    start_row: Option<usize>,
    tile_color: Option<String>,
    dim: Option<f32>,
    corner_radius: Option<u32>,
    accent_hue_delta: Option<f32>,
    accent_amount: Option<f32>,
    panel_color: Option<String>,
    panel_alpha: Option<f32>,
    tile_alpha: Option<f32>,
    border_color: Option<String>,
    border_alpha: Option<f32>,
    show_tile_outlines: Option<bool>,
    use_cache: Option<bool>,
}

/// `[extra_entries]`: launchable entries defined in the config rather than by
/// a .desktop file, one `icon = command` per line of `options`. They join the
/// normal entry list (picker, search) named by their command, with the icon
/// text (typically a Nerd Font glyph) rendered as the icon.
#[derive(Deserialize, Default)]
#[serde(default)]
struct ExtraEntries {
    options: String,
}

fn parse_extra_entries(options: &str) -> Vec<(String, String)> {
    options
        .lines()
        .filter_map(|line| {
            let (name, exec) = line.trim().split_once('=')?;
            Some((name.trim().to_string(), exec.trim().to_string()))
        })
        .collect()
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

fn load_config() -> Config {
    let home = env::var("HOME").unwrap_or_default();
    let user_path = format!("{home}/.config/wlgrid/wlgrid.toml");
    if !std::path::Path::new(&user_path).exists() {
        // Older versions named it config.toml; move it rather than shadowing
        // it with a fresh default.
        let legacy = format!("{home}/.config/wlgrid/config.toml");
        if std::fs::rename(&legacy, &user_path).is_err() {
            let _ = std::fs::create_dir_all(format!("{home}/.config/wlgrid"));
            let _ = std::fs::write(&user_path, include_str!("../wlgrid.toml.default"));
        }
    }

    for path in [user_path.as_str(), "/etc/wlgrid/wlgrid.toml"] {
        if let Ok(content) = std::fs::read_to_string(path) {
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
    /// Desktop entry name per tile (names survive entry list reordering).
    tiles: Vec<Option<String>>,
}

fn state_path() -> String {
    format!("{}/.config/wlgrid/state.json", env::var("HOME").unwrap_or_default())
}

fn load_state() -> AppState {
    // Older versions kept state in ~/.cache; fall back so layouts survive the move.
    let legacy = format!("{}/.cache/wlgrid/state.json", env::var("HOME").unwrap_or_default());
    [state_path(), legacy].iter()
        .find_map(|p| serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok())
        .unwrap_or_default()
}

fn save_state(tiles: Vec<Option<String>>) {
    let path = state_path();
    if let Some(parent) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(&AppState { tiles }) {
        match std::fs::write(&path, json) {
            Ok(()) => dlog!("  saved state to {}", path),
            Err(e) => dlog!("  failed to save state: {}", e),
        }
    }
}

// ── text rendering ──

/// Rasterise `text` into a tight RGBA coverage buffer (white pixels, alpha =
/// glyph coverage) for upload as a GL texture, tinted at draw time. The internal
/// baseline sits `ascent` px from the top (returned as the 4th element), so a
/// caller wanting a baseline at `baseline_y` draws the buffer at
/// `y = baseline_y - ascent`. Returns None for empty / zero-width text.
fn rasterize_text(fonts: &Fonts, text: &str, size: f32) -> Option<(Vec<u8>, u32, u32, i32)> {
    if size <= 0.0 {
        return None;
    }
    let w = fonts.text_width(text, size).ceil() as u32;
    if w == 0 {
        return None;
    }
    let base = fonts.text.as_scaled(size);
    let ascent = base.ascent();
    let h = (base.ascent() - base.descent()).ceil().max(1.0) as u32;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut pen_x = 0.0f32;
    for c in text.chars() {
        let font = fonts.for_char(c);
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
                let idx = ((py as u32 * w + px as u32) * 4) as usize;
                // Glyphs shouldn't overlap, but if they do keep the strongest
                // coverage rather than letting later glyphs erase earlier ones.
                if alpha > buf[idx + 3] {
                    buf[idx..idx + 4].copy_from_slice(&[0xFF, 0xFF, 0xFF, alpha]);
                }
            });
        }
        pen_x += font.as_scaled(size).h_advance(gid);
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

const NONE: [f32; 4] = [0.0; 4];
const WHITE: [u8; 4] = [0xFF; 4];

#[allow(clippy::too_many_arguments)]
fn rect(x: i32, y: i32, w: i32, h: i32, radius: f32, fill: [f32; 4], border: [f32; 4], border_w: f32) -> GlCmd<'static> {
    GlCmd::Rect(GlRect { x, y, w, h, radius, fill, border, border_w })
}

/// Whether (x, y) lies inside the rect (rx, ry, rw, rh).
fn hit((rx, ry, rw, rh): (i32, i32, i32, i32), x: f64, y: f64) -> bool {
    x >= rx as f64 && x < (rx + rw) as f64 && y >= ry as f64 && y < (ry + rh) as f64
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

// ── launching ──

/// Expand the `$USER` placeholder to the current username. This is a plain
/// string substitution done before spawning (no shell involved), so it only
/// ever recognizes this one literal token - not general env var expansion.
fn expand_placeholders(arg: &str) -> String {
    if arg.contains("$USER") {
        let user = env::var("USER").unwrap_or_default();
        arg.replace("$USER", &user)
    } else {
        arg.to_string()
    }
}

/// Parse a shell-like command string, respecting quoted arguments.
fn parse_exec_args(exec: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut chars = exec.chars().peekable();
    // Desktop entry field codes (%f, %F, %u, %U, etc.) are dropped.
    let mut flush = |current: &mut String| {
        if !current.is_empty() && (!current.starts_with('%') || current.len() != 2) {
            args.push(expand_placeholders(current));
        }
        current.clear();
    };

    while let Some(c) = chars.next() {
        match c {
            '\'' if !in_double_quote => in_single_quote = !in_single_quote,
            '"' if !in_single_quote => in_double_quote = !in_double_quote,
            '\\' if !in_single_quote => {
                // Handle escape sequences
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            ' ' | '\t' if !in_single_quote && !in_double_quote => flush(&mut current),
            _ => current.push(c),
        }
    }
    flush(&mut current);
    args
}

/// Launch an application from its Exec string (without shell - secure)
fn launch_exec(exec: &str, name: &str) {
    let args = parse_exec_args(exec);
    let Some((program, cmd_args)) = args.split_first() else {
        dlog!("  launch: empty command for {}", name);
        return;
    };
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
    // `-t`: print a timestamped log of startup (and everything after) to stderr.
    if env::args().any(|a| a == "-t") {
        wlgrid::enable_log();
    }
    dlog!("wlgrid starting");

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
    dlog!("instance lock acquired");

    // ── connect to wayland ──
    let conn = Connection::connect_to_env().unwrap();
    dlog!("connected to Wayland display {}", env::var("WAYLAND_DISPLAY").unwrap_or_default());
    let (globals, mut event_queue) = registry_queue_init(&conn).unwrap();
    let qh = event_queue.handle();
    dlog!("registry roundtrip done ({} globals)", globals.contents().clone_list().len());

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor missing");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("layer shell missing");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm missing");
    dlog!("bound compositor, layer shell, shm");

    let config = load_config();
    let grid_w: usize = config.width.unwrap_or(6).max(1);
    let grid_h: usize = config.height.unwrap_or(4).max(1);
    // Icon pixel size. Config stores it as f32 for user convenience; we round to u32.
    let icon_size: u32 = config.icon_size
        .map(|s| s.round().max(1.0) as u32)
        .unwrap_or(wlgrid::DEFAULT_ICON_SIZE);
    let tile_size = ((icon_size as f32) * (64.0 / wlgrid::DEFAULT_ICON_SIZE as f32))
        .round()
        .max(icon_size as f32 + 8.0) as i32;
    let tile_gap = ((icon_size as f32) * (8.0 / wlgrid::DEFAULT_ICON_SIZE as f32))
        .round()
        .max(4.0) as i32;

    // ── create layer surface ──
    let surface = compositor.create_surface(&qh);
    let layer = layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("wlgrid"), None);
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    layer.set_exclusive_zone(-1); // don't push other surfaces
    layer.commit();
    dlog!("layer surface committed");

    // Fonts are needed before entries so icon-less entries can render their name.
    let Some(fonts) = Fonts::load() else {
        eprintln!("wlgrid: no usable font found; install a sans + a Nerd Font");
        return;
    };
    dlog!("fonts loaded");
    let use_cache = config.use_cache.unwrap_or(true);
    let mut icon_cache = if use_cache { IconCache::load(icon_size) } else { IconCache::new(icon_size) };
    let extra_entries = parse_extra_entries(&config.extra_entries.options);
    let icons = load_entries(icon_size, &fonts, &mut icon_cache, &extra_entries);
    dlog!("{} entries loaded", icons.len());

    let mut app = App {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        shm,
        exit: false,
        first_configure: true,
        width: 0,
        height: 0,
        layer,
        keyboard: None,
        pointer: None,
        touch: None,
        grid_w,
        grid_h,
        tile_size,
        tile_gap,
        pad: 8,
        scale: 1,
        tiles: Vec::new(),
        icons,
        icon_size,
        icon_cache,
        cache_saver: None,
        use_cache,
        extra_entries,
        pointer_pos: (0.0, 0.0),
        hovered_tile: Some({
            let col = config.start_col.unwrap_or(grid_w / 2).min(grid_w - 1);
            let row = config.start_row.unwrap_or(grid_h / 2).min(grid_h - 1);
            row * grid_w + col
        }),
        press_start: None,
        drag_from: None,
        dirty: true,
        frame_pending: false,
        first_frame_presented: false,
        icons_at_first_frame: 0,
        picker_target: None,
        picker_scroll: 0,
        picker_hovered: None,
        picker_search: String::new(),
        search_query: String::new(),
        search_sel: 0,
        fonts,
        theme: Theme::from_config(&config),
        gl_renderer: None,
        cursor_theme: None, // loaded lazily on first pointer enter
        cursor_surface: compositor.create_surface(&qh),
        probe_draw_ms: Vec::new(),
    };
    app.tiles = app.tiles_from_names(&load_state().tiles);
    dlog!("state restored ({} tiles filled), entering event loop", app.tiles.iter().flatten().count());

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

    save_state(app.tile_names());
    if let Some(h) = app.cache_saver.take() {
        let _ = h.join();
    }
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
    let tiles_filled = app.tiles.iter().filter(|t| t.is_some()).count();
    let first_frame_ms = time_to_first_frame
        .map(|d| d.as_millis())
        .unwrap_or(0);

    // Count how many icons are the procedural "?" placeholder vs real.
    // `make_placeholder_icon` is deterministic, so any icon with matching
    // pixels came from the fallback path rather than a real icon file.
    let placeholder_pixels = make_placeholder_icon(app.icon_size);
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
    if !app.icons.is_empty() && app.icons_at_first_frame == 0 {
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

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const DRAG_THRESHOLD: f64 = 5.0;

// Picker layout, in unscaled px.
const PICKER_ITEM_WIDTH: i32 = 80;   // wide enough for a short name
const PICKER_ITEM_HEIGHT: i32 = 72;  // icon + name
const PICKER_ITEM_GAP: i32 = 8;
const PICKER_COLS: usize = 6;
const PICKER_VISIBLE_ROWS: usize = 5;
const PICKER_VISIBLE: usize = PICKER_COLS * PICKER_VISIBLE_ROWS;
const PICKER_SEARCH_HEIGHT: i32 = 32;

// Type-to-launch results box, in unscaled px.
const SEARCH_MAX_RESULTS: usize = 8;
const SEARCH_HEADER_H: f32 = 44.0;
const SEARCH_ROW_H: f32 = 36.0;

struct App {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    exit: bool,
    first_configure: bool,
    // Full-screen surface size, in buffer pixels.
    width: i32,
    height: i32,
    layer: LayerSurface,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    touch: Option<wl_touch::WlTouch>,
    // Grid layout, in unscaled px (multiplied by `scale` wherever used).
    grid_w: usize,
    grid_h: usize,
    tile_size: i32,
    tile_gap: i32,
    pad: i32,
    scale: i32,
    // Which entry each tile shows (index into `icons`)
    tiles: Vec<Option<usize>>,
    icons: Vec<Icon>,
    icon_size: u32,
    icon_cache: IconCache,
    cache_saver: Option<thread::JoinHandle<()>>,
    use_cache: bool,
    extra_entries: Vec<(String, String)>,
    // Input state (shared by pointer and touch)
    pointer_pos: (f64, f64),
    hovered_tile: Option<usize>,  // shared by mouse, touch, and keyboard
    press_start: Option<(f64, f64, usize)>, // (x, y, tile_index) when press/touch began
    drag_from: Option<usize>,
    // Rendering state
    dirty: bool,
    frame_pending: bool,
    first_frame_presented: bool,  // set true when the first frame callback fires (probe)
    icons_at_first_frame: usize,  // snapshot of icons.len() at first frame time (probe)
    // Picker state
    picker_target: Option<usize>,    // which tile we're picking for (None = closed)
    picker_scroll: usize,            // index of the first visible filtered entry
    picker_hovered: Option<usize>,   // which picker item is hovered (visual index)
    picker_search: String,           // search filter for picker
    // Type-to-launch state
    search_query: String,
    search_sel: usize,               // selected row in the results list
    // Required: wlgrid exits at startup if no usable font is found.
    fonts: Fonts,
    theme: Theme,
    gl_renderer: Option<gpu_gl::GlRenderer>,
    cursor_theme: Option<CursorTheme>,
    cursor_surface: wl_surface::WlSurface,
    probe_draw_ms: Vec<f32>,
}

impl App {
    fn record_draw_ms(&mut self, ms: f32) {
        if self.probe_draw_ms.len() >= 2048 {
            let _ = self.probe_draw_ms.remove(0);
        }
        self.probe_draw_ms.push(ms);
    }

    /// Mark the frame dirty and make sure a frame callback will redraw it.
    fn redraw(&mut self, qh: &QueueHandle<Self>) {
        self.dirty = true;
        if !self.frame_pending {
            self.layer.wl_surface().frame(qh, self.layer.wl_surface().clone());
            self.layer.commit();
            self.frame_pending = true;
        }
    }

    fn draw(&mut self, qh: &QueueHandle<Self>) {
        let t0 = Instant::now();
        self.dirty = false;
        let Some(mut renderer) = self.gl_renderer.take() else { return };
        let cmds = self.frame_cmds();
        if let Err(e) = renderer.render(self.theme.dim, &cmds) {
            eprintln!("wlgrid: gl render failed: {e}");
        }
        drop(cmds);
        self.gl_renderer = Some(renderer);

        self.layer.wl_surface().damage_buffer(0, 0, self.width, self.height);
        self.layer.wl_surface().frame(qh, self.layer.wl_surface().clone());
        self.layer.commit();
        self.frame_pending = true;
        self.record_draw_ms(t0.elapsed().as_secs_f64() as f32 * 1000.0);
    }

    /// Sprite for entry `idx`. Keyed by index so its texture is uploaded once
    /// and reused; the GPU scales it to `size` for free via LINEAR sampling.
    fn icon_cmd(&self, idx: usize, x: i32, y: i32, size: i32) -> GlCmd<'_> {
        let src = self.icon_size as i32;
        GlCmd::Sprite(GlSprite {
            key: idx as u64 + 1, x, y, w: size, h: size,
            pixels: Cow::Borrowed(&self.icons[idx].pixels), src_w: src, src_h: src, tint: [1.0; 4],
        })
    }

    /// Rasterise `text` as a tinted sprite whose baseline lands at `baseline_y`.
    #[allow(clippy::too_many_arguments)]
    fn text_cmd(&self, cmds: &mut Vec<GlCmd<'_>>, x: i32, baseline_y: i32, text: &str, size: f32, color: [u8; 4]) {
        if let Some((buf, w, h, ascent)) = rasterize_text(&self.fonts, text, size) {
            cmds.push(GlCmd::Sprite(GlSprite {
                key: 0, x, y: baseline_y - ascent, w: w as i32, h: h as i32,
                pixels: Cow::Owned(buf), src_w: w as i32, src_h: h as i32, tint: rgba4(color),
            }));
        }
    }

    /// Build the frame as an ordered (z-order) list of GL primitives. All
    /// scaling happens here at draw time — source icons stay at their base
    /// resolution and only the on-screen ones are ever touched.
    fn frame_cmds(&self) -> Vec<GlCmd<'_>> {
        let t = self.theme;
        let s = self.scale as f32;
        let (ox, oy) = self.grid_offset();
        let (cw, ch) = self.content_size();
        let eff = (self.icon_size as f32 * s).round().max(1.0) as i32;
        let mut c: Vec<GlCmd> = Vec::new();

        // Panel backing the whole grid.
        c.push(rect(ox, oy, cw, ch, t.radius * s, rgba3(t.panel, t.panel_a), rgba3(t.border, (t.border_a * 0.9).min(1.0)), s));

        // Grid tiles + their icons.
        let hover = accent_delta(t.tile, t.tile_a, t.accent_hue_delta, t.accent_amount);
        for i in 0..self.tiles.len() {
            let (tx, ty, ts, _) = self.tile_rect(i);
            let (fill, fa) = if self.hovered_tile == Some(i) { hover } else { (t.tile, t.tile_a) };
            let outline = if t.show_tile_outlines { s } else { 0.0 };
            c.push(rect(tx, ty, ts, ts, t.radius * s, rgba3(fill, fa), rgba3(t.border, t.border_a), outline));
            if let Some(idx) = self.tiles[i].filter(|_| self.drag_from != Some(i)) {
                c.push(self.icon_cmd(idx, tx + (ts - eff) / 2, ty + (ts - eff) / 2, eff));
            }
        }

        // Drag overlay: drop-target outline + the dragged icon under the cursor.
        if let Some(from) = self.drag_from {
            if let Some(to) = self.hovered_tile.filter(|&to| to != from) {
                let (tx, ty, ts, _) = self.tile_rect(to);
                c.push(rect(tx, ty, ts, ts, t.radius * s, NONE, [0.0, 1.0, 0.0, 1.0], (2.0 * s).max(1.0)));
            }
            if let Some(idx) = self.tiles[from] {
                let (px, py) = (self.pointer_pos.0 as i32, self.pointer_pos.1 as i32);
                c.push(self.icon_cmd(idx, px - eff / 2, py - eff / 2, eff));
            }
        }

        if self.picker_target.is_some() {
            self.picker_cmds(&mut c, eff);
        }
        if !self.search_query.is_empty() {
            self.search_cmds(&mut c);
        }
        c
    }

    fn picker_cmds<'a>(&'a self, c: &mut Vec<GlCmd<'a>>, eff: i32) {
        let s = self.scale as f32;
        let px_ = |v: f32| (v * s) as i32;
        c.push(rect(0, 0, self.width, self.height, 0.0, [0.0, 0.0, 0.0, 0.5], NONE, 0.0));
        let (px, py, pw, ph) = self.picker_rect();
        c.push(rect(px, py, pw, ph, self.theme.radius * s, rgba4([0x1A, 0x1A, 0x1A, 0xFF]), rgba4([0x55, 0x55, 0x55, 0xFF]), 2.0 * s));

        let box_y = py + px_(8.0);
        c.push(rect(px + px_(8.0), box_y, pw - px_(16.0), px_(PICKER_SEARCH_HEIGHT as f32), 4.0 * s,
            rgba4([0x2D, 0x2D, 0x2D, 0xFF]), rgba4([0x55, 0x55, 0x55, 0xFF]), 1.0));
        let (query, color) = if self.picker_search.is_empty() {
            ("Type to search...", [0x88, 0x88, 0x88, 0xFF])
        } else {
            (self.picker_search.as_str(), WHITE)
        };
        self.text_cmd(c, px + px_(12.0), box_y + px_(22.0), query, 16.0 * s, color);

        let name_size = 10.0 * s;
        let max_chars = 10;
        let mut tooltip = None;
        let filtered = self.filtered_icon_indices();
        for (vis, &idx) in filtered.iter().skip(self.picker_scroll).take(PICKER_VISIBLE).enumerate() {
            let (ix, iy, iw, ih) = self.picker_item_rect(vis);
            let hovered = self.picker_hovered == Some(vis);
            let bg = if hovered { [0x46, 0x46, 0x46, 0xFF] } else { [0x2D, 0x2D, 0x2D, 0xFF] };
            c.push(rect(ix, iy, iw, ih, 4.0 * s, rgba4(bg), NONE, 0.0));
            c.push(self.icon_cmd(idx, ix + (iw - eff) / 2, iy + px_(4.0), eff));

            let name = &self.icons[idx].name;
            let truncated = name.chars().count() > max_chars;
            let label: String = if truncated {
                format!("{}…", name.chars().take(max_chars - 1).collect::<String>())
            } else {
                name.clone()
            };
            let tw = self.fonts.text_width(&label, name_size) as i32;
            self.text_cmd(c, ix + (iw - tw) / 2, iy + ih - px_(4.0), &label, name_size, [0xCC, 0xCC, 0xCC, 0xFF]);
            if hovered && truncated {
                tooltip = Some((name, ix, iy, iw));
            }
        }

        // Full name of a hovered, truncated entry.
        if let Some((name, ix, iy, iw)) = tooltip {
            let size = 12.0 * s;
            let padding = px_(6.0);
            let tooltip_w = self.fonts.text_width(name, size) as i32 + padding * 2;
            let tooltip_h = px_(20.0);
            let tx = (ix + (iw - tooltip_w) / 2).max(px + 4).min(px + pw - tooltip_w - 4);
            let ty = (iy - tooltip_h - px_(4.0)).max(py + 4);
            c.push(rect(tx, ty, tooltip_w, tooltip_h, 4.0 * s, rgba4([0x00, 0x00, 0x00, 0xEE]), rgba4([0x88, 0x88, 0x88, 0xFF]), 1.0));
            self.text_cmd(c, tx + padding, ty + px_(15.0), name, size, WHITE);
        }
    }

    fn search_cmds<'a>(&'a self, c: &mut Vec<GlCmd<'a>>) {
        let s = self.scale as f32;
        let px_ = |v: f32| (v * s) as i32;
        let matches = self.search_matches();
        let (bx, by, bw, bh) = self.search_rect(matches.len());
        c.push(rect(bx, by, bw, bh, 6.0 * s, rgba4([0x18, 0x14, 0x10, 0xEE]), rgba4([0x60, 0x60, 0x80, 0xFF]), s.max(1.0)));
        let color = if matches.is_empty() { [0xCC, 0xCC, 0xCC, 0xFF] } else { WHITE };
        self.text_cmd(c, bx + px_(16.0), by + px_(30.0), &self.search_query, 24.0 * s, color);

        let icon = px_(28.0);
        for (i, &idx) in matches.iter().enumerate() {
            let (rx, ry, rw, rh) = self.search_row_rect(i);
            let selected = i == self.search_sel;
            if selected {
                c.push(rect(rx, ry, rw, rh, 4.0 * s, rgba4([0x46, 0x46, 0x46, 0xFF]), NONE, 0.0));
            }
            c.push(self.icon_cmd(idx, rx + px_(6.0), ry + (rh - icon) / 2, icon));
            let color = if selected { WHITE } else { [0xBB, 0xBB, 0xBB, 0xFF] };
            self.text_cmd(c, rx + px_(44.0), ry + px_(24.0), &self.icons[idx].name, 16.0 * s, color);
        }
    }

    // ── layout ──

    /// Size of the grid panel, in buffer px.
    fn content_size(&self) -> (i32, i32) {
        let s = self.scale;
        let span = |n: usize| (2 * self.pad + n as i32 * self.tile_size + (n as i32 - 1) * self.tile_gap) * s;
        (span(self.grid_w), span(self.grid_h))
    }

    /// Top-left of the grid panel, centred on the full-screen surface.
    fn grid_offset(&self) -> (i32, i32) {
        let (cw, ch) = self.content_size();
        ((self.width - cw) / 2, (self.height - ch) / 2)
    }

    fn tile_rect(&self, index: usize) -> (i32, i32, i32, i32) {
        let s = self.scale;
        let (ox, oy) = self.grid_offset();
        let (col, row) = ((index % self.grid_w) as i32, (index / self.grid_w) as i32);
        let step = (self.tile_size + self.tile_gap) * s;
        let ts = self.tile_size * s;
        (ox + self.pad * s + col * step, oy + self.pad * s + row * step, ts, ts)
    }

    fn tile_at(&self, x: f64, y: f64) -> Option<usize> {
        (0..self.tiles.len()).find(|&i| hit(self.tile_rect(i), x, y))
    }

    fn picker_rect(&self) -> (i32, i32, i32, i32) {
        let s = self.scale;
        let (cols, rows) = (PICKER_COLS as i32, PICKER_VISIBLE_ROWS as i32);
        let pw = (16 + cols * PICKER_ITEM_WIDTH + (cols - 1) * PICKER_ITEM_GAP) * s;
        let ph = (16 + PICKER_SEARCH_HEIGHT + 8 + rows * PICKER_ITEM_HEIGHT + (rows - 1) * PICKER_ITEM_GAP) * s;
        ((self.width - pw) / 2, (self.height - ph) / 2, pw, ph)
    }

    fn picker_item_rect(&self, index: usize) -> (i32, i32, i32, i32) {
        let s = self.scale;
        let (px, py, _, _) = self.picker_rect();
        let (col, row) = ((index % PICKER_COLS) as i32, (index / PICKER_COLS) as i32);
        let x = px + (8 + col * (PICKER_ITEM_WIDTH + PICKER_ITEM_GAP)) * s;
        let y = py + (8 + PICKER_SEARCH_HEIGHT + 8 + row * (PICKER_ITEM_HEIGHT + PICKER_ITEM_GAP)) * s;
        (x, y, PICKER_ITEM_WIDTH * s, PICKER_ITEM_HEIGHT * s)
    }

    /// Visual index of the picker item under (x, y).
    fn picker_item_at(&self, x: f64, y: f64) -> Option<usize> {
        if !hit(self.picker_rect(), x, y) {
            return None;
        }
        let shown = self.filtered_icon_indices().len().saturating_sub(self.picker_scroll).min(PICKER_VISIBLE);
        (0..shown).find(|&i| hit(self.picker_item_rect(i), x, y))
    }

    /// Largest scroll offset; kept a multiple of PICKER_COLS so columns line up.
    fn picker_max_scroll(&self) -> usize {
        self.filtered_icon_indices().len().saturating_sub(PICKER_VISIBLE).div_ceil(PICKER_COLS) * PICKER_COLS
    }

    fn search_rect(&self, rows: usize) -> (i32, i32, i32, i32) {
        let s = self.scale as f32;
        let w = (420.0 * s).max(self.fonts.text_width(&self.search_query, 24.0 * s) + 32.0 * s) as i32;
        let h = ((SEARCH_HEADER_H + rows as f32 * SEARCH_ROW_H + 4.0) * s) as i32;
        ((self.width - w) / 2, self.grid_offset().1 + (8.0 * s) as i32, w, h)
    }

    fn search_row_rect(&self, i: usize) -> (i32, i32, i32, i32) {
        let s = self.scale as f32;
        let (bx, by, bw, _) = self.search_rect(0);
        let y = by + ((SEARCH_HEADER_H + i as f32 * SEARCH_ROW_H) * s) as i32;
        (bx + (4.0 * s) as i32, y, bw - (8.0 * s) as i32, (SEARCH_ROW_H * s) as i32)
    }

    fn search_row_at(&self, x: f64, y: f64) -> Option<usize> {
        if self.search_query.is_empty() {
            return None;
        }
        (0..self.search_matches().len()).find(|&i| hit(self.search_row_rect(i), x, y))
    }

    // ── entries / tiles ──

    fn tile_names(&self) -> Vec<Option<String>> {
        self.tiles.iter().map(|t| t.map(|idx| self.icons[idx].name.clone())).collect()
    }

    fn tiles_from_names(&self, names: &[Option<String>]) -> Vec<Option<usize>> {
        (0..self.grid_w * self.grid_h)
            .map(|i| {
                let name = names.get(i)?.as_ref()?;
                self.icons.iter().position(|icon| &icon.name == name)
            })
            .collect()
    }

    /// Re-scan desktop entries (cheap: icon pixels come from the cache) so
    /// apps installed while we're running show up in the picker.
    fn reload_entries(&mut self) {
        let names = self.tile_names();
        let icons = load_entries(self.icon_size, &self.fonts, &mut self.icon_cache, &self.extra_entries);
        let changed = icons.len() != self.icons.len()
            || icons.iter().zip(&self.icons).any(|(a, b)| a.name != b.name || a.pixels != b.pixels);
        if changed {
            dlog!("  entries changed, reloaded {} entries", icons.len());
            self.icons = icons;
            self.tiles = self.tiles_from_names(&names);
            // Sprite textures are keyed by entry index, which just shifted.
            if let Some(r) = self.gl_renderer.as_mut() {
                r.clear_textures();
            }
        }
        self.save_cache_if_dirty();
    }

    /// Write the icon cache in the background if lookups added to it.
    fn save_cache_if_dirty(&mut self) {
        if !self.use_cache || !self.icon_cache.dirty {
            return;
        }
        self.icon_cache.dirty = false;
        if let Some(h) = self.cache_saver.take() {
            let _ = h.join();
        }
        let cache = self.icon_cache.clone();
        self.cache_saver = Some(thread::spawn(move || cache.save()));
    }

    /// Indices of entries matching the picker filter.
    fn filtered_icon_indices(&self) -> Vec<usize> {
        let query = self.picker_search.to_lowercase();
        (0..self.icons.len()).filter(|&i| self.icons[i].name_lower.contains(&query)).collect()
    }

    /// Entries matching the type-to-launch query: prefix matches first, then
    /// substring matches, capped at SEARCH_MAX_RESULTS.
    fn search_matches(&self) -> Vec<usize> {
        let query = self.search_query.to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        let (mut prefix, mut rest) = (Vec::new(), Vec::new());
        for (i, icon) in self.icons.iter().enumerate() {
            if icon.name_lower.starts_with(&query) {
                prefix.push(i);
            } else if icon.name_lower.contains(&query) {
                rest.push(i);
            }
        }
        prefix.extend(rest);
        prefix.truncate(SEARCH_MAX_RESULTS);
        prefix
    }

    // ── actions ──

    fn launch(&mut self, idx: usize) {
        launch_exec(&self.icons[idx].exec, &self.icons[idx].name);
        self.exit = true;
    }

    fn open_picker(&mut self, tile: usize, hovered: Option<usize>) {
        self.reload_entries();
        dlog!("  picker: opening for tile {}", tile);
        self.picker_target = Some(tile);
        self.picker_scroll = 0;
        self.picker_hovered = hovered;
        self.picker_search.clear();
    }

    fn close_picker(&mut self) {
        self.picker_target = None;
        self.picker_hovered = None;
        self.picker_search.clear();
    }

    /// Assign the picker item at visual index `vis` to the target tile, then close.
    fn picker_choose(&mut self, vis: Option<usize>) {
        let idx = vis.and_then(|v| self.filtered_icon_indices().get(self.picker_scroll + v).copied());
        if let (Some(idx), Some(target)) = (idx, self.picker_target) {
            self.tiles[target] = Some(idx);
            dlog!("  picker: assigned {} to tile {}", self.icons[idx].name, target);
        }
        self.close_picker();
    }

    /// Apply `edit` to whichever query has focus (picker filter or launcher search).
    fn edit_query(&mut self, edit: impl FnOnce(&mut String)) {
        if self.picker_target.is_some() {
            edit(&mut self.picker_search);
            self.picker_scroll = 0;
            self.picker_hovered = Some(0);
        } else {
            edit(&mut self.search_query);
            self.search_sel = 0;
        }
    }

    /// Arrow-key navigation within whichever view has focus: search results,
    /// the picker (scrolling as needed), or the grid. Returns true on change.
    fn navigate(&mut self, dx: i32, dy: i32) -> bool {
        if !self.search_query.is_empty() {
            let n = self.search_matches().len() as i32;
            let sel = (self.search_sel as i32 + dy).clamp(0, (n - 1).max(0)) as usize;
            return std::mem::replace(&mut self.search_sel, sel) != sel;
        }
        if self.picker_target.is_some() {
            let len = self.filtered_icon_indices().len() as i32;
            let Some(vis) = self.picker_hovered else {
                self.picker_hovered = (len > 0).then_some(0);
                return len > 0;
            };
            let cols = PICKER_COLS as i32;
            let col = vis as i32 % cols + dx;
            let next = (self.picker_scroll + vis) as i32 + dx + dy * cols;
            if col < 0 || col >= cols || next < 0 || next >= len {
                return false;
            }
            let next = next as usize;
            if next < self.picker_scroll {
                self.picker_scroll -= PICKER_COLS;
            } else if next >= self.picker_scroll + PICKER_VISIBLE {
                self.picker_scroll += PICKER_COLS;
            }
            self.picker_hovered = Some(next - self.picker_scroll);
            return true;
        }
        let Some(idx) = self.hovered_tile else { return false };
        let col = (idx % self.grid_w) as i32 + dx;
        let row = (idx / self.grid_w) as i32 + dy;
        if col < 0 || row < 0 || col >= self.grid_w as i32 || row >= self.grid_h as i32 {
            return false;
        }
        self.hovered_tile = Some(row as usize * self.grid_w + col as usize);
        true
    }

    /// Enter: launch the selected search result, confirm the picker, or
    /// launch / fill the focused tile.
    fn activate(&mut self) -> bool {
        if !self.search_query.is_empty() {
            if let Some(&idx) = self.search_matches().get(self.search_sel) {
                self.launch(idx);
            }
        } else if self.picker_target.is_some() {
            self.picker_choose(self.picker_hovered);
        } else if let Some(tile) = self.hovered_tile {
            match self.tiles[tile] {
                Some(idx) => self.launch(idx),
                None => self.open_picker(tile, Some(0)),
            }
        }
        true
    }

    /// Press (pointer button or touch down). Returns true if a redraw is needed.
    fn handle_press(&mut self, x: f64, y: f64) -> bool {
        if self.picker_target.is_some() {
            self.picker_choose(self.picker_item_at(x, y));
            return true;
        }
        if let Some(row) = self.search_row_at(x, y) {
            self.launch(self.search_matches()[row]);
        } else if let Some(tile) = self.tile_at(x, y) {
            self.press_start = Some((x, y, tile));
            self.hovered_tile = Some(tile);
            return true;
        } else {
            let (ox, oy) = self.grid_offset();
            let (cw, ch) = self.content_size();
            if !hit((ox, oy, cw, ch), x, y) {
                dlog!("  closed (pressed outside grid)");
                self.exit = true;
            }
        }
        false
    }

    /// Release (pointer button or touch up). Returns true if a redraw is needed.
    fn handle_release(&mut self, x: f64, y: f64) -> bool {
        if let Some(from) = self.drag_from.take() {
            if let Some(to) = self.tile_at(x, y).filter(|&to| to != from) {
                self.tiles.swap(from, to);
                dlog!("  swapped tile {} <-> {}", from, to);
            }
            return true;
        }
        let Some((_, _, tile)) = self.press_start.take() else { return false };
        match self.tiles[tile] {
            Some(idx) => self.launch(idx),
            None => self.open_picker(tile, None),
        }
        true
    }

    /// Check if motion should start a drag. Returns true if a redraw is needed.
    fn handle_drag_motion(&mut self, x: f64, y: f64) -> bool {
        if let Some((px, py, tile)) = self.press_start {
            if (x - px).hypot(y - py) > DRAG_THRESHOLD {
                self.drag_from = Some(tile);
                self.press_start = None;
                dlog!("  drag start from tile {}", tile);
            }
        }
        if self.drag_from.is_none() {
            return false;
        }
        self.pointer_pos = (x, y);
        if let Some(tile) = self.tile_at(x, y) {
            self.hovered_tile = Some(tile);
        }
        true
    }

    /// Pointer moved to (x, y): update hover state. Returns true on change.
    fn pointer_moved(&mut self, x: f64, y: f64) -> bool {
        self.pointer_pos = (x, y);
        if self.picker_target.is_some() {
            let hovered = self.picker_item_at(x, y);
            return std::mem::replace(&mut self.picker_hovered, hovered) != hovered;
        }
        let mut changed = self.handle_drag_motion(x, y);
        // Hover is sticky: moving between tiles over a gap keeps the last tile
        // lit. While dragging, hovered_tile is the drop target (set above).
        if let Some(tile) = self.tile_at(x, y).filter(|_| self.drag_from.is_none()) {
            changed |= self.hovered_tile.replace(tile) != Some(tile);
        }
        if let Some(row) = self.search_row_at(x, y) {
            changed |= std::mem::replace(&mut self.search_sel, row) != row;
        }
        changed
    }

    fn set_cursor(&mut self, conn: &Connection, pointer: &wl_pointer::WlPointer, serial: u32) {
        if self.cursor_theme.is_none() {
            self.cursor_theme = CursorTheme::load(conn, self.shm.wl_shm().clone(), 24).ok();
            dlog!("  cursor theme loaded lazily: {}", self.cursor_theme.is_some());
        }
        let Some(cursor) = self.cursor_theme.as_mut().and_then(|t| t.get_cursor("default")) else { return };
        let image = &cursor[0];
        let (hx, hy) = image.hotspot();
        let (w, h) = image.dimensions();
        self.cursor_surface.attach(Some(image), 0, 0);
        self.cursor_surface.damage_buffer(0, 0, w as i32, h as i32);
        self.cursor_surface.commit();
        pointer.set_cursor(serial, Some(&self.cursor_surface), hx as i32, hy as i32);
    }
}

// ── handler impls ──

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _conn: &Connection, qh: &QueueHandle<Self>, surface: &wl_surface::WlSurface, new_factor: i32) {
        if new_factor < 1 || new_factor == self.scale {
            return;
        }
        dlog!("  scale_factor_changed: {} -> {}", self.scale, new_factor);
        surface.set_buffer_scale(new_factor);
        self.width = self.width / self.scale * new_factor;
        self.height = self.height / self.scale * new_factor;
        self.scale = new_factor;
        // Deliberately do NOT create the renderer here: before the first
        // configure we don't yet know the real surface size, and creating a
        // wl_egl_window at the wrong size then resizing it before its first
        // buffer isn't reliably honored (the first frame lands in the
        // top-left). configure() creates it once, at the correct size.
        if let Some(r) = self.gl_renderer.as_mut() {
            r.resize(self.width, self.height);
        }
        // If we've already presented a frame, set_buffer_scale has just made
        // the currently-attached buffer render at the wrong size (e.g. a 1x
        // buffer shown at 2x lands in the top-left quarter). Repaint a fresh,
        // correctly-sized buffer now rather than committing the stale one via
        // a frame request and waiting for the next frame callback / input event.
        if !self.first_configure && self.gl_renderer.is_some() {
            self.draw(qh);
        } else {
            self.redraw(qh);
        }
    }
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.frame_pending = false;
        if !self.first_frame_presented {
            dlog!("first frame presented");
            self.first_frame_presented = true;
            self.icons_at_first_frame = self.icons.len();
            self.save_cache_if_dirty();
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
        let old_size = (self.width, self.height);
        if cfg.new_size.0 != 0 { self.width = cfg.new_size.0 as i32 * self.scale; }
        if cfg.new_size.1 != 0 { self.height = cfg.new_size.1 as i32 * self.scale; }

        // GL is the only renderer. Created here (not at startup) because it
        // needs the configured surface size.
        dlog!("layer surface configured {}x{}", self.width, self.height);
        if self.gl_renderer.is_none() {
            let (display, surface) = (conn.display().id(), self.layer.wl_surface().id());
            match gpu_gl::GlRenderer::new(display, surface, self.width, self.height) {
                Ok(r) => self.gl_renderer = Some(r),
                Err(e) => eprintln!("wlgrid: gl init failed: {e}"),
            }
            dlog!("EGL + GL renderer initialised");
        }
        if let Some(r) = self.gl_renderer.as_mut() {
            r.resize(self.width, self.height);
        }

        if self.first_configure {
            self.first_configure = false;
            self.draw(qh);
            dlog!("first frame drawn and committed");
        } else if (self.width, self.height) != old_size {
            // A later configure changed our size: repaint immediately,
            // otherwise the stale (now wrong-sized) buffer lingers until the
            // next input event.
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
        let redraw = match event.keysym {
            Keysym::Escape => {
                // Innermost first: clear search, then close picker, then exit.
                if !self.search_query.is_empty() {
                    self.search_query.clear();
                } else if self.picker_target.is_some() {
                    self.close_picker();
                } else {
                    self.exit = true;
                }
                true
            }
            Keysym::BackSpace => {
                self.edit_query(|q| { q.pop(); });
                true
            }
            Keysym::Delete => {
                // Delete key removes the focused tile's entry (same as right-click)
                match self.hovered_tile.filter(|_| self.picker_target.is_none()) {
                    Some(tile) => self.tiles[tile].take().is_some(),
                    None => false,
                }
            }
            Keysym::Left => self.navigate(-1, 0),
            Keysym::Right => self.navigate(1, 0),
            Keysym::Up => self.navigate(0, -1),
            Keysym::Down => self.navigate(0, 1),
            Keysym::Return => self.activate(),
            _ => match event.utf8.as_ref().and_then(|s| s.chars().next()) {
                Some(c) if !c.is_control() => {
                    self.edit_query(|q| q.push(c));
                    true
                }
                _ => false,
            },
        };
        if redraw {
            self.redraw(qh);
        }
    }
    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: Modifiers, _: u32) {}
}

impl PointerHandler for App {
    fn pointer_frame(&mut self, conn: &Connection, qh: &QueueHandle<Self>, pointer: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        let mut redraw = false;
        for ev in events {
            if &ev.surface != self.layer.wl_surface() { continue; }
            let (x, y) = (ev.position.0 * self.scale as f64, ev.position.1 * self.scale as f64);
            match ev.kind {
                PointerEventKind::Enter { serial } => {
                    self.set_cursor(conn, pointer, serial);
                    redraw |= self.pointer_moved(x, y);
                }
                PointerEventKind::Motion { .. } => redraw |= self.pointer_moved(x, y),
                PointerEventKind::Press { button: BTN_LEFT, .. } => redraw |= self.handle_press(x, y),
                PointerEventKind::Release { button, .. } if self.picker_target.is_none() => {
                    if button == BTN_LEFT {
                        redraw |= self.handle_release(x, y);
                    } else if button == BTN_RIGHT {
                        if let Some(tile) = self.tile_at(x, y) {
                            redraw |= self.tiles[tile].take().is_some();
                        }
                        self.press_start = None;
                    }
                }
                PointerEventKind::Axis { vertical, .. } if self.picker_target.is_some() => {
                    // Scroll the picker a row at a time (mouse-only)
                    let before = self.picker_scroll;
                    if vertical.absolute > 0.0 {
                        self.picker_scroll = (self.picker_scroll + PICKER_COLS).min(self.picker_max_scroll());
                    } else if vertical.absolute < 0.0 {
                        self.picker_scroll = self.picker_scroll.saturating_sub(PICKER_COLS);
                    }
                    redraw |= self.picker_scroll != before;
                }
                _ => {}
            }
        }
        if redraw {
            self.redraw(qh);
        }
    }
}

impl TouchHandler for App {
    fn down(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_touch::WlTouch, _serial: u32, _time: u32, _surface: wl_surface::WlSurface, _id: i32, position: (f64, f64)) {
        let (x, y) = (position.0 * self.scale as f64, position.1 * self.scale as f64);
        dlog!("  touch down at ({:.0}, {:.0})", x, y);
        self.pointer_pos = (x, y);
        if self.handle_press(x, y) {
            self.redraw(qh);
        }
    }

    fn up(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_touch::WlTouch, _serial: u32, _time: u32, _id: i32) {
        // wl_touch.up carries no position; use the last down/motion position.
        let (x, y) = self.pointer_pos;
        if self.handle_release(x, y) {
            self.redraw(qh);
        }
    }

    fn motion(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_touch::WlTouch, _time: u32, _id: i32, position: (f64, f64)) {
        let (x, y) = (position.0 * self.scale as f64, position.1 * self.scale as f64);
        if self.handle_drag_motion(x, y) {
            self.redraw(qh);
        }
    }

    fn cancel(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_touch::WlTouch) {
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
