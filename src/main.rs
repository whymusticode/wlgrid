use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;
use std::io::Write;
use image::GenericImageView;
use serde::{Deserialize, Serialize};
use fontdue::{Font, FontSettings};

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
    opacity: Option<f32>,
    #[serde(default)]
    bottom_bar: Option<BottomBar>,
    #[serde(default)]
    search_engines: Option<String>,
    #[serde(default)]
    search: Option<String>,
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
    let config_paths = [
        env::var("HOME").ok().map(|h| format!("{h}/.config/wlgrid/config.toml")),
        Some("/etc/wlgrid/config.toml".to_string()),
    ];

    for path in config_paths.into_iter().flatten() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            match toml::from_str(&content) {
                Ok(config) => {
                    eprintln!("  loaded config from {}", path);
                    return config;
                }
                Err(e) => eprintln!("  config parse error in {}: {}", path, e),
            }
        }
    }

    eprintln!("  no config found, using defaults");
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
                eprintln!("  loaded state from {}", path.display());
                return state;
            }
        }
    }
    eprintln!("  no saved state, starting fresh");
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
                    eprintln!("  saved state to {}", path.display());
                }
            }
            Err(e) => eprintln!("  failed to save state: {}", e),
        }
    }
}

// ── binary cache for fast startup ──

const CACHE_VERSION: u32 = 2;

#[derive(Serialize, Deserialize)]
struct Cache {
    version: u32,
    checksum: u64,
    icons: Vec<CachedIcon>,
    text_font_path: Option<String>,
    symbols_font_path: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct CachedIcon {
    name: String,
    exec: String,
    pixels: Vec<u8>,
    width: u32,
    height: u32,
}

fn cache_path() -> Option<PathBuf> {
    env::var("HOME").ok().map(|h| PathBuf::from(format!("{h}/.config/wlgrid/cache.bin")))
}

fn compute_checksum() -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();

    // Hash mtimes of all application directories
    for dir in get_application_dirs() {
        if let Ok(meta) = std::fs::metadata(&dir) {
            if let Ok(mtime) = meta.modified() {
                mtime.hash(&mut hasher);
            }
        }
    }

    // Hash config.toml mtime
    if let Ok(home) = env::var("HOME") {
        let config_path = format!("{home}/.config/wlgrid/config.toml");
        if let Ok(meta) = std::fs::metadata(&config_path) {
            if let Ok(mtime) = meta.modified() {
                mtime.hash(&mut hasher);
            }
        }
    }

    // Include cache version
    CACHE_VERSION.hash(&mut hasher);

    hasher.finish()
}

fn load_cache() -> Option<Cache> {
    let path = cache_path()?;
    let data = std::fs::read(&path).ok()?;
    let cache: Cache = bincode::deserialize(&data).ok()?;

    // Verify version and checksum
    if cache.version != CACHE_VERSION {
        eprintln!("  cache: version mismatch");
        return None;
    }

    let expected_checksum = compute_checksum();
    if cache.checksum != expected_checksum {
        eprintln!("  cache: checksum mismatch");
        return None;
    }

    eprintln!("  cache: valid, loading {} icons", cache.icons.len());
    Some(cache)
}

fn save_cache(icons: &[Icon], text_font_path: Option<&Path>, symbols_font_path: Option<&Path>) {
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
                eprintln!("  cache: saved {} icons ({} bytes)", cache.icons.len(), data.len());
            }
        }
    }
}

// ── text rendering ──

struct Fonts {
    text: Font,
    symbols: Option<Font>,
}

struct FontsWithPaths {
    fonts: Fonts,
    text_path: PathBuf,
    symbols_path: Option<PathBuf>,
}

fn is_nerd_symbol(c: char) -> bool {
    let cp = c as u32;
    // Nerd font symbol ranges (Private Use Areas)
    (0xE000..=0xF8FF).contains(&cp) ||      // Basic PUA
    (0xF0000..=0xFFFFD).contains(&cp) ||    // Supplementary PUA-A
    (0x100000..=0x10FFFD).contains(&cp) ||  // Supplementary PUA-B
    (0x23FB..=0x23FE).contains(&cp) ||      // Power symbols
    (0x2B58..=0x2B58).contains(&cp) ||      // Heavy circle
    (0xF500..=0xFD46).contains(&cp)         // More nerd icons
}

/// Load fonts from cached paths (fast path)
fn load_fonts_from_paths(text_path: &str, symbols_path: Option<&str>) -> Option<Fonts> {
    let text_bytes = std::fs::read(text_path).ok()?;
    let text_font = Font::from_bytes(text_bytes, FontSettings::default()).ok()?;
    eprintln!("  cache: loaded text font from {}", text_path);

    let symbols_font = symbols_path.and_then(|p| {
        let bytes = std::fs::read(p).ok()?;
        let font = Font::from_bytes(bytes, FontSettings::default()).ok()?;
        eprintln!("  cache: loaded symbols font from {}", p);
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
    eprintln!("  found {} font files", all_fonts.len());

    // Find text font (prefer DejaVu, Liberation, or any sans)
    let text_patterns = ["DejaVuSans", "LiberationSans", "NotoSans", "Ubuntu", "Roboto"];
    let mut text_font = None;
    let mut text_path = None;
    for pattern in text_patterns {
        for path in &all_fonts {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.contains(pattern) && !name.contains("Nerd") && !name.contains("Bold") && !name.contains("Italic") {
                if let Ok(bytes) = std::fs::read(path) {
                    if let Ok(font) = Font::from_bytes(bytes, FontSettings::default()) {
                        eprintln!("  loaded text font: {}", path.display());
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
                    if let Ok(font) = Font::from_bytes(bytes, FontSettings::default()) {
                        eprintln!("  loaded text font (fallback): {}", path.display());
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
                if let Ok(font) = Font::from_bytes(bytes, FontSettings::default()) {
                    eprintln!("  loaded symbols font: {}", path.display());
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

fn render_text(
    canvas: &mut [u8],
    canvas_w: u32,
    canvas_h: u32,
    fonts: &Fonts,
    text: &str,
    x: i32,
    y: i32,
    size: f32,
    color: [u8; 4],
) {
    let mut cursor_x = x as f32;
    for c in text.chars() {
        // Choose font based on character
        let font = if is_nerd_symbol(c) {
            fonts.symbols.as_ref().unwrap_or(&fonts.text)
        } else {
            &fonts.text
        };

        let (metrics, bitmap) = font.rasterize(c, size);

        let gx = cursor_x as i32 + metrics.xmin;
        let gy = y - metrics.height as i32 - metrics.ymin;

        for row in 0..metrics.height {
            for col in 0..metrics.width {
                let px = gx + col as i32;
                let py = gy + row as i32;
                if px < 0 || py < 0 || px >= canvas_w as i32 || py >= canvas_h as i32 {
                    continue;
                }

                let alpha = bitmap[row * metrics.width + col] as u32;
                if alpha == 0 { continue; }

                let idx = ((py as u32 * canvas_w + px as u32) * 4) as usize;
                if idx + 3 >= canvas.len() { continue; }

                // Alpha blend
                let inv_alpha = 255 - alpha;
                canvas[idx] = ((color[0] as u32 * alpha + canvas[idx] as u32 * inv_alpha) / 255) as u8;
                canvas[idx + 1] = ((color[1] as u32 * alpha + canvas[idx + 1] as u32 * inv_alpha) / 255) as u8;
                canvas[idx + 2] = ((color[2] as u32 * alpha + canvas[idx + 2] as u32 * inv_alpha) / 255) as u8;
                canvas[idx + 3] = 255;
            }
        }

        cursor_x += metrics.advance_width;
    }
}

fn text_width(fonts: &Fonts, text: &str, size: f32) -> f32 {
    text.chars().map(|c| {
        let font = if is_nerd_symbol(c) {
            fonts.symbols.as_ref().unwrap_or(&fonts.text)
        } else {
            &fonts.text
        };
        font.metrics(c, size).advance_width
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
            KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface, wl_touch},
    Connection, QueueHandle,
};
use wayland_cursor::CursorTheme;

// ── desktop entry + icon loading ──

struct DesktopEntry {
    name: String,
    icon_name: String,
    exec: String,
}

fn parse_desktop_file(path: &Path) -> Option<DesktopEntry> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut name = None;
    let mut icon = None;
    let mut exec = None;
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
        }
    }

    Some(DesktopEntry {
        name: name?,
        icon_name: icon.unwrap_or_default(),
        exec: exec.unwrap_or_default(),
    })
}

fn find_icon_file(icon_name: &str) -> Option<PathBuf> {
    // If it's already an absolute path
    if icon_name.starts_with('/') {
        let p = PathBuf::from(icon_name);
        if p.exists() { return Some(p); }
    }

    // Search in common icon directories
    // Prefer PNG over SVG since we can't load SVG; also check common sizes first
    let sizes = ["48x48", "64x64", "96x96", "128x128", "256x256", "512x512", "32x32", "scalable"];
    let categories = ["apps", "applications"];
    let themes = ["hicolor", "Adwaita", "breeze", "Papirus"];
    let extensions = ["png", "svg", "webp", "jpg", "jpeg"]; // PNG preferred, then SVG

    for theme in themes {
        for size in sizes {
            for cat in categories {
                for ext in extensions {
                    let path = PathBuf::from(format!(
                        "/usr/share/icons/{theme}/{size}/{cat}/{icon_name}.{ext}"
                    ));
                    if path.exists() { return Some(path); }

                    // Also check ~/.local/share/icons
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

    // Check /usr/share/pixmaps
    for ext in extensions {
        let path = PathBuf::from(format!("/usr/share/pixmaps/{icon_name}.{ext}"));
        if path.exists() { return Some(path); }
    }

    // NixOS: check /run/current-system/sw/share/icons
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

    // Flatpak icons (system and user)
    let flatpak_dirs = [
        "/var/lib/flatpak/exports/share/icons",
    ];
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

const ICON_SIZE: u32 = 48; // Unified icon size for grid and picker

/// Get application directories from XDG_DATA_DIRS and user directories.
/// User directories come first for priority.
fn get_application_dirs() -> Vec<String> {
    let mut dirs = Vec::new();

    // User directories first (higher priority)
    if let Ok(home) = env::var("HOME") {
        // XDG_DATA_HOME defaults to ~/.local/share
        let data_home = env::var("XDG_DATA_HOME")
            .unwrap_or_else(|_| format!("{home}/.local/share"));
        dirs.push(format!("{data_home}/applications"));

        // Flatpak user apps
        dirs.push(format!("{home}/.local/share/flatpak/exports/share/applications"));
        // NixOS user profile
        dirs.push(format!("{home}/.nix-profile/share/applications"));
    }

    // NixOS per-user profile
    if let Ok(user) = env::var("USER") {
        dirs.push(format!("/etc/profiles/per-user/{user}/share/applications"));
    }

    // XDG_DATA_DIRS - system directories
    let data_dirs = env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());

    for dir in data_dirs.split(':') {
        if !dir.is_empty() {
            dirs.push(format!("{dir}/applications"));
        }
    }

    // Flatpak system apps (may not be in XDG_DATA_DIRS)
    dirs.push("/var/lib/flatpak/exports/share/applications".to_string());

    dirs
}

fn load_desktop_entries() -> Vec<(DesktopEntry, Vec<u8>, u32, u32)> {
    let mut entries = Vec::new();
    let mut seen_names = std::collections::HashSet::new();

    // Scan directories (user dirs first for priority)
    for dir in get_application_dirs() {
        eprintln!("  scanning {}", dir);
        let Ok(read_dir) = std::fs::read_dir(&dir) else { continue };

        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().map_or(true, |e| e != "desktop") { continue; }

            let Some(de) = parse_desktop_file(&path) else { continue };
            if seen_names.contains(&de.name) { continue; }

            // Try to load icon, use empty placeholder if not found
            let (pixels, w, h) = if de.icon_name.is_empty() {
                eprintln!("    {} - no icon specified", de.name);
                (vec![0u8; (ICON_SIZE * ICON_SIZE * 4) as usize], ICON_SIZE, ICON_SIZE)
            } else if let Some(icon_path) = find_icon_file(&de.icon_name) {
                if let Some((p, w, h)) = load_icon_rgba(&icon_path, ICON_SIZE) {
                    eprintln!("    {} - scaled to {}x{}", de.name, w, h);
                    (p, w, h)
                } else {
                    eprintln!("    {} - failed to load {}", de.name, icon_path.display());
                    (vec![0u8; (ICON_SIZE * ICON_SIZE * 4) as usize], ICON_SIZE, ICON_SIZE)
                }
            } else {
                eprintln!("    {} - icon '{}' not found", de.name, de.icon_name);
                (vec![0u8; (ICON_SIZE * ICON_SIZE * 4) as usize], ICON_SIZE, ICON_SIZE)
            };

            seen_names.insert(de.name.clone());
            entries.push((de, pixels, w, h));
        }
    }

    entries
}

fn load_icon_rgba(path: &Path, target_size: u32) -> Option<(Vec<u8>, u32, u32)> {
    let bytes = std::fs::read(path).ok()?;

    // Check if it's an SVG file
    if path.extension().map_or(false, |e| e == "svg") {
        return load_svg_rgba(&bytes, target_size);
    }

    let img = image::load_from_memory(&bytes).ok()?;

    // Scale to target size if needed
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

fn load_svg_rgba(data: &[u8], target_size: u32) -> Option<(Vec<u8>, u32, u32)> {
    use resvg::usvg::{Options, Tree};
    use resvg::tiny_skia::{self, Pixmap};

    let tree = Tree::from_data(data, &Options::default()).ok()?;
    let size = tree.size();

    // Create pixmap at target size
    let mut pixmap = Pixmap::new(target_size, target_size)?;

    // Calculate scale to fit
    let scale_x = target_size as f32 / size.width();
    let scale_y = target_size as f32 / size.height();
    let scale = scale_x.min(scale_y);

    // Center the image
    let tx = (target_size as f32 - size.width() * scale) / 2.0;
    let ty = (target_size as f32 - size.height() * scale) / 2.0;

    let transform = tiny_skia::Transform::from_scale(scale, scale).post_translate(tx, ty);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    Some((pixmap.take(), target_size, target_size))
}

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
            eprintln!("  zoxide: loaded {} directories", dirs.len());
            dirs
        }
        _ => {
            eprintln!("  zoxide: failed to load (is zoxide installed?)");
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
    eprintln!("  launch: raw exec = '{}'", exec);

    let args = parse_exec_args(exec);

    if args.is_empty() {
        eprintln!("  launch: empty command for {}", name);
        return;
    }

    let program = &args[0];
    let cmd_args = &args[1..];

    eprintln!("  launch: {} -> '{}' {:?}", name, program, cmd_args);

    // Spawn detached process directly (no shell)
    match Command::new(program)
        .args(cmd_args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_) => eprintln!("  launch: spawned successfully"),
        Err(e) => eprintln!("  launch: failed: {}", e),
    }
}

/// Blit RGBA source onto ARGB8888 canvas (Wayland SHM format)
fn blit_rgba(
    canvas: &mut [u8],
    canvas_w: u32,
    canvas_h: u32,
    src: &[u8],
    src_w: u32,
    src_h: u32,
    dst_x: i32,
    dst_y: i32,
) {
    for sy in 0..src_h {
        let dy = dst_y + sy as i32;
        if dy < 0 || dy >= canvas_h as i32 {
            continue;
        }
        for sx in 0..src_w {
            let dx = dst_x + sx as i32;
            if dx < 0 || dx >= canvas_w as i32 {
                continue;
            }

            let src_idx = ((sy * src_w + sx) * 4) as usize;
            let dst_idx = ((dy as u32 * canvas_w + dx as u32) * 4) as usize;

            // RGBA -> BGRA (little-endian ARGB8888)
            let r = src[src_idx];
            let g = src[src_idx + 1];
            let b = src[src_idx + 2];
            let a = src[src_idx + 3];

            // Alpha blending with existing pixel
            if a == 255 {
                canvas[dst_idx] = b;
                canvas[dst_idx + 1] = g;
                canvas[dst_idx + 2] = r;
                canvas[dst_idx + 3] = a;
            } else if a > 0 {
                let alpha = a as u32;
                let inv_alpha = 255 - alpha;
                canvas[dst_idx] = ((b as u32 * alpha + canvas[dst_idx] as u32 * inv_alpha) / 255) as u8;
                canvas[dst_idx + 1] = ((g as u32 * alpha + canvas[dst_idx + 1] as u32 * inv_alpha) / 255) as u8;
                canvas[dst_idx + 2] = ((r as u32 * alpha + canvas[dst_idx + 2] as u32 * inv_alpha) / 255) as u8;
                canvas[dst_idx + 3] = 255;
            }
        }
    }
}

/// Blit RGBA source onto ARGB8888 canvas with scaling (nearest neighbor)
fn blit_rgba_scaled(
    canvas: &mut [u8],
    canvas_w: u32,
    canvas_h: u32,
    src: &[u8],
    src_w: u32,
    src_h: u32,
    dst_x: i32,
    dst_y: i32,
    dst_w: u32,
    dst_h: u32,
) {
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return;
    }

    for dy_offset in 0..dst_h {
        let dy = dst_y + dy_offset as i32;
        if dy < 0 || dy >= canvas_h as i32 {
            continue;
        }
        // Map destination y to source y
        let sy = (dy_offset * src_h / dst_h).min(src_h - 1);

        for dx_offset in 0..dst_w {
            let dx = dst_x + dx_offset as i32;
            if dx < 0 || dx >= canvas_w as i32 {
                continue;
            }
            // Map destination x to source x
            let sx = (dx_offset * src_w / dst_w).min(src_w - 1);

            let src_idx = ((sy * src_w + sx) * 4) as usize;
            let dst_idx = ((dy as u32 * canvas_w + dx as u32) * 4) as usize;

            // RGBA -> BGRA (little-endian ARGB8888)
            let r = src[src_idx];
            let g = src[src_idx + 1];
            let b = src[src_idx + 2];
            let a = src[src_idx + 3];

            // Alpha blending with existing pixel
            if a == 255 {
                canvas[dst_idx] = b;
                canvas[dst_idx + 1] = g;
                canvas[dst_idx + 2] = r;
                canvas[dst_idx + 3] = a;
            } else if a > 0 {
                let alpha = a as u32;
                let inv_alpha = 255 - alpha;
                canvas[dst_idx] = ((b as u32 * alpha + canvas[dst_idx] as u32 * inv_alpha) / 255) as u8;
                canvas[dst_idx + 1] = ((g as u32 * alpha + canvas[dst_idx + 1] as u32 * inv_alpha) / 255) as u8;
                canvas[dst_idx + 2] = ((r as u32 * alpha + canvas[dst_idx + 2] as u32 * inv_alpha) / 255) as u8;
                canvas[dst_idx + 3] = 255;
            }
        }
    }
}

/// Fill a rectangle with solid color (BGRA)
fn fill_rect(
    canvas: &mut [u8],
    canvas_w: u32,
    canvas_h: u32,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    color: [u8; 4],
) {
    for dy in 0..h {
        let py = y + dy as i32;
        if py < 0 || py >= canvas_h as i32 {
            continue;
        }
        for dx in 0..w {
            let px = x + dx as i32;
            if px < 0 || px >= canvas_w as i32 {
                continue;
            }
            let idx = ((py as u32 * canvas_w + px as u32) * 4) as usize;
            canvas[idx..idx + 4].copy_from_slice(&color);
        }
    }
}

fn main() {
    let startup_time = Instant::now();

    // ── env probe ──
    eprintln!("=== wlgrid layer-shell probe ===");
    for k in [
        "XDG_SESSION_TYPE", "WAYLAND_DISPLAY", "XDG_RUNTIME_DIR",
        "HYPRLAND_INSTANCE_SIGNATURE",
    ] {
        eprintln!("  {k} = {}", env::var(k).unwrap_or("<unset>".into()));
    }
    for lib in ["libwayland-client.so.0", "libwayland-egl.so.1", "libxkbcommon.so.0"] {
        let ok = unsafe { libloading::Library::new(lib).is_ok() };
        eprintln!("  {lib:36} {}", if ok { "OK" } else { "MISSING" });
    }

    // ── connect to wayland ──
    let conn = Connection::connect_to_env().unwrap();
    let (globals, mut event_queue) = registry_queue_init(&conn).unwrap();
    let qh = event_queue.handle();
    eprintln!("  wayland connection OK");

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor missing");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("layer shell missing");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm missing");
    eprintln!("  compositor + layer_shell + shm bound");

    // Load config
    let config = load_config();

    // Grid configuration from config
    let grid_w: usize = config.width.unwrap_or(6);
    let grid_h: usize = config.height.unwrap_or(4);
    let tile_size: u32 = 64;
    let tile_gap: u32 = 8;
    let num_tiles = grid_w * grid_h;

    // Load bottom bar items from config
    let dock: Vec<DockEntry> = config.bottom_bar
        .as_ref()
        .map(|bb| parse_bottom_bar_options(&bb.options))
        .unwrap_or_default()
        .into_iter()
        .map(|item| {
            eprintln!("  bar item: {} -> {}", item.name, item.exec);
            DockEntry {
                name: item.name,
                exec: item.exec,
            }
        })
        .collect();
    eprintln!("  loaded {} bottom bar items", dock.len());

    // Get dock font size and opacity from config
    let dock_font_size = config.bottom_bar.as_ref()
        .and_then(|bb| bb.font)
        .unwrap_or(16.0);
    let opacity = config.opacity.unwrap_or(1.0).clamp(0.0, 1.0);
    eprintln!("  dock font size: {}, opacity: {}", dock_font_size, opacity);

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
    layer.set_size(surface_w, surface_h);
    layer.commit();
    eprintln!("  layer surface {}x{}, waiting for configure...", surface_w, surface_h);

    let pool = SlotPool::new((surface_w * surface_h * 4) as usize, &shm).expect("pool alloc failed");

    // Load cursor theme for setting cursor on pointer enter
    let cursor_theme = CursorTheme::load(&conn, shm.wl_shm().clone(), 24).ok();
    let cursor_surface = compositor.create_surface(&qh);
    eprintln!("  cursor theme loaded: {}", cursor_theme.is_some());

    // Try to load from cache first (fast path)
    let (icons, fonts) = if let Some(cache) = load_cache() {
        // Load icons from cache
        let icons: Vec<Icon> = cache.icons.into_iter().map(|ci| Icon {
            name: ci.name,
            exec: ci.exec,
            pixels: ci.pixels,
            width: ci.width,
            height: ci.height,
        }).collect();

        // Load fonts from cached paths
        let fonts = cache.text_font_path.as_ref().and_then(|text_path| {
            load_fonts_from_paths(text_path, cache.symbols_font_path.as_deref())
        });

        eprintln!("  cache: loaded {} icons", icons.len());
        (icons, fonts)
    } else {
        // Cache miss - do full load (slow path)
        eprintln!("  cache: miss, doing full load");

        // Load desktop entries
        let desktop_entries = load_desktop_entries();
        let icons: Vec<Icon> = desktop_entries
            .into_iter()
            .map(|(de, pixels, w, h)| Icon {
                name: de.name,
                exec: de.exec,
                pixels,
                width: w,
                height: h,
            })
            .collect();
        eprintln!("  loaded {} desktop entries", icons.len());

        // Load fonts with search
        let fonts_with_paths = load_fonts_with_search();
        let fonts = fonts_with_paths.as_ref().map(|fp| Fonts {
            text: fp.fonts.text.clone(),
            symbols: fp.fonts.symbols.clone(),
        });

        // Save cache for next time
        if let Some(ref fp) = fonts_with_paths {
            save_cache(&icons, Some(&fp.text_path), fp.symbols_path.as_deref());
        } else {
            save_cache(&icons, None, None);
        }

        (icons, fonts)
    };

    if fonts.is_some() {
        eprintln!("  fonts loaded successfully");
    }

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
    eprintln!("  restored {} tiles from saved state", restored_count);

    let mut app = App {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        shm,
        exit: false,
        first_configure: true,
        pool,
        width: surface_w,
        height: surface_h,
        layer,
        keyboard: None,
        pointer: None,
        touch: None,
        touch_start: None,
        touch_drag_from: None,
        grid_w,
        grid_h,
        tile_size,
        tile_gap,
        tiles,
        icons,
        icons_checksum: compute_checksum(),
        pointer_pos: (0.0, 0.0),
        hovered_tile: Some(0),  // start with first tile focused
        press_start: None,
        drag_from: None,
        drag_threshold: 5.0,
        dirty: true,
        frame_pending: false,
        picker_target: None,
        picker_scroll: 0,
        picker_hovered: None,
        picker_search: String::new(),
        dock,
        hovered_dock: None,
        dock_font_size,
        fonts,
        opacity,
        search_query: String::new(),
        zoxide_dirs: load_zoxide_dirs(),
        search_engines: config.search_engines
            .as_ref()
            .map(|s| parse_search_engines(s))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(default_search_engines),
        hovered_search_engine: None,
        search_types: config.search
            .as_ref()
            .map(|s| parse_search_config(s))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(default_search_types),
        cursor_theme,
        cursor_surface,
        pointer_enter_serial: 0,
        startup_time,
    };

    loop {
        event_queue.blocking_dispatch(&mut app).unwrap();
        if app.exit { break; }
    }

    // Save state before exiting
    save_state(&app.tiles, &app.icons);
    eprintln!("  exiting");
}

struct Icon {
    name: String,
    exec: String,
    pixels: Vec<u8>,
    width: u32,
    height: u32,
}

struct DockEntry {
    name: String,
    exec: String,
}

const DOCK_HEIGHT: u32 = 64;

struct App {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    exit: bool,
    first_configure: bool,
    pool: SlotPool,
    width: u32,
    height: u32,
    layer: LayerSurface,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    touch: Option<wl_touch::WlTouch>,
    // Touch state
    touch_start: Option<(f64, f64, usize)>, // (x, y, tile_index) when touch began
    touch_drag_from: Option<usize>,
    // Grid config
    grid_w: usize,
    grid_h: usize,
    tile_size: u32,
    tile_gap: u32,
    // Grid state: which tiles have icons (index into icons vec)
    tiles: Vec<Option<usize>>,
    icons: Vec<Icon>,
    icons_checksum: u64,  // checksum when icons were loaded
    // Pointer state (Phase 3 & 4)
    pointer_pos: (f64, f64),
    hovered_tile: Option<usize>,  // shared by mouse and keyboard
    press_start: Option<(f64, f64, usize)>, // (x, y, tile_index) when pressed
    drag_from: Option<usize>,
    drag_threshold: f64,
    // Rendering state
    dirty: bool,
    frame_pending: bool,
    // Picker state
    picker_target: Option<usize>,    // which tile we're picking for (None = closed)
    picker_scroll: usize,            // scroll offset in picker list
    picker_hovered: Option<usize>,   // which picker item is hovered (visual index)
    picker_search: String,           // search filter for picker
    // Dock state
    dock: Vec<DockEntry>,
    hovered_dock: Option<usize>,
    dock_font_size: f32,
    // Fonts for text rendering (text + symbols)
    fonts: Option<Fonts>,
    // Appearance
    opacity: f32,
    // Search state (zoxide directories)
    search_query: String,
    zoxide_dirs: Vec<String>,
    search_engines: Vec<SearchEngine>,
    hovered_search_engine: Option<usize>,
    search_types: Vec<SearchType>,
    // Cursor
    cursor_theme: Option<CursorTheme>,
    cursor_surface: wl_surface::WlSurface,
    pointer_enter_serial: u32,
    // Startup timing
    startup_time: Instant,
}

impl App {
    fn required_size(&self) -> (u32, u32) {
        let w = 16 + self.grid_w as u32 * self.tile_size + self.grid_w.saturating_sub(1) as u32 * self.tile_gap;
        let h = 16 + self.grid_h as u32 * self.tile_size + self.grid_h.saturating_sub(1) as u32 * self.tile_gap;
        (w, h)
    }

    fn tile_rect(&self, index: usize) -> (i32, i32, u32, u32) {
        let col = index % self.grid_w;
        let row = index / self.grid_w;
        let x = 8 + (col as u32 * (self.tile_size + self.tile_gap)) as i32;
        let y = 8 + (row as u32 * (self.tile_size + self.tile_gap)) as i32;
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

    fn dock_bar_y(&self) -> i32 {
        // Dock starts after the grid
        let grid_h = 16 + self.grid_h as u32 * self.tile_size + self.grid_h.saturating_sub(1) as u32 * self.tile_gap;
        grid_h as i32
    }

    fn dock_item_at(&self, x: f64, y: f64) -> Option<usize> {
        if self.dock.is_empty() {
            return None;
        }
        let dock_y = self.dock_bar_y();
        if y < dock_y as f64 || y >= (dock_y as f64 + DOCK_HEIGHT as f64) {
            return None;
        }
        // Smaller hitboxes (60% of spacing, centered) to prevent accidental clicks
        let item_spacing = self.width / self.dock.len() as u32;
        let hitbox_width = (item_spacing as f64 * 0.6) as u32;
        let hitbox_margin = (item_spacing - hitbox_width) / 2;
        for i in 0..self.dock.len() {
            let start_x = i as u32 * item_spacing + hitbox_margin;
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
            eprintln!("  request_frame: scheduled");
        }
    }

    /// Check if desktop entries have changed and reload icons if so.
    /// Returns true if icons were reloaded.
    fn refresh_icons_if_needed(&mut self) -> bool {
        let current_checksum = compute_checksum();
        if current_checksum == self.icons_checksum {
            return false;
        }

        eprintln!("  picker: desktop entries changed, reloading icons");

        // Save current tile -> name mappings before reload
        let tile_names: Vec<Option<String>> = self.tiles.iter()
            .map(|opt| opt.and_then(|idx| self.icons.get(idx).map(|i| i.name.clone())))
            .collect();

        // Reload desktop entries
        let desktop_entries = load_desktop_entries();
        self.icons = desktop_entries
            .into_iter()
            .map(|(de, pixels, w, h)| Icon {
                name: de.name,
                exec: de.exec,
                pixels,
                width: w,
                height: h,
            })
            .collect();
        eprintln!("  picker: reloaded {} icons", self.icons.len());

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
        // Center the picker in the surface
        let cols = Self::PICKER_COLS as u32;
        let rows = Self::PICKER_VISIBLE_ROWS as u32;
        let pw = 16 + cols * Self::PICKER_ITEM_WIDTH + (cols - 1) * Self::PICKER_ITEM_GAP;
        let ph = 16 + Self::PICKER_SEARCH_HEIGHT + 8 + rows * Self::PICKER_ITEM_HEIGHT + (rows - 1) * Self::PICKER_ITEM_GAP;
        let px = (self.width as i32 - pw as i32) / 2;
        let py = (self.height as i32 - ph as i32) / 2;
        (px, py, pw, ph)
    }

    fn picker_items_y(&self) -> i32 {
        let (_, py, _, _) = self.picker_rect();
        py + 8 + Self::PICKER_SEARCH_HEIGHT as i32 + 8
    }

    fn picker_item_rect(&self, index: usize) -> (i32, i32, u32, u32) {
        let (px, _, _, _) = self.picker_rect();
        let items_y = self.picker_items_y();
        let col = index % Self::PICKER_COLS;
        let row = index / Self::PICKER_COLS;
        let x = px + 8 + (col as u32 * (Self::PICKER_ITEM_WIDTH + Self::PICKER_ITEM_GAP)) as i32;
        let y = items_y + (row as u32 * (Self::PICKER_ITEM_HEIGHT + Self::PICKER_ITEM_GAP)) as i32;
        (x, y, Self::PICKER_ITEM_WIDTH, Self::PICKER_ITEM_HEIGHT)
    }

    /// Returns indices of icons matching the picker search filter
    fn filtered_icon_indices(&self) -> Vec<usize> {
        if self.picker_search.is_empty() {
            (0..self.icons.len()).collect()
        } else {
            let query = self.picker_search.to_lowercase();
            self.icons.iter()
                .enumerate()
                .filter(|(_, icon)| icon.name.to_lowercase().contains(&query))
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
        if let Some(dir) = self.zoxide_dirs.iter().find(|d| {
            d.rsplit('/').next().unwrap_or(d).to_lowercase().starts_with(&query)
        }) {
            return Some(dir);
        }

        // Then try contains match on full path
        self.zoxide_dirs.iter().find(|d| d.to_lowercase().contains(&query)).map(|s| s.as_str())
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
        let mut prefix_matches: Vec<usize> = self.icons.iter()
            .enumerate()
            .filter(|(_, icon)| icon.name.to_lowercase().starts_with(&query))
            .map(|(i, _)| i)
            .collect();

        // Then: contains matches (excluding already matched)
        let contains_matches: Vec<usize> = self.icons.iter()
            .enumerate()
            .filter(|(i, icon)| {
                !prefix_matches.contains(i) &&
                icon.name.to_lowercase().contains(&query)
            })
            .map(|(i, _)| i)
            .collect();

        prefix_matches.extend(contains_matches);
        prefix_matches
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

    fn search_engine_at(&self, x: f64, y: f64) -> Option<usize> {
        // Only active when search query exists and no zoxide match
        if self.search_query.is_empty() || self.find_best_zoxide_match().is_some() {
            return None;
        }

        let fonts = self.fonts.as_ref()?;
        let w = self.width;

        let font_size = 24.0;
        let tw = text_width(fonts, &self.search_query, font_size) as u32;
        let box_w = tw.max(200) + 32;
        let box_x = (w as i32 - box_w as i32) / 2;
        let box_y = 8;

        let btn_font_size = 14.0;
        let btn_y = box_y + 45;
        let btn_h = 28i32;
        let btn_gap = 8i32;

        // Check if y is in button row
        if y < btn_y as f64 || y >= (btn_y + btn_h) as f64 {
            return None;
        }

        // Calculate button positions
        let btn_widths: Vec<u32> = self.search_engines.iter()
            .map(|e| text_width(fonts, &e.name, btn_font_size) as u32 + 16)
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
                eprintln!("  opening {} with {}", dir, editor);
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
        eprintln!("  opening {} with xdg-open", dir);
    }

    fn draw(&mut self, qh: &QueueHandle<Self>) {
        self.dirty = false;
        let t0 = Instant::now();

        let (w, h) = (self.width, self.height);
        let num_tiles = self.grid_w * self.grid_h;
        let tile_size = self.tile_size;
        let hovered = self.hovered_tile;
        let drag_from = self.drag_from;
        let pointer_pos = self.pointer_pos;

        // Build draw list: (tile_index, tile_x, tile_y, Option<icon_index>)
        // NO CLONING - just indices
        let draw_list: Vec<_> = (0..num_tiles)
            .map(|i| {
                let (tx, ty, _, _) = self.tile_rect(i);
                let icon_idx = self.tiles.get(i).and_then(|slot| *slot);
                (i, tx, ty, icon_idx)
            })
            .collect();

        // Get dragged icon index
        let dragged_icon_idx = drag_from.and_then(|from| {
            self.tiles.get(from).and_then(|slot| *slot)
        });

        // Pre-compute drop target rect
        let drop_target_rect = drag_from.and_then(|from| {
            hovered.filter(|&to| from != to).map(|to| self.tile_rect(to))
        });

        // Pre-compute picker data
        // Returns: (rect, hovered_visual_idx, items: Vec<(icon_idx, name, rect)>, search_text)
        let picker_data: Option<((i32, i32, u32, u32), Option<usize>, Vec<(usize, String, (i32, i32, u32, u32))>, String)> = if self.picker_target.is_some() {
            let rect = self.picker_rect();
            let picker_hovered = self.picker_hovered;
            let picker_scroll = self.picker_scroll;
            let filtered = self.filtered_icon_indices();
            let visible_count = Self::PICKER_COLS * Self::PICKER_VISIBLE_ROWS;
            let items: Vec<_> = (0..visible_count)
                .filter_map(|i| {
                    let filtered_idx = picker_scroll + i;
                    if filtered_idx >= filtered.len() { return None; }
                    let icon_idx = filtered[filtered_idx];
                    let name = self.icons[icon_idx].name.clone();
                    Some((icon_idx, name, self.picker_item_rect(i)))
                })
                .collect();
            Some((rect, picker_hovered, items, self.picker_search.clone()))
        } else {
            None
        };

        // Pre-compute dock data
        let hovered_dock = self.hovered_dock;
        let dock_y = if self.dock.is_empty() { 0 } else { self.dock_bar_y() };

        // Pre-compute search box data (unified search: folders + desktop)
        // Returns: (query_text, has_match, show_search_engines, first_icon_idx)
        let search_data: Option<(String, bool, bool, Option<usize>)> = if !self.search_query.is_empty() {
            let best_match = self.find_best_search_match();
            match best_match {
                SearchMatch::Folder(ref dir) => {
                    let short = dir.rsplit('/').next().unwrap_or(dir);
                    let display_text = format!("{} → {}", self.search_query, short);
                    Some((display_text, true, false, None))
                }
                SearchMatch::Desktop(ref indices) => {
                    // Show first desktop match in search box with icon
                    let first_idx = indices.first().copied();
                    let first_name = first_idx
                        .and_then(|i| self.icons.get(i))
                        .map(|icon| icon.name.as_str())
                        .unwrap_or("");
                    let display_text = format!("{} → {}", self.search_query, first_name);
                    Some((display_text, true, false, first_idx))
                }
                SearchMatch::None => {
                    Some((self.search_query.clone(), false, true, None))
                }
            }
        } else {
            None
        };

        // Pre-compute search engine button data
        let search_engine_names: Vec<String> = self.search_engines.iter()
            .map(|e| e.name.clone())
            .collect();
        let hovered_search_engine = self.hovered_search_engine;

        let t1 = Instant::now();
        eprintln!("  draw: precompute {:.2}ms", (t1 - t0).as_secs_f64() * 1000.0);

        let stride = w as i32 * 4;
        let (buffer, canvas) = self.pool
            .create_buffer(w as i32, h as i32, stride, wl_shm::Format::Argb8888)
            .expect("create buffer");

        let t2 = Instant::now();
        eprintln!("  draw: create_buffer {:.2}ms", (t2 - t1).as_secs_f64() * 1000.0);

        // Aero Glass style background with gradient (BGRA)
        let bg_alpha = (self.opacity * 255.0) as u8;
        for y in 0..h {
            // Vertical gradient: lighter at top, darker at bottom
            // With a subtle blue/purple tint for that obsidian look
            let t = y as f32 / h as f32;
            let base = 0x10 + ((1.0 - t) * 0x10 as f32) as u8; // 0x20 at top, 0x10 at bottom
            let b = base + 4; // slight blue tint
            let g = base;
            let r = base + 2; // tiny bit of warmth

            let row_start = (y * w * 4) as usize;
            for x in 0..w {
                let idx = row_start + (x * 4) as usize;
                canvas[idx] = b;
                canvas[idx + 1] = g;
                canvas[idx + 2] = r;
                canvas[idx + 3] = bg_alpha;
            }
        }

        // Top shine line (bright highlight for glass effect)
        for x in 0..w {
            let idx = (x * 4) as usize;
            canvas[idx] = 0x60;     // B
            canvas[idx + 1] = 0x58; // G
            canvas[idx + 2] = 0x50; // R
            canvas[idx + 3] = bg_alpha;
        }

        let t3 = Instant::now();
        eprintln!("  draw: clear {:.2}ms", (t3 - t2).as_secs_f64() * 1000.0);

        // Draw grid of tiles with glass bevel effect
        for (i, tx, ty, icon_idx) in &draw_list {
            let is_hovered = hovered == Some(*i);

            // Tile background with gradient (lighter at top for glass effect)
            for row in 0..tile_size {
                let py = *ty + row as i32;
                if py < 0 || py >= h as i32 { continue; }

                // Gradient within tile: brighter at top
                let t = row as f32 / tile_size as f32;
                let (base_top, base_bot) = if is_hovered {
                    (0x55u8, 0x38u8)
                } else {
                    (0x40u8, 0x25u8)
                };
                let base = base_top - ((t * (base_top - base_bot) as f32) as u8);
                let b = base + 3; // subtle blue
                let g = base;
                let r = base + 1;

                for col in 0..tile_size {
                    let px = *tx + col as i32;
                    if px < 0 || px >= w as i32 { continue; }
                    let idx = ((py as u32 * w + px as u32) * 4) as usize;
                    canvas[idx] = b;
                    canvas[idx + 1] = g;
                    canvas[idx + 2] = r;
                    canvas[idx + 3] = bg_alpha;
                }
            }

            // Top edge highlight (glass shine)
            for col in 0..tile_size {
                let px = *tx + col as i32;
                if px < 0 || px >= w as i32 { continue; }
                let py = *ty;
                if py >= 0 && py < h as i32 {
                    let idx = ((py as u32 * w + px as u32) * 4) as usize;
                    canvas[idx] = 0x70;     // B - brighter
                    canvas[idx + 1] = 0x68; // G
                    canvas[idx + 2] = 0x60; // R
                    canvas[idx + 3] = bg_alpha;
                }
            }

            // Bottom edge shadow
            for col in 0..tile_size {
                let px = *tx + col as i32;
                if px < 0 || px >= w as i32 { continue; }
                let py = *ty + tile_size as i32 - 1;
                if py >= 0 && py < h as i32 {
                    let idx = ((py as u32 * w + px as u32) * 4) as usize;
                    canvas[idx] = 0x18;
                    canvas[idx + 1] = 0x15;
                    canvas[idx + 2] = 0x12;
                    canvas[idx + 3] = bg_alpha;
                }
            }

            // Draw icon if present (skip if being dragged)
            if let Some(idx) = icon_idx {
                if drag_from != Some(*i) {
                    if let Some(icon) = self.icons.get(*idx) {
                        let ix = tx + (tile_size as i32 - icon.width as i32) / 2;
                        let iy = ty + (tile_size as i32 - icon.height as i32) / 2;
                        blit_rgba(canvas, w, h, &icon.pixels, icon.width, icon.height, ix, iy);
                    }
                }
            }
        }

        let t4 = Instant::now();
        eprintln!("  draw: tiles {:.2}ms", (t4 - t3).as_secs_f64() * 1000.0);

        // Draw dock bar with glass effect
        if !self.dock.is_empty() {
            // Dock background with gradient
            for row in 0..DOCK_HEIGHT {
                let py = dock_y + row as i32;
                if py < 0 || py >= h as i32 { continue; }

                let t = row as f32 / DOCK_HEIGHT as f32;
                let base = 0x18 + ((1.0 - t) * 0x08 as f32) as u8;
                let b = base + 2;
                let g = base;
                let r = base + 1;

                for x in 0..w {
                    let idx = ((py as u32 * w + x) * 4) as usize;
                    canvas[idx] = b;
                    canvas[idx + 1] = g;
                    canvas[idx + 2] = r;
                    canvas[idx + 3] = bg_alpha;
                }
            }

            // Top separator line (subtle shine)
            if dock_y >= 0 && dock_y < h as i32 {
                for x in 0..w {
                    let idx = ((dock_y as u32 * w + x) * 4) as usize;
                    canvas[idx] = 0x50;
                    canvas[idx + 1] = 0x48;
                    canvas[idx + 2] = 0x40;
                    canvas[idx + 3] = bg_alpha;
                }
            }

            // Draw dock items evenly spaced
            let font_size = self.dock_font_size;
            let item_count = self.dock.len() as u32;
            let item_spacing = w / item_count;

            for (i, entry) in self.dock.iter().enumerate() {
                let center_x = (i as u32 * item_spacing + item_spacing / 2) as i32;
                let is_hovered = hovered_dock == Some(i);
                let item_start_x = i as u32 * item_spacing;

                // Full-width hover shading (covers entire item portion)
                if is_hovered {
                    for row in 0..DOCK_HEIGHT {
                        let py = dock_y + row as i32;
                        if py < 0 || py >= h as i32 { continue; }

                        // Brighter gradient for hovered section
                        let t = row as f32 / DOCK_HEIGHT as f32;
                        let base = 0x30 + ((1.0 - t) * 0x15 as f32) as u8;

                        for col in 0..item_spacing {
                            let px = item_start_x + col;
                            if px >= w { continue; }
                            let idx = ((py as u32 * w + px) * 4) as usize;
                            canvas[idx] = base + 4;     // B
                            canvas[idx + 1] = base + 2; // G
                            canvas[idx + 2] = base;     // R
                            canvas[idx + 3] = bg_alpha;
                        }
                    }
                }

                // Text color: brighter when hovered
                let text_color = if is_hovered {
                    [0xFF, 0xFF, 0xFF, 0xFF] // bright white
                } else {
                    [0xAA, 0xAA, 0xAA, 0xFF] // dimmer
                };

                if let Some(ref fonts) = self.fonts {
                    let tw = text_width(fonts, &entry.name, font_size) as i32;
                    let text_x = center_x - tw / 2;
                    let text_y = dock_y + (DOCK_HEIGHT as i32 / 2) + (font_size as i32 / 3);
                    render_text(canvas, w, h, fonts, &entry.name, text_x, text_y, font_size, text_color);
                }
            }
        }

        // Draw drop target highlight during drag
        if drag_from.is_some() {
            if let Some((tx, ty, tw, th)) = drop_target_rect {
                draw_rect_outline(canvas, w, h, tx, ty, tw, th, [0x00, 0xFF, 0x00, 0xFF], 2);
            }

            // Draw dragged icon following cursor
            if let Some(idx) = dragged_icon_idx {
                if let Some(icon) = self.icons.get(idx) {
                    let x = pointer_pos.0 as i32 - icon.width as i32 / 2;
                    let y = pointer_pos.1 as i32 - icon.height as i32 / 2;
                    blit_rgba(canvas, w, h, &icon.pixels, icon.width, icon.height, x, y);
                }
            }
        }

        let t5 = Instant::now();
        eprintln!("  draw: drag overlay {:.2}ms", (t5 - t4).as_secs_f64() * 1000.0);

        // Draw picker if open
        if let Some(((px, py, pw, ph), picker_hovered, items, search_text)) = picker_data {
            // Dim background
            for y_pos in 0..h {
                for x_pos in 0..w {
                    let idx = ((y_pos * w + x_pos) * 4) as usize;
                    canvas[idx] = canvas[idx] / 2;
                    canvas[idx + 1] = canvas[idx + 1] / 2;
                    canvas[idx + 2] = canvas[idx + 2] / 2;
                }
            }

            // Picker background
            fill_rect(canvas, w, h, px, py, pw, ph, [0x1A, 0x1A, 0x1A, 0xFF]);
            draw_rect_outline(canvas, w, h, px, py, pw, ph, [0x55, 0x55, 0x55, 0xFF], 2);

            // Draw search box at top
            let search_box_y = py + 8;
            let search_box_w = pw - 16;
            fill_rect(canvas, w, h, px + 8, search_box_y, search_box_w, Self::PICKER_SEARCH_HEIGHT, [0x2D, 0x2D, 0x2D, 0xFF]);
            draw_rect_outline(canvas, w, h, px + 8, search_box_y, search_box_w, Self::PICKER_SEARCH_HEIGHT, [0x55, 0x55, 0x55, 0xFF], 1);
            if let Some(ref fonts) = self.fonts {
                let display = if search_text.is_empty() { "Type to search...".to_string() } else { search_text };
                let color = if self.picker_search.is_empty() { [0x88, 0x88, 0x88, 0xFF] } else { [0xFF, 0xFF, 0xFF, 0xFF] };
                render_text(canvas, w, h, fonts, &display, px + 12, search_box_y + 22, 16.0, color);
            }

            // Draw picker items
            let name_font_size = 10.0;
            for (visual_idx, (icon_idx, name, (ix, iy, iw, ih))) in items.iter().enumerate() {
                // Item background (highlight if hovered)
                let bg = if picker_hovered == Some(visual_idx) {
                    [0x46, 0x46, 0x46, 0xFF]
                } else {
                    [0x2D, 0x2D, 0x2D, 0xFF]
                };
                fill_rect(canvas, w, h, *ix, *iy, *iw, *ih, bg);

                // Draw icon (centered horizontally, at top of cell)
                if let Some(icon) = self.icons.get(*icon_idx) {
                    let ox = ix + (*iw as i32 - icon.width as i32) / 2;
                    let oy = *iy + 4;  // 4px from top
                    blit_rgba(canvas, w, h, &icon.pixels, icon.width, icon.height, ox, oy);
                }

                // Draw app name below icon
                if let Some(ref fonts) = self.fonts {
                    // Truncate name to fit
                    let max_chars = 10;
                    let display_name: String = if name.chars().count() > max_chars {
                        format!("{}…", &name.chars().take(max_chars - 1).collect::<String>())
                    } else {
                        name.clone()
                    };
                    let tw = text_width(fonts, &display_name, name_font_size);
                    let text_x = ix + (*iw as i32 - tw as i32) / 2;
                    let text_y = iy + *ih as i32 - 4;  // 4px from bottom
                    render_text(canvas, w, h, fonts, &display_name, text_x, text_y, name_font_size, [0xCC, 0xCC, 0xCC, 0xFF]);
                }
            }

            // Draw tooltip for hovered item (on top of everything)
            if let Some(hovered_idx) = picker_hovered {
                if let Some((_, name, (ix, iy, iw, _ih))) = items.get(hovered_idx) {
                    // Only show tooltip if name was truncated
                    if name.chars().count() > 10 {
                        if let Some(ref fonts) = self.fonts {
                            let tooltip_font_size = 12.0;
                            let tw = text_width(fonts, name, tooltip_font_size) as u32;
                            let padding = 6u32;
                            let tooltip_w = tw + padding * 2;
                            let tooltip_h = 20u32;

                            // Position tooltip above the item, centered
                            let tooltip_x = ix + (*iw as i32 - tooltip_w as i32) / 2;
                            let tooltip_y = *iy - tooltip_h as i32 - 4;

                            // Clamp to picker bounds
                            let tooltip_x = tooltip_x.max(px + 4).min(px + pw as i32 - tooltip_w as i32 - 4);
                            let tooltip_y = tooltip_y.max(py + 4);

                            // Draw tooltip background
                            fill_rect(canvas, w, h, tooltip_x, tooltip_y, tooltip_w, tooltip_h, [0x00, 0x00, 0x00, 0xEE]);
                            draw_rect_outline(canvas, w, h, tooltip_x, tooltip_y, tooltip_w, tooltip_h, [0x88, 0x88, 0x88, 0xFF], 1);

                            // Draw tooltip text
                            render_text(canvas, w, h, fonts, name, tooltip_x + padding as i32, tooltip_y + 15, tooltip_font_size, [0xFF, 0xFF, 0xFF, 0xFF]);
                        }
                    }
                }
            }
        }

        let t6 = Instant::now();
        eprintln!("  draw: picker {:.2}ms", (t6 - t5).as_secs_f64() * 1000.0);

        // Draw search box if query is not empty
        if let Some((display_text, has_match, show_engines, first_icon_idx)) = search_data {
            if let Some(ref fonts) = self.fonts {
                let font_size = 24.0;
                let btn_font_size = 14.0;
                let btn_gap = 8i32;
                let tw = text_width(fonts, &display_text, font_size) as u32;

                // Calculate button widths to size the box properly
                let btn_widths: Vec<u32> = if show_engines && !search_engine_names.is_empty() {
                    search_engine_names.iter()
                        .map(|name| text_width(fonts, name, btn_font_size) as u32 + 16)
                        .collect()
                } else {
                    vec![]
                };
                let buttons_total_w = if btn_widths.is_empty() {
                    0u32
                } else {
                    btn_widths.iter().sum::<u32>() + (btn_widths.len() as u32 - 1) * btn_gap as u32
                };

                // Icon size for search box (scaled down)
                let icon_size = 32u32;
                let icon_padding = if first_icon_idx.is_some() { icon_size + 8 } else { 0 };

                // Box width must fit icon, text, and buttons
                let box_w = (tw + icon_padding).max(200).max(buttons_total_w + 32) + 32;
                let box_h = if show_engines { 88 } else { 40 };
                let box_x = (w as i32 - box_w as i32) / 2;
                let box_y = 8;

                // Background with glass effect
                for row in 0..box_h {
                    let py = box_y + row as i32;
                    if py < 0 || py >= h as i32 { continue; }
                    let t = row as f32 / box_h as f32;
                    let base = 0x18 - (t * 8.0) as u8;
                    for col in 0..box_w {
                        let px = box_x + col as i32;
                        if px < 0 || px >= w as i32 { continue; }
                        let idx = ((py as u32 * w + px as u32) * 4) as usize;
                        canvas[idx] = base + 8;
                        canvas[idx + 1] = base + 4;
                        canvas[idx + 2] = base;
                        canvas[idx + 3] = 0xEE;
                    }
                }
                draw_rect_outline(canvas, w, h, box_x, box_y, box_w, box_h, [0x60, 0x60, 0x80, 0xFF], 1);

                // Draw icon if we have a desktop match
                if let Some(icon_idx) = first_icon_idx {
                    if let Some(icon) = self.icons.get(icon_idx) {
                        let icon_x = box_x + 8;
                        let icon_y = box_y + (box_h as i32 - icon_size as i32) / 2;
                        // Scale icon to fit
                        blit_rgba_scaled(canvas, w, h, &icon.pixels, icon.width, icon.height,
                                        icon_x, icon_y, icon_size, icon_size);
                    }
                }

                // Query text (shifted right if icon present)
                let text_x = box_x + 16 + icon_padding as i32;
                let text_y = box_y + 28;
                let text_color = if has_match {
                    [0xFF, 0xFF, 0xFF, 0xFF]
                } else {
                    [0xCC, 0xCC, 0xCC, 0xFF]
                };
                render_text(canvas, w, h, fonts, &display_text, text_x, text_y, font_size, text_color);

                // Draw search engine buttons if no zoxide match
                if show_engines && !btn_widths.is_empty() {
                    let btn_y = box_y + 48;
                    let btn_h = 28u32;

                    let total_w = buttons_total_w as i32;
                    let mut btn_x = box_x + (box_w as i32 - total_w) / 2;

                    for (i, name) in search_engine_names.iter().enumerate() {
                        let btn_w = btn_widths[i];
                        let is_hovered = hovered_search_engine == Some(i);

                        // Button background
                        let bg_color = if is_hovered {
                            [0x60, 0x50, 0x40, 0xFF]
                        } else {
                            [0x30, 0x28, 0x20, 0xFF]
                        };
                        fill_rect(canvas, w, h, btn_x, btn_y, btn_w, btn_h, bg_color);

                        // Button text
                        let txt_color = if is_hovered {
                            [0xFF, 0xFF, 0xFF, 0xFF]
                        } else {
                            [0xBB, 0xBB, 0xBB, 0xFF]
                        };
                        let txt_x = btn_x + 8;
                        let txt_y = btn_y + 19;
                        render_text(canvas, w, h, fonts, name, txt_x, txt_y, btn_font_size, txt_color);

                        btn_x += btn_w as i32 + btn_gap;
                    }
                }
            }
        }

        self.layer.wl_surface().damage_buffer(0, 0, w as i32, h as i32);
        self.layer.wl_surface().frame(qh, self.layer.wl_surface().clone());
        buffer.attach_to(self.layer.wl_surface()).expect("buffer attach");
        self.layer.commit();
        self.frame_pending = true;

        let t7 = Instant::now();
        eprintln!("  draw: commit {:.2}ms, TOTAL {:.2}ms", (t7 - t6).as_secs_f64() * 1000.0, (t7 - t0).as_secs_f64() * 1000.0);
    }
}

/// Draw rectangle outline
fn draw_rect_outline(
    canvas: &mut [u8],
    canvas_w: u32,
    canvas_h: u32,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    color: [u8; 4],
    thickness: u32,
) {
    // Top edge
    fill_rect(canvas, canvas_w, canvas_h, x, y, w, thickness, color);
    // Bottom edge
    fill_rect(canvas, canvas_w, canvas_h, x, y + h as i32 - thickness as i32, w, thickness, color);
    // Left edge
    fill_rect(canvas, canvas_w, canvas_h, x, y, thickness, h, color);
    // Right edge
    fill_rect(canvas, canvas_w, canvas_h, x + w as i32 - thickness as i32, y, thickness, h, color);
}

// ── handler impls (mostly stubs) ──

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.frame_pending = false;
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
    fn configure(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &LayerSurface, cfg: LayerSurfaceConfigure, _: u32) {
        let old_w = self.width;
        let old_h = self.height;
        if cfg.new_size.0 != 0 { self.width = cfg.new_size.0; }
        if cfg.new_size.1 != 0 { self.height = cfg.new_size.1; }

        // Resize pool if surface got larger
        if self.width != old_w || self.height != old_h {
            let new_size = (self.width * self.height * 4) as usize;
            if self.pool.len() < new_size {
                eprintln!("  resizing pool: {} -> {}", self.pool.len(), new_size);
                self.pool.resize(new_size).expect("pool resize");
            }
        }

        if self.first_configure {
            self.first_configure = false;
            eprintln!("  configured {}x{}, drawing first frame", self.width, self.height);
            self.draw(qh);
            eprintln!("  Time to interactive: {:.2}ms", self.startup_time.elapsed().as_secs_f64() * 1000.0);
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
        eprintln!("  key: {:?}", event.keysym);

        if event.keysym == Keysym::Escape {
            if !self.search_query.is_empty() {
                // Clear search first
                self.search_query.clear();
                self.dirty = true;
                self.request_frame(qh);
            } else if self.picker_target.is_some() {
                // Close picker
                eprintln!("  picker: closed (escape)");
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
                eprintln!("  picker search: '{}'", self.picker_search);
                self.dirty = true;
                self.request_frame(qh);
            } else if !self.search_query.is_empty() {
                self.search_query.pop();
                eprintln!("  search: '{}'", self.search_query);
                self.dirty = true;
                self.request_frame(qh);
            }
        } else if event.keysym == Keysym::Delete {
            // Delete key removes the focused tile's entry (same as right-click)
            if self.picker_target.is_none() {
                if let Some(tile) = self.hovered_tile {
                    if self.tiles[tile].is_some() {
                        self.tiles[tile] = None;
                        eprintln!("  removed tile {} (delete key)", tile);
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
                            eprintln!("  tab complete: '{}'", self.search_query);
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
                    eprintln!("  zoxide add: {}", path);
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
                            // No match - use first search engine
                            if let Some(engine) = self.search_engines.first() {
                                let query = self.search_query.replace(' ', "+");
                                let url = engine.url_template.replace("{}", &query);
                                let _ = Command::new("xdg-open")
                                    .arg(&url)
                                    .stdin(std::process::Stdio::null())
                                    .stdout(std::process::Stdio::null())
                                    .stderr(std::process::Stdio::null())
                                    .spawn();
                                eprintln!("  search {} for: {}", engine.name, self.search_query);
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
                        eprintln!("  picker: selected {} for tile {}", self.icons[icon_idx].name, target);
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
                    eprintln!("  picker: opening for tile {}", tile_idx);
                    self.picker_target = Some(tile_idx);
                    self.picker_scroll = 0;
                    self.picker_hovered = Some(0);  // Start with first item selected
                    self.picker_search.clear();
                    self.dirty = true;
                    self.request_frame(qh);
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
                    eprintln!("  picker search: '{}'", self.picker_search);
                } else {
                    self.search_query.push(c);
                    eprintln!("  search: '{}'", self.search_query);
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
    fn pointer_frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, pointer: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        let t0 = Instant::now();
        let mut needs_redraw = false;
        let event_count = events.len();

        for ev in events {
            if &ev.surface != self.layer.wl_surface() { continue; }

            // Handle pointer enter - set cursor to make it visible
            if let PointerEventKind::Enter { serial } = ev.kind {
                self.pointer_enter_serial = serial;
                // Set cursor from theme
                if let Some(ref mut cursor_theme) = self.cursor_theme {
                    if let Some(cursor) = cursor_theme.get_cursor("default") {
                        let image = &cursor[0];
                        let (hx, hy) = image.hotspot();
                        let (w, h) = image.dimensions();
                        self.cursor_surface.attach(Some(image), 0, 0);
                        self.cursor_surface.damage_buffer(0, 0, w as i32, h as i32);
                        self.cursor_surface.commit();
                        pointer.set_cursor(serial, Some(&self.cursor_surface), hx as i32, hy as i32);
                        eprintln!("  cursor: set via wl_pointer.set_cursor");
                    }
                }
            }

            // If picker is open, handle picker interactions
            if self.picker_target.is_some() {
                match ev.kind {
                    PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                        self.pointer_pos = ev.position;
                        let new_hovered = self.picker_item_at(ev.position.0, ev.position.1);
                        if new_hovered != self.picker_hovered {
                            self.picker_hovered = new_hovered;
                            needs_redraw = true;
                        }
                    }
                    PointerEventKind::Press { button, .. } if button == 0x110 => {
                        // Left click in picker
                        if let Some(visual_idx) = self.picker_item_at(ev.position.0, ev.position.1) {
                            // Convert visual index to icon index
                            let filtered = self.filtered_icon_indices();
                            let filtered_idx = self.picker_scroll + visual_idx;
                            if filtered_idx < filtered.len() {
                                let icon_idx = filtered[filtered_idx];
                                // Select this icon for the target tile
                                if let Some(target) = self.picker_target {
                                    self.tiles[target] = Some(icon_idx);
                                    eprintln!("  picker: assigned icon {} to tile {}", self.icons[icon_idx].name, target);
                                }
                            }
                            self.picker_target = None;
                            self.picker_hovered = None;
                            self.picker_search.clear();
                            needs_redraw = true;
                        } else {
                            // Clicked outside picker - close it
                            eprintln!("  picker: closed (clicked outside)");
                            self.picker_target = None;
                            self.picker_hovered = None;
                            self.picker_search.clear();
                            needs_redraw = true;
                        }
                    }
                    PointerEventKind::Axis { vertical, .. } => {
                        // Scroll in picker (use absolute value, positive = down)
                        let scroll_dir = if vertical.absolute > 0.0 { 1 } else if vertical.absolute < 0.0 { -1 } else { 0 };
                        if scroll_dir != 0 {
                            let filtered_len = self.filtered_icon_indices().len();
                            let max_scroll = filtered_len.saturating_sub(Self::PICKER_COLS * Self::PICKER_VISIBLE_ROWS);
                            if scroll_dir > 0 {
                                // Scroll down
                                self.picker_scroll = (self.picker_scroll + Self::PICKER_COLS).min(max_scroll);
                            } else {
                                // Scroll up
                                self.picker_scroll = self.picker_scroll.saturating_sub(Self::PICKER_COLS);
                            }
                            eprintln!("  picker: scroll to {}/{}", self.picker_scroll, filtered_len);
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
                    self.pointer_pos = ev.position;
                    let new_hovered = self.tile_at(ev.position.0, ev.position.1);
                    let new_dock_hovered = self.dock_item_at(ev.position.0, ev.position.1);

                    // Check if we should start dragging
                    if let Some((px, py, tile)) = self.press_start {
                        let dx = ev.position.0 - px;
                        let dy = ev.position.1 - py;
                        let dist = (dx * dx + dy * dy).sqrt();

                        if dist > self.drag_threshold && self.drag_from.is_none() {
                            self.drag_from = Some(tile);
                            self.press_start = None;
                            eprintln!("  drag start from tile {}", tile);
                            needs_redraw = true;
                        }
                    }

                    // Update grid hover state - only change focus when moving to a different tile
                    // (don't lose focus when moving to empty space between tiles)
                    if let Some(tile) = new_hovered {
                        if self.hovered_tile != Some(tile) {
                            self.hovered_tile = Some(tile);
                            needs_redraw = true;
                        }
                    }

                    // Update dock hover state
                    if new_dock_hovered != self.hovered_dock {
                        self.hovered_dock = new_dock_hovered;
                        needs_redraw = true;
                    }

                    // Update search engine hover state
                    let new_search_engine_hovered = self.search_engine_at(ev.position.0, ev.position.1);
                    if new_search_engine_hovered != self.hovered_search_engine {
                        self.hovered_search_engine = new_search_engine_hovered;
                        needs_redraw = true;
                    }

                    // Redraw during drag to update icon position
                    if self.drag_from.is_some() {
                        needs_redraw = true;
                    }
                }
                PointerEventKind::Leave { .. } => {
                    // Keep tile focus when pointer leaves - only clear dock/search hover
                    self.hovered_dock = None;
                    self.hovered_search_engine = None;
                    needs_redraw = true;
                }
                PointerEventKind::Press { button, .. } => {
                    if button == 0x110 { // BTN_LEFT
                        // Check search engine click first
                        if let Some(engine_idx) = self.search_engine_at(ev.position.0, ev.position.1) {
                            if let Some(engine) = self.search_engines.get(engine_idx) {
                                let query = self.search_query.replace(' ', "+");
                                let url = engine.url_template.replace("{}", &query);
                                let _ = Command::new("xdg-open")
                                    .arg(&url)
                                    .stdin(std::process::Stdio::null())
                                    .stdout(std::process::Stdio::null())
                                    .stderr(std::process::Stdio::null())
                                    .spawn();
                                eprintln!("  search {} for: {}", engine.name, self.search_query);
                                self.exit = true;
                            }
                        } else if let Some(tile) = self.tile_at(ev.position.0, ev.position.1) {
                            self.press_start = Some((ev.position.0, ev.position.1, tile));
                        } else if let Some(dock_idx) = self.dock_item_at(ev.position.0, ev.position.1) {
                            // Click on dock item - launch immediately
                            if let Some(entry) = self.dock.get(dock_idx) {
                                launch_exec(&entry.exec, &entry.name);
                                self.exit = true;
                            }
                        }
                    }
                }
                PointerEventKind::Release { button, .. } => {
                    if button == 0x110 { // BTN_LEFT
                        if let Some(from) = self.drag_from.take() {
                            // Was dragging - do the swap
                            if let Some(to) = self.tile_at(ev.position.0, ev.position.1) {
                                if from != to {
                                    self.tiles.swap(from, to);
                                    eprintln!("  swapped tile {} <-> {}", from, to);
                                }
                            }
                            needs_redraw = true;
                        } else if let Some((_, _, tile)) = self.press_start.take() {
                            // Was a click (no drag happened)
                            if self.tiles[tile].is_none() {
                                // Empty tile - open picker
                                self.refresh_icons_if_needed();
                                eprintln!("  picker: opening for tile {}", tile);
                                self.picker_target = Some(tile);
                                self.picker_scroll = 0;
                                self.picker_hovered = None;
                                self.picker_search.clear();
                                needs_redraw = true;
                            } else if let Some(icon_idx) = self.tiles[tile] {
                                // Filled tile - launch the app
                                if let Some(icon) = self.icons.get(icon_idx) {
                                    launch_exec(&icon.exec, &icon.name);
                                    // Exit after launching
                                    self.exit = true;
                                }
                            }
                        }
                    } else if button == 0x111 { // BTN_RIGHT
                        if let Some(tile) = self.tile_at(ev.position.0, ev.position.1) {
                            self.tiles[tile] = None;
                            eprintln!("  removed tile {}", tile);
                            needs_redraw = true;
                        }
                    }
                    self.press_start = None;
                }
                _ => {}
            }
        }

        let t1 = Instant::now();
        if needs_redraw {
            eprintln!("pointer_frame: {} events, process {:.2}ms, marking dirty", event_count, (t1 - t0).as_secs_f64() * 1000.0);
            self.dirty = true;
            self.request_frame(qh);
        }
    }
}

impl TouchHandler for App {
    fn down(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_touch::WlTouch, _serial: u32, _time: u32, _surface: wl_surface::WlSurface, _id: i32, position: (f64, f64)) {
        eprintln!("  touch down at ({:.0}, {:.0})", position.0, position.1);

        // If picker is open, handle picker touch
        if self.picker_target.is_some() {
            if let Some(visual_idx) = self.picker_item_at(position.0, position.1) {
                let filtered = self.filtered_icon_indices();
                let filtered_idx = self.picker_scroll + visual_idx;
                if filtered_idx < filtered.len() {
                    let icon_idx = filtered[filtered_idx];
                    if let Some(target) = self.picker_target {
                        self.tiles[target] = Some(icon_idx);
                        eprintln!("  picker: assigned icon {} to tile {}", self.icons[icon_idx].name, target);
                    }
                }
                self.picker_target = None;
                self.picker_hovered = None;
                self.picker_search.clear();
                self.dirty = true;
                self.request_frame(qh);
            } else {
                // Touched outside picker - close it
                eprintln!("  picker: closed (touched outside)");
                self.picker_target = None;
                self.picker_hovered = None;
                self.picker_search.clear();
                self.dirty = true;
                self.request_frame(qh);
            }
            return;
        }

        // Check search engine touch
        if let Some(engine_idx) = self.search_engine_at(position.0, position.1) {
            if let Some(engine) = self.search_engines.get(engine_idx) {
                let query = self.search_query.replace(' ', "+");
                let url = engine.url_template.replace("{}", &query);
                let _ = Command::new("xdg-open")
                    .arg(&url)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
                eprintln!("  search {} for: {}", engine.name, self.search_query);
                self.exit = true;
            }
            return;
        }

        // Check dock touch
        if let Some(dock_idx) = self.dock_item_at(position.0, position.1) {
            if let Some(entry) = self.dock.get(dock_idx) {
                launch_exec(&entry.exec, &entry.name);
                self.exit = true;
            }
            return;
        }

        // Check tile touch - record start position for potential drag
        if let Some(tile) = self.tile_at(position.0, position.1) {
            self.touch_start = Some((position.0, position.1, tile));
            self.hovered_tile = Some(tile);
            self.dirty = true;
            self.request_frame(qh);
        }
    }

    fn up(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_touch::WlTouch, _serial: u32, _time: u32, _id: i32) {
        eprintln!("  touch up");

        if let Some(from) = self.touch_drag_from.take() {
            // Was dragging - do the swap
            if let Some(to) = self.hovered_tile {
                if from != to {
                    self.tiles.swap(from, to);
                    eprintln!("  swapped tile {} <-> {}", from, to);
                }
            }
            self.dirty = true;
            self.request_frame(qh);
        } else if let Some((_, _, tile)) = self.touch_start.take() {
            // Was a tap (no drag)
            if self.tiles[tile].is_none() {
                // Empty tile - open picker
                self.refresh_icons_if_needed();
                eprintln!("  picker: opening for tile {}", tile);
                self.picker_target = Some(tile);
                self.picker_scroll = 0;
                self.picker_hovered = None;
                self.picker_search.clear();
                self.dirty = true;
                self.request_frame(qh);
            } else if let Some(icon_idx) = self.tiles[tile] {
                // Filled tile - launch the app
                if let Some(icon) = self.icons.get(icon_idx) {
                    launch_exec(&icon.exec, &icon.name);
                    self.exit = true;
                }
            }
        }
        self.touch_start = None;
    }

    fn motion(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_touch::WlTouch, _time: u32, _id: i32, position: (f64, f64)) {
        // Check if we should start dragging
        if let Some((px, py, tile)) = self.touch_start {
            let dx = position.0 - px;
            let dy = position.1 - py;
            let dist = (dx * dx + dy * dy).sqrt();

            if dist > self.drag_threshold && self.touch_drag_from.is_none() {
                self.touch_drag_from = Some(tile);
                self.touch_start = None;
                eprintln!("  touch drag start from tile {}", tile);
                self.dirty = true;
                self.request_frame(qh);
            }
        }

        // Update hover during drag
        if self.touch_drag_from.is_some() {
            let new_hovered = self.tile_at(position.0, position.1);
            if let Some(tile) = new_hovered {
                if self.hovered_tile != Some(tile) {
                    self.hovered_tile = Some(tile);
                    self.dirty = true;
                    self.request_frame(qh);
                }
            }
        }
    }

    fn cancel(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_touch::WlTouch) {
        eprintln!("  touch cancel");
        self.touch_start = None;
        self.touch_drag_from = None;
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
