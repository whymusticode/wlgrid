//! Pure-logic functions shared between the binary and benches.
//!
//! Anything here must be Wayland-free and runnable in a plain cargo bench.

use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use image::GenericImageView;
use ab_glyph::{Font, FontVec, ScaleFont, point};
use serde::{Deserialize, Serialize};

// ── timing log ─────────────────────────────────────────────────────────────
//
// `dlog!` is an `eprintln!` that's silent unless enabled by `wlgrid -t` (see
// `enable_log`) or the `WLGRID_DEBUG` environment variable (handy for
// benches). Each line is prefixed with milliseconds since logging started,
// which `main` does first thing, so the log reads as a startup timeline.

use std::sync::OnceLock;
use std::time::Instant;

static LOG_START: OnceLock<Option<Instant>> = OnceLock::new();

/// Turn logging on, with timestamps relative to now.
pub fn enable_log() {
    let _ = LOG_START.set(Some(Instant::now()));
}

/// When logging started, or None if it's off. Resolved once per process.
pub fn log_start() -> Option<Instant> {
    *LOG_START.get_or_init(|| std::env::var("WLGRID_DEBUG").is_ok().then(Instant::now))
}

/// Timestamped log line; a no-op (except for a load) when logging is off.
/// Use `eprintln!` directly for actual errors that users should always see.
#[macro_export]
macro_rules! dlog {
    ($($arg:tt)*) => {
        if let Some(t) = $crate::log_start() {
            eprintln!("{:8.2}ms {}", t.elapsed().as_secs_f64() * 1000.0, format_args!($($arg)*));
        }
    };
}

// ── icon + desktop entry types ─────────────────────────────────────────────

/// Default icon size (pixels) if none is set in wlgrid.toml.
pub const DEFAULT_ICON_SIZE: u32 = 48;

/// A launchable entry. `pixels` is always `icon_size`×`icon_size` RGBA.
#[derive(Clone)]
pub struct Icon {
    pub name: String,
    pub name_lower: String,
    pub exec: String,
    pub pixels: Vec<u8>,
}

pub struct DesktopEntry {
    pub name: String,
    pub icon_name: String,
    pub exec: String,
}

fn sanitize_desktop_exec(exec: &str) -> Option<String> {
    let exec = exec.trim();
    if exec.is_empty() || exec.len() > 1024 || exec.chars().any(char::is_control) {
        return None;
    }
    // Reject explicit shell-wrapper launchers; they undermine no-shell exec safety.
    let lower = exec.to_ascii_lowercase();
    let shell_wrapped = [
        "sh -c ", "bash -c ", "zsh -c ", "fish -c ",
        "/bin/sh -c ", "/bin/bash -c ", "/usr/bin/bash -c ",
    ];
    if shell_wrapped.iter().any(|p| lower.starts_with(p)) {
        return None;
    }
    Some(exec.to_string())
}

// ── icon cache ─────────────────────────────────────────────────────────────
//
// Only icon *pixels* are cached: resolving an `Icon=` name to a file and
// decoding/resizing it is ~50ms for a typical system, while parsing every
// .desktop file and locating fonts is ~1-2ms combined. Entries are keyed by
// their `Icon=` value; an empty pixel buffer records "no icon file found" so
// misses (the slowest lookups) aren't repeated either.

const CACHE_VERSION: u32 = 3;

#[derive(Serialize, Deserialize, Clone)]
pub struct IconCache {
    version: u32,
    size: u32,
    icons: HashMap<String, Vec<u8>>,
    /// Set when a lookup added an entry, so the caller knows to save.
    #[serde(skip)]
    pub dirty: bool,
}

impl IconCache {
    pub fn new(size: u32) -> Self {
        IconCache { version: CACHE_VERSION, size, icons: HashMap::new(), dirty: false }
    }

    fn path() -> Option<PathBuf> {
        env::var("HOME").ok().map(|h| PathBuf::from(format!("{h}/.cache/wlgrid/icons.bin")))
    }

    /// Load the on-disk cache, or an empty one if it's missing, stale, or
    /// was written for a different icon size.
    pub fn load(size: u32) -> Self {
        let cache = Self::path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|d| bincode::deserialize::<IconCache>(&d).ok())
            .filter(|c| c.version == CACHE_VERSION && c.size == size);
        dlog!("  cache: {}", cache.as_ref().map_or("miss".to_string(), |c| format!("{} icons", c.icons.len())));
        cache.unwrap_or_else(|| Self::new(size))
    }

    pub fn save(&self) {
        let Some(path) = Self::path() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(data) = bincode::serialize(self) {
            if std::fs::write(&path, &data).is_ok() {
                dlog!("  cache: saved {} icons ({} bytes)", self.icons.len(), data.len());
            }
        }
    }

    /// Pixels for `icon_name`, resolving and decoding it on a cache miss.
    /// Empty if no usable icon file exists.
    fn get(&mut self, icon_name: &str) -> Vec<u8> {
        if let Some(p) = self.icons.get(icon_name) {
            return p.clone();
        }
        let pixels = find_icon_file(icon_name)
            .and_then(|p| load_icon_rgba(&p, self.size))
            .unwrap_or_default();
        dlog!("    icon '{}': {}", icon_name, if pixels.is_empty() { "not found" } else { "loaded" });
        self.icons.insert(icon_name.to_string(), pixels.clone());
        self.dirty = true;
        pixels
    }
}

// ── desktop entry scanning ─────────────────────────────────────────────────

pub fn parse_desktop_file(path: &Path) -> Option<DesktopEntry> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut name = None;
    let mut icon = None;
    let mut exec = None;
    let mut app_type = None;
    let mut hidden = false;
    let mut no_display = false;
    let mut in_desktop_entry = false;

    for line in content.lines() {
        let line = line.trim();
        if line == "[Desktop Entry]" {
            in_desktop_entry = true;
            continue;
        }
        if line.starts_with('[') {
            in_desktop_entry = false;
            continue;
        }
        if !in_desktop_entry { continue; }

        if let Some(val) = line.strip_prefix("Name=") {
            if name.is_none() { name = Some(val.to_string()); }
        } else if let Some(val) = line.strip_prefix("Icon=") {
            icon = Some(val.to_string());
        } else if let Some(val) = line.strip_prefix("Exec=") {
            exec = Some(val.to_string());
        } else if let Some(val) = line.strip_prefix("Type=") {
            app_type = Some(val.trim().to_string());
        } else if let Some(val) = line.strip_prefix("Hidden=") {
            hidden = val.trim().eq_ignore_ascii_case("true");
        } else if let Some(val) = line.strip_prefix("NoDisplay=") {
            no_display = val.trim().eq_ignore_ascii_case("true");
        }
    }

    // Only include visible application entries.
    if app_type.as_deref() != Some("Application") || hidden || no_display {
        return None;
    }
    let exec = sanitize_desktop_exec(&exec?)?;

    Some(DesktopEntry {
        name: name?,
        icon_name: icon.unwrap_or_default(),
        exec,
    })
}

pub fn find_icon_file(icon_name: &str) -> Option<PathBuf> {
    if icon_name.starts_with('/') {
        let p = PathBuf::from(icon_name);
        if p.exists() { return Some(p); }
    }

    let sizes = ["48x48", "64x64", "96x96", "128x128", "256x256", "512x512", "32x32", "scalable"];
    let categories = ["apps", "applications"];
    let themes = ["hicolor", "Adwaita", "breeze", "Papirus"];
    let extensions = ["png", "svg", "webp", "jpg", "jpeg"];

    let home = env::var("HOME").unwrap_or_default();
    let bases = [
        "/usr/share/icons".to_string(),
        format!("{home}/.local/share/icons"),
        "/run/current-system/sw/share/icons".to_string(),
        "/var/lib/flatpak/exports/share/icons".to_string(),
        format!("{home}/.local/share/flatpak/exports/share/icons"),
    ];

    for base in &bases {
        for theme in themes {
            // Skip absent themes up front: a miss otherwise costs ~80 stats each.
            if !Path::new(&format!("{base}/{theme}")).is_dir() { continue; }
            for size in sizes {
                for cat in categories {
                    for ext in extensions {
                        let path = PathBuf::from(format!("{base}/{theme}/{size}/{cat}/{icon_name}.{ext}"));
                        if path.exists() { return Some(path); }
                    }
                }
            }
        }
    }

    extensions.iter()
        .map(|ext| PathBuf::from(format!("/usr/share/pixmaps/{icon_name}.{ext}")))
        .find(|p| p.exists())
}

/// Every directory in the tree rooted at `root` (including `root` itself),
/// or an empty list if `root` doesn't exist. Symlinks are not followed, so a
/// cyclic link can't send us into an infinite walk.
fn walk_subdirs(root: &str) -> Vec<String> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_dir())
        .map(|e| e.path().to_string_lossy().into_owned())
        .collect()
}

/// Get application directories from XDG_DATA_DIRS and user directories.
/// User directories come first for priority.
pub fn get_application_dirs() -> Vec<String> {
    let mut dirs = Vec::new();

    if let Ok(home) = env::var("HOME") {
        let data_home = env::var("XDG_DATA_HOME")
            .unwrap_or_else(|_| format!("{home}/.local/share"));
        dirs.push(format!("{data_home}/applications"));

        // Wine installs its Start Menu entries under applications/wine/Programs,
        // nested one directory per vendor/program group. The top-level scan is
        // non-recursive, so expand that subtree explicitly.
        dirs.extend(walk_subdirs(&format!("{data_home}/applications/wine/Programs")));

        dirs.push(format!("{home}/.local/share/flatpak/exports/share/applications"));
        dirs.push(format!("{home}/.nix-profile/share/applications"));
    }

    if let Ok(user) = env::var("USER") {
        dirs.push(format!("/etc/profiles/per-user/{user}/share/applications"));
    }

    let data_dirs = env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());

    for dir in data_dirs.split(':') {
        if !dir.is_empty() {
            dirs.push(format!("{dir}/applications"));
        }
    }

    dirs.push("/var/lib/flatpak/exports/share/applications".to_string());

    dirs
}

/// Procedural placeholder icon: a question mark on a transparent background.
/// Used when an entry has no icon and its name renders to nothing, so the
/// tile still visibly indicates "something is here".
pub fn make_placeholder_icon(size: u32) -> Vec<u8> {
    let mut px = vec![0u8; (size * size * 4) as usize];

    let fg = [0xE0u8, 0xE0u8, 0xE8u8, 0xFFu8];

    let put = |px: &mut Vec<u8>, x: i32, y: i32| {
        if x < 0 || y < 0 || x >= size as i32 || y >= size as i32 { return; }
        let i = ((y as u32 * size + x as u32) * 4) as usize;
        px[i..i + 4].copy_from_slice(&fg);
    };

    let cx = size as f32 / 2.0;
    let hook_cy = size as f32 * 0.35;
    let r_outer = size as f32 * 0.24;
    let r_inner = size as f32 * 0.12;

    for y in 0..size as i32 {
        for x in 0..size as i32 {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - hook_cy;
            let d2 = dx * dx + dy * dy;

            let in_ring = d2 <= r_outer * r_outer && d2 >= r_inner * r_inner;
            let bottom_left_cut = dy > 0.0 && dx < 0.0;
            if in_ring && !bottom_left_cut {
                put(&mut px, x, y);
            }
        }
    }

    let stem_w = (size as f32 * 0.14) as i32;
    let stem_x0 = (cx - stem_w as f32 / 2.0) as i32;
    let stem_y0 = (size as f32 * 0.50) as i32;
    let stem_y1 = (size as f32 * 0.70) as i32;
    for y in stem_y0..stem_y1 {
        for x in stem_x0..(stem_x0 + stem_w) {
            put(&mut px, x, y);
        }
    }

    let dot_w = (size as f32 * 0.16) as i32;
    let dot_x0 = (cx - dot_w as f32 / 2.0) as i32;
    let dot_y0 = (size as f32 * 0.78) as i32;
    for y in dot_y0..(dot_y0 + dot_w) {
        for x in dot_x0..(dot_x0 + dot_w) {
            put(&mut px, x, y);
        }
    }

    px
}

// ── fonts ──────────────────────────────────────────────────────────────────

/// Fonts used for text rendering: a primary text font plus an optional Nerd
/// Font symbols font for glyphs in the private-use ranges.
pub struct Fonts {
    pub text: FontVec,
    pub symbols: Option<FontVec>,
}

impl Fonts {
    /// The font that should render `c`.
    pub fn for_char(&self, c: char) -> &FontVec {
        match &self.symbols {
            Some(s) if is_nerd_symbol(c) => s,
            _ => &self.text,
        }
    }

    /// Horizontal advance of `text` at `size` px.
    pub fn text_width(&self, text: &str, size: f32) -> f32 {
        text.chars().map(|c| {
            let f = self.for_char(c);
            f.as_scaled(size).h_advance(f.glyph_id(c))
        }).sum()
    }

    /// Find a regular sans text font and a Nerd Font symbols font in the
    /// usual system/user font dirs. Walking the dirs is well under 1ms.
    pub fn load() -> Option<Fonts> {
        let home = env::var("HOME").unwrap_or_default();
        let dirs = [
            "/run/current-system/sw/share/X11/fonts".to_string(),
            "/run/current-system/sw/share/fonts".to_string(),
            "/usr/share/fonts".to_string(),
            "/usr/local/share/fonts".to_string(),
            format!("{home}/.local/share/fonts"),
            format!("{home}/.fonts"),
        ];
        let files: Vec<(PathBuf, String)> = dirs.iter()
            .flat_map(|d| walkdir::WalkDir::new(d).into_iter().filter_map(Result::ok))
            .filter(|e| e.path().extension().is_some_and(|e| e == "ttf" || e == "otf"))
            .map(|e| (e.path().to_path_buf(), e.file_name().to_string_lossy().into_owned()))
            .collect();
        dlog!("  found {} font files", files.len());

        let load = |(path, _): &(PathBuf, String)| {
            let font = FontVec::try_from_vec(std::fs::read(path).ok()?).ok()?;
            dlog!("  loaded font: {}", path.display());
            Some(font)
        };
        let regular = |n: &str| !["Nerd", "Symbol", "Bold", "Italic"].iter().any(|x| n.contains(x));
        // Preferred families in order; "" matches anything as a last resort.
        let text = ["DejaVuSans", "LiberationSans", "NotoSans", "Ubuntu", "Roboto", ""].iter()
            .flat_map(|pat| files.iter().filter(move |(_, n)| n.contains(pat) && regular(n)))
            .find_map(load)?;
        let symbols = files.iter()
            .filter(|(_, n)| n.contains("NerdFont") && n.contains("Symbol"))
            .find_map(load);
        Some(Fonts { text, symbols })
    }
}

/// Whether `c` falls in a Nerd Font symbol range (private-use areas, etc.).
pub fn is_nerd_symbol(c: char) -> bool {
    let cp = c as u32;
    (0xE000..=0xF8FF).contains(&cp)        // Basic PUA
        || (0xF0000..=0xFFFFD).contains(&cp)   // Supplementary PUA-A
        || (0x100000..=0x10FFFD).contains(&cp) // Supplementary PUA-B
        || (0x23FB..=0x23FE).contains(&cp)     // Power symbols
        || (0x2B58..=0x2B58).contains(&cp)     // Heavy circle
        || (0xF500..=0xFD46).contains(&cp)     // More nerd icons
}

/// Render an entry's name into a `size`×`size` RGBA icon, used when the entry
/// has no real icon. If the name ends in a Nerd Font glyph we render just that
/// glyph large; otherwise we render the whole name, shrunk to fit the square.
pub fn render_name_icon(fonts: &Fonts, name: &str, size: u32) -> Vec<u8> {
    let name = name.trim();
    let text: String = match name.chars().last() {
        Some(c) if is_nerd_symbol(c) => c.to_string(),
        _ => name.to_string(),
    };
    if text.is_empty() {
        return make_placeholder_icon(size);
    }

    let sz = size as f32;
    let fit = sz * 0.86;
    // Generous height for a single glyph, smaller for a word; then shrink to width.
    let mut px = if text.chars().count() == 1 { sz * 0.92 } else { sz * 0.55 };
    let measured = fonts.text_width(&text, px);
    if measured > fit { px *= fit / measured.max(1.0); }
    px = px.min(fit);
    let text_w = fonts.text_width(&text, px);

    // Outline glyphs along a baseline at y=0; ab_glyph yields px bounds relative
    // to that baseline (min.y negative above it). Collect to centre vertically.
    let mut pen_x = 0.0f32;
    let mut outlines = Vec::new();
    let (mut top, mut bot) = (f32::MAX, f32::MIN);
    for c in text.chars() {
        let f = fonts.for_char(c);
        let gid = f.glyph_id(c);
        if let Some(o) = f.outline_glyph(gid.with_scale_and_position(px, point(pen_x, 0.0))) {
            let b = o.px_bounds();
            top = top.min(b.min.y);
            bot = bot.max(b.max.y);
            outlines.push(o);
        }
        pen_x += f.as_scaled(px).h_advance(gid);
    }
    if outlines.is_empty() {
        return make_placeholder_icon(size);
    }

    let x_off = (sz - text_w) / 2.0;
    let y_off = (sz - (bot - top)) / 2.0 - top;
    let mut buf = vec![0u8; (size * size * 4) as usize];
    for o in &outlines {
        let b = o.px_bounds();
        o.draw(|gx, gy, cov| {
            let xx = (b.min.x + x_off) as i32 + gx as i32;
            let yy = (b.min.y + y_off) as i32 + gy as i32;
            if xx < 0 || yy < 0 || xx >= size as i32 || yy >= size as i32 { return; }
            let a = (cov * 255.0) as u8;
            if a == 0 { return; }
            let idx = ((yy as u32 * size + xx as u32) * 4) as usize;
            buf[idx..idx + 4].copy_from_slice(&[0xE0, 0xE0, 0xE8, a]);
        });
    }
    buf
}

/// Load all desktop entries, then append the config's `extra` (icon, command)
/// entries, which are named by their command. Icon pixels come from `cache`;
/// entries without a usable icon file get their name rendered as the icon
/// instead, and extra entries render their icon text the same way.
pub fn load_entries(icon_size: u32, fonts: &Fonts, cache: &mut IconCache, extra: &[(String, String)]) -> Vec<Icon> {
    let mut entries = Vec::new();
    for dir in get_application_dirs() {
        let Ok(read_dir) = std::fs::read_dir(&dir) else { continue };
        entries.extend(read_dir.flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "desktop"))
            .filter_map(|p| parse_desktop_file(&p)));
    }

    let mut seen_names = std::collections::HashSet::new();
    let mut icons: Vec<Icon> = entries.into_iter()
        .filter(|de| seen_names.insert(de.name.clone()))
        .map(|de| {
            let pixels = if de.icon_name.is_empty() { Vec::new() } else { cache.get(&de.icon_name) };
            let pixels = if pixels.is_empty() { render_name_icon(fonts, &de.name, icon_size) } else { pixels };
            Icon { name_lower: de.name.to_lowercase(), name: de.name, exec: de.exec, pixels }
        })
        .collect();
    icons.extend(extra.iter()
        .filter(|(_, exec)| seen_names.insert(exec.clone()))
        .map(|(icon, exec)| Icon {
            name: exec.clone(),
            name_lower: exec.to_lowercase(),
            exec: exec.clone(),
            pixels: render_name_icon(fonts, icon, icon_size),
        }));
    icons
}

/// Decode an image/SVG file into `target_size`×`target_size` RGBA.
pub fn load_icon_rgba(path: &Path, target_size: u32) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).ok()?;

    if path.extension().is_some_and(|e| e == "svg") {
        return load_svg_rgba(&bytes, target_size);
    }

    let img = image::load_from_memory(&bytes).ok()?;
    let img = if img.dimensions() != (target_size, target_size) {
        img.resize_exact(target_size, target_size, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };
    Some(img.to_rgba8().into_vec())
}

pub fn load_svg_rgba(data: &[u8], target_size: u32) -> Option<Vec<u8>> {
    use resvg::usvg::{Options, Tree};
    use resvg::tiny_skia::{self, Pixmap};

    let tree = Tree::from_data(data, &Options::default()).ok()?;
    let size = tree.size();

    let mut pixmap = Pixmap::new(target_size, target_size)?;

    let scale = (target_size as f32 / size.width()).min(target_size as f32 / size.height());
    let tx = (target_size as f32 - size.width() * scale) / 2.0;
    let ty = (target_size as f32 - size.height() * scale) / 2.0;

    let transform = tiny_skia::Transform::from_scale(scale, scale).post_translate(tx, ty);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    Some(pixmap.take())
}
