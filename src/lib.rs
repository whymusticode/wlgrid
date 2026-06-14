//! Pure-logic functions shared between the binary and benches.
//!
//! Anything here must be Wayland-free and runnable in a plain cargo bench.

use std::env;
use std::path::{Path, PathBuf};
use image::GenericImageView;
use ab_glyph::{Font, FontVec, ScaleFont, point};
use serde::{Deserialize, Serialize};

// ── debug logging ──────────────────────────────────────────────────────────
//
// `dlog!` is a drop-in replacement for `eprintln!` that's silent unless the
// `WLGRID_DEBUG` environment variable is set (to anything). The flag is read
// once per process via OnceLock. The motivation: in normal launches we don't
// want ~100 per-icon log lines syscalling to stderr before we've even drawn a
// frame. Benches and manual debugging runs can opt in with
// `WLGRID_DEBUG=1 cargo run` or `WLGRID_DEBUG=1 cargo bench`.

use std::sync::OnceLock;

static DEBUG_ENABLED: OnceLock<bool> = OnceLock::new();

/// Returns whether verbose debug logging is enabled. Checked once per process.
pub fn debug_enabled() -> bool {
    *DEBUG_ENABLED.get_or_init(|| std::env::var("WLGRID_DEBUG").is_ok())
}

/// Verbose log macro. Becomes a no-op (except for a bool check) when
/// `WLGRID_DEBUG` is unset. Use `eprintln!` directly for actual errors that
/// users should always see.
#[macro_export]
macro_rules! dlog {
    ($($arg:tt)*) => {
        if $crate::debug_enabled() {
            eprintln!($($arg)*);
        }
    };
}

// ── icon + desktop entry types ─────────────────────────────────────────────

/// Default icon size (pixels) if none is set in config.toml.
pub const DEFAULT_ICON_SIZE: u32 = 48;

#[derive(Clone)]
pub struct Icon {
    pub name: String,
    pub name_lower: String,
    pub exec: String,
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
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

// ── binary cache (for fast startup) ────────────────────────────────────────

pub const CACHE_VERSION: u32 = 2;

#[derive(Serialize, Deserialize)]
pub struct Cache {
    pub version: u32,
    pub checksum: u64,
    pub icons: Vec<CachedIcon>,
    pub text_font_path: Option<String>,
    pub symbols_font_path: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct CachedIcon {
    pub name: String,
    pub exec: String,
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

pub fn cache_path() -> Option<PathBuf> {
    env::var("HOME").ok().map(|h| PathBuf::from(format!("{h}/.config/wlgrid/cache.bin")))
}

pub fn compute_checksum() -> u64 {
    // Hash the sorted list of .desktop filenames across all application dirs.
    //
    // We don't hash dir mtimes because on NixOS the application dirs live
    // inside the system profile derivation and have pinned mtimes that never
    // change when packages are added — so mtime-based invalidation silently
    // misses new apps. Reading the dirs is fast (~ms) and reliably catches
    // additions and removals.
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();

    for dir in get_application_dirs() {
        let Ok(read_dir) = std::fs::read_dir(&dir) else { continue };
        let mut names: Vec<String> = read_dir
            .flatten()
            .filter_map(|e| {
                let path = e.path();
                if path.extension().map_or(true, |ext| ext != "desktop") {
                    return None;
                }
                path.file_name().map(|n| n.to_string_lossy().into_owned())
            })
            .collect();
        names.sort();
        dir.hash(&mut hasher);
        for name in names {
            name.hash(&mut hasher);
        }
    }

    // Hash config.toml mtime (this one isn't a nix store path, so mtime works)
    if let Ok(home) = env::var("HOME") {
        let config_path = format!("{home}/.config/wlgrid/config.toml");
        if let Ok(meta) = std::fs::metadata(&config_path) {
            if let Ok(mtime) = meta.modified() {
                mtime.hash(&mut hasher);
            }
        }
    }

    CACHE_VERSION.hash(&mut hasher);

    hasher.finish()
}

pub fn load_cache() -> Option<Cache> {
    let path = cache_path()?;
    let data = std::fs::read(&path).ok()?;
    let cache: Cache = bincode::deserialize(&data).ok()?;

    if cache.version != CACHE_VERSION {
        dlog!("  cache: version mismatch");
        return None;
    }

    let expected_checksum = compute_checksum();
    if cache.checksum != expected_checksum {
        dlog!("  cache: checksum mismatch");
        return None;
    }

    dlog!("  cache: valid, loading {} icons", cache.icons.len());
    Some(cache)
}

pub fn save_cache(icons: &[Icon], text_font_path: Option<&Path>, symbols_font_path: Option<&Path>) {
    let cache = Cache {
        version: CACHE_VERSION,
        checksum: compute_checksum(),
        icons: icons.iter().map(|i| CachedIcon {
            name: i.name.clone(),
            exec: i.exec.clone(),
            pixels: i.pixels.clone(),
            width: i.width,
            height: i.height,
        }).collect(),
        text_font_path: text_font_path.map(|p| p.to_string_lossy().into_owned()),
        symbols_font_path: symbols_font_path.map(|p| p.to_string_lossy().into_owned()),
    };

    if let Some(path) = cache_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(data) = bincode::serialize(&cache) {
            if std::fs::write(&path, &data).is_ok() {
                dlog!("  cache: saved {} icons ({} bytes)", cache.icons.len(), data.len());
            }
        }
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

    for theme in themes {
        for size in sizes {
            for cat in categories {
                for ext in extensions {
                    let path = PathBuf::from(format!(
                        "/usr/share/icons/{theme}/{size}/{cat}/{icon_name}.{ext}"
                    ));
                    if path.exists() { return Some(path); }

                    if let Ok(home) = env::var("HOME") {
                        let local_path = PathBuf::from(format!(
                            "{home}/.local/share/icons/{theme}/{size}/{cat}/{icon_name}.{ext}"
                        ));
                        if local_path.exists() { return Some(local_path); }
                    }
                }
            }
        }
    }

    for ext in extensions {
        let path = PathBuf::from(format!("/usr/share/pixmaps/{icon_name}.{ext}"));
        if path.exists() { return Some(path); }
    }

    for theme in themes {
        for size in sizes {
            for cat in categories {
                for ext in extensions {
                    let path = PathBuf::from(format!(
                        "/run/current-system/sw/share/icons/{theme}/{size}/{cat}/{icon_name}.{ext}"
                    ));
                    if path.exists() { return Some(path); }
                }
            }
        }
    }

    let flatpak_dirs = ["/var/lib/flatpak/exports/share/icons"];
    let home = env::var("HOME").ok();
    let user_flatpak = home.as_ref().map(|h| format!("{h}/.local/share/flatpak/exports/share/icons"));

    for base in flatpak_dirs.iter().map(|s| s.to_string()).chain(user_flatpak) {
        for theme in themes {
            for size in sizes {
                for cat in categories {
                    for ext in extensions {
                        let path = PathBuf::from(format!(
                            "{base}/{theme}/{size}/{cat}/{icon_name}.{ext}"
                        ));
                        if path.exists() { return Some(path); }
                    }
                }
            }
        }
    }

    None
}

/// Get application directories from XDG_DATA_DIRS and user directories.
/// User directories come first for priority.
pub fn get_application_dirs() -> Vec<String> {
    let mut dirs = Vec::new();

    if let Ok(home) = env::var("HOME") {
        let data_home = env::var("XDG_DATA_HOME")
            .unwrap_or_else(|_| format!("{home}/.local/share"));
        dirs.push(format!("{data_home}/applications"));

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
/// Used when a desktop entry has no icon or its icon can't be loaded, so the
/// tile still visibly indicates "something is here".
pub fn make_placeholder_icon(size: u32) -> (Vec<u8>, u32, u32) {
    let mut px = vec![0u8; (size * size * 4) as usize];

    let fg = [0xE0u8, 0xE0u8, 0xE8u8, 0xFFu8];

    let put = |px: &mut Vec<u8>, x: i32, y: i32| {
        if x < 0 || y < 0 || x >= size as i32 || y >= size as i32 { return; }
        let i = ((y as u32 * size + x as u32) * 4) as usize;
        px[i] = fg[0];
        px[i + 1] = fg[1];
        px[i + 2] = fg[2];
        px[i + 3] = fg[3];
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

    (px, size, size)
}

/// Fonts used for text rendering: a primary text font plus an optional Nerd
/// Font symbols font for glyphs in the private-use ranges.
pub struct Fonts {
    pub text: FontVec,
    pub symbols: Option<FontVec>,
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
pub fn render_name_icon(fonts: &Fonts, name: &str, size: u32) -> (Vec<u8>, u32, u32) {
    let name = name.trim();
    let text: String = match name.chars().last() {
        Some(c) if is_nerd_symbol(c) => c.to_string(),
        _ => name.to_string(),
    };
    if text.is_empty() {
        return make_placeholder_icon(size);
    }

    let font_for = |c: char| -> &FontVec {
        if is_nerd_symbol(c) { fonts.symbols.as_ref().unwrap_or(&fonts.text) } else { &fonts.text }
    };

    let sz = size as f32;
    let fit = sz * 0.86;
    // Generous height for a single glyph, smaller for a word; then shrink to width.
    let mut px = if text.chars().count() == 1 { sz * 0.92 } else { sz * 0.55 };
    let measure = |px: f32| -> f32 {
        text.chars().map(|c| {
            let f = font_for(c);
            f.as_scaled(px).h_advance(f.glyph_id(c))
        }).sum()
    };
    let measured = measure(px);
    if measured > fit { px *= fit / measured.max(1.0); }
    px = px.min(fit);
    let text_w = measure(px);

    // Outline glyphs along a baseline at y=0; ab_glyph yields px bounds relative
    // to that baseline (min.y negative above it). Collect to centre vertically.
    let mut pen_x = 0.0f32;
    let mut outlines = Vec::new();
    let (mut top, mut bot) = (f32::MAX, f32::MIN);
    for c in text.chars() {
        let f = font_for(c);
        let sf = f.as_scaled(px);
        let gid = f.glyph_id(c);
        if let Some(o) = f.outline_glyph(gid.with_scale_and_position(px, point(pen_x, 0.0))) {
            let b = o.px_bounds();
            top = top.min(b.min.y);
            bot = bot.max(b.max.y);
            outlines.push(o);
        }
        pen_x += sf.h_advance(gid);
    }
    if outlines.is_empty() {
        return make_placeholder_icon(size);
    }

    let x_off = (sz - text_w) / 2.0;
    let y_off = (sz - (bot - top)) / 2.0 - top;
    let mut buf = vec![0u8; (size * size * 4) as usize];
    let fg = [0xE0u8, 0xE0u8, 0xE8u8];
    for o in &outlines {
        let b = o.px_bounds();
        o.draw(|gx, gy, cov| {
            let xx = (b.min.x + x_off) as i32 + gx as i32;
            let yy = (b.min.y + y_off) as i32 + gy as i32;
            if xx < 0 || yy < 0 || xx >= size as i32 || yy >= size as i32 { return; }
            let a = (cov * 255.0) as u8;
            if a == 0 { return; }
            let idx = ((yy as u32 * size + xx as u32) * 4) as usize;
            buf[idx] = fg[0];
            buf[idx + 1] = fg[1];
            buf[idx + 2] = fg[2];
            buf[idx + 3] = a;
        });
    }
    (buf, size, size)
}

/// Load desktop entries. When an entry has no resolvable icon, render its name
/// (via `fonts`) as the icon; without fonts, fall back to the `?` placeholder.
pub fn load_desktop_entries(icon_size: u32, fonts: Option<&Fonts>) -> Vec<Icon> {
    let mut icons = Vec::new();
    let mut seen_names = std::collections::HashSet::new();

    for dir in get_application_dirs() {
        dlog!("  scanning {}", dir);
        let Ok(read_dir) = std::fs::read_dir(&dir) else { continue };

        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().map_or(true, |e| e != "desktop") { continue; }

            let Some(de) = parse_desktop_file(&path) else { continue };
            if !seen_names.insert(de.name.clone()) { continue; }

            let loaded_icon = if de.icon_name.is_empty() {
                dlog!("    {} - no icon specified", de.name);
                None
            } else if let Some(icon_path) = find_icon_file(&de.icon_name) {
                if let Some((p, w, h)) = load_icon_rgba(&icon_path, icon_size) {
                    dlog!("    {} - scaled to {}x{}", de.name, w, h);
                    Some((p, w, h))
                } else {
                    dlog!("    {} - failed to load {}", de.name, icon_path.display());
                    None
                }
            } else {
                dlog!("    {} - icon '{}' not found", de.name, de.icon_name);
                None
            };

            let (pixels, width, height) = loaded_icon.unwrap_or_else(|| match fonts {
                Some(f) => render_name_icon(f, &de.name, icon_size),
                None => make_placeholder_icon(icon_size),
            });

            icons.push(Icon {
                name_lower: de.name.to_lowercase(),
                name: de.name,
                exec: de.exec,
                pixels,
                width,
                height,
            });
        }
    }

    icons
}

pub fn load_icon_rgba(path: &Path, target_size: u32) -> Option<(Vec<u8>, u32, u32)> {
    let bytes = std::fs::read(path).ok()?;

    if path.extension().map_or(false, |e| e == "svg") {
        return load_svg_rgba(&bytes, target_size);
    }

    let img = image::load_from_memory(&bytes).ok()?;

    let (w, h) = img.dimensions();
    let img = if w != target_size || h != target_size {
        img.resize_exact(target_size, target_size, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };

    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Some((rgba.into_vec(), w, h))
}

pub fn load_svg_rgba(data: &[u8], target_size: u32) -> Option<(Vec<u8>, u32, u32)> {
    use resvg::usvg::{Options, Tree};
    use resvg::tiny_skia::{self, Pixmap};

    let tree = Tree::from_data(data, &Options::default()).ok()?;
    let size = tree.size();

    let mut pixmap = Pixmap::new(target_size, target_size)?;

    let scale_x = target_size as f32 / size.width();
    let scale_y = target_size as f32 / size.height();
    let scale = scale_x.min(scale_y);

    let tx = (target_size as f32 - size.width() * scale) / 2.0;
    let ty = (target_size as f32 - size.height() * scale) / 2.0;

    let transform = tiny_skia::Transform::from_scale(scale, scale).post_translate(tx, ty);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    Some((pixmap.take(), target_size, target_size))
}
