use eframe::egui::{self, Align, Layout, Sense};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    env,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
use walkdir::WalkDir;

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    id: String,
    name: String,
    exec: String,
    icon: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct BottomBar {
    options: String,
    commands: HashMap<String, String>,
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Config {
    width: usize,
    height: usize,
    opacity: f32,
    mode: String,
    alpha: f32,
    icon_size: f32,
    tile_gap: f32,
    bottom_gap: f32,
    #[serde(skip_serializing, skip_deserializing, default)]
    tiles: Vec<Option<String>>,
    custom_entries: Vec<Entry>,
    bottom_bar: BottomBar,
}

#[derive(Serialize, Deserialize, Default)]
struct TileLayout {
    tiles: Vec<String>,
    #[serde(default)]
    scores: HashMap<String, f32>,
}

#[derive(Deserialize, Default)]
struct LegacyTileLayout {
    tiles: Vec<Option<String>>,
}

struct App {
    cfg: Config,
    layout_path: PathBuf,
    scores: HashMap<String, f32>,
    all_entries: HashMap<String, Entry>,
    icon_index: HashMap<String, PathBuf>,
    textures: HashMap<String, egui::TextureHandle>,
    missing_textures: HashSet<String>,
    picker_open: bool,
    picker_target: Option<usize>,
    drag_from: Option<usize>,
    status: String,
}

fn config_path() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config/wlgrid/config.toml")
}

fn layout_path() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config/wlgrid/tile_layout.toml")
}

fn default_config() -> Config {
    Config::default()
}

impl Default for BottomBar {
    fn default() -> Self {
        let options = "Logout\nReboot\nShutdown\nHibernate\nSuspend";
        let commands = [
            ("Logout".to_string(), "loginctl terminate-user \"$USER\"".to_string()),
            ("Reboot".to_string(), "systemctl reboot".to_string()),
            ("Shutdown".to_string(), "systemctl poweroff".to_string()),
            ("Hibernate".to_string(), "systemctl hibernate".to_string()),
            ("Suspend".to_string(), "systemctl suspend".to_string()),
        ]
        .into_iter()
        .collect();
        Self { options: options.into(), commands }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            width: 6,
            height: 4,
            opacity: 0.9,
            mode: "grid".into(),
            alpha: 0.08,
            icon_size: 96.0,
            tile_gap: 8.0,
            bottom_gap: 6.0,
            tiles: vec![None; 24],
            custom_entries: vec![],
            bottom_bar: BottomBar::default(),
        }
    }
}

fn ensure_config(path: &Path) -> (Config, String) {
    if let Ok(text) = fs::read_to_string(path) {
        if text.trim().is_empty() {
            return (default_config(), "config.toml is empty, using defaults".into());
        }
        if let Ok(mut cfg) = toml::from_str::<Config>(&text) {
            let len = cfg.width.saturating_mul(cfg.height);
            cfg.tiles.resize(len, None);
            cfg.opacity = cfg.opacity.clamp(0.15, 1.0);
            cfg.alpha = cfg.alpha.clamp(0.0, 1.0);
            cfg.icon_size = cfg.icon_size.clamp(40.0, 220.0);
            cfg.tile_gap = cfg.tile_gap.clamp(0.0, 24.0);
            cfg.bottom_gap = cfg.bottom_gap.clamp(0.0, 24.0);
            return (cfg, String::new());
        }
        return (default_config(), "config.toml parse failed, using defaults".into());
    }
    (default_config(), String::new())
}

fn load_layout(path: &Path, cfg: &mut Config, scores: &mut HashMap<String, f32>) -> Result<(), String> {
    if let Ok(text) = fs::read_to_string(path) {
        if text.trim().is_empty() {
            return Ok(());
        }
        match toml::from_str::<TileLayout>(&text) {
            Ok(layout) => {
                cfg.tiles = layout
                    .tiles
                    .into_iter()
                    .map(|t| if t.trim().is_empty() { None } else { Some(t) })
                    .collect();
                cfg.tiles.resize(cfg.width.saturating_mul(cfg.height), None);
                *scores = layout.scores;
            }
            Err(e1) => match toml::from_str::<LegacyTileLayout>(&text) {
                Ok(layout) => {
                    cfg.tiles = layout.tiles;
                    cfg.tiles.resize(cfg.width.saturating_mul(cfg.height), None);
                    scores.clear();
                }
                Err(_) => {
                    let ts = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let bad = path.with_extension(format!("bad-{ts}.toml"));
                    fs::rename(path, bad).map_err(|x| format!("layout parse error: {e1}; backup failed: {x}"))?;
                    return Err(format!("layout parse error: {e1}; broken file moved"));
                }
            }
        }
    }
    Ok(())
}

fn save_layout(path: &Path, cfg: &Config, scores: &HashMap<String, f32>) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let data = TileLayout {
        tiles: cfg.tiles.iter().map(|t| t.clone().unwrap_or_default()).collect(),
        scores: scores.clone(),
    };
    atomic_write(path, toml::to_string_pretty(&data).map_err(|e| e.to_string())?.as_bytes())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    let mut f = fs::File::create(&tmp).map_err(|e| e.to_string())?;
    f.write_all(bytes).map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())
}

fn desktop_dirs() -> Vec<PathBuf> {
    let home = env::var("HOME").unwrap_or_default();
    let data_home = env::var("XDG_DATA_HOME").unwrap_or_else(|_| format!("{home}/.local/share"));
    let data_dirs = env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    let mut out = vec![PathBuf::from(data_home).join("applications")];
    out.extend(data_dirs.split(':').map(|d| PathBuf::from(d).join("applications")));
    out
}

fn parse_desktop(path: &Path) -> Option<Entry> {
    let text = fs::read_to_string(path).ok()?;
    let mut in_entry = false;
    let mut name = String::new();
    let mut exec = String::new();
    let mut icon = None;
    let mut kind = String::new();
    let mut nodisplay = false;
    for l in text.lines() {
        let line = l.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry || line.starts_with('#') || !line.contains('=') {
            continue;
        }
        let (k, v) = line.split_once('=').unwrap_or(("", ""));
        match k {
            "Name" if name.is_empty() => name = v.to_string(),
            "Exec" if exec.is_empty() => exec = v.to_string(),
            "Icon" if icon.is_none() => icon = Some(v.to_string()),
            "Type" => kind = v.to_string(),
            "NoDisplay" => nodisplay = v.eq_ignore_ascii_case("true"),
            _ => {}
        }
    }
    if name.is_empty() || exec.is_empty() || kind != "Application" || nodisplay {
        return None;
    }
    Some(Entry {
        id: path.file_stem()?.to_string_lossy().to_string(),
        name,
        exec,
        icon,
    })
}

fn load_entries(cfg: &Config) -> HashMap<String, Entry> {
    let mut map = HashMap::new();
    for d in desktop_dirs() {
        if let Ok(rd) = fs::read_dir(d) {
            for f in rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "desktop")) {
                if let Some(e) = parse_desktop(&f) {
                    map.entry(e.id.clone()).or_insert(e);
                }
            }
        }
    }
    for c in &cfg.custom_entries {
        map.insert(c.id.clone(), c.clone());
    }
    map
}

fn icon_roots() -> Vec<PathBuf> {
    let home = env::var("HOME").unwrap_or_default();
    let mut out = vec![
        PathBuf::from(format!("{home}/.icons")),
        PathBuf::from(format!("{home}/.local/share/icons")),
        PathBuf::from("/usr/share/icons"),
        PathBuf::from("/usr/local/share/icons"),
        PathBuf::from("/usr/share/pixmaps"),
        PathBuf::from("/usr/local/share/pixmaps"),
    ];
    if let Ok(extra) = env::var("XDG_DATA_DIRS") {
        out.extend(extra.split(':').map(|d| PathBuf::from(d).join("icons")));
        out.extend(extra.split(':').map(|d| PathBuf::from(d).join("pixmaps")));
    }
    out
}

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "webp"))
}

fn build_icon_index() -> HashMap<String, PathBuf> {
    let mut idx = HashMap::new();
    for root in icon_roots().into_iter().filter(|p| p.exists()) {
        for e in WalkDir::new(root).max_depth(6).into_iter().flatten() {
            let p = e.path();
            if !p.is_file() || !is_image(p) {
                continue;
            }
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                idx.entry(stem.to_ascii_lowercase()).or_insert_with(|| p.to_path_buf());
            }
        }
    }
    idx
}

fn resolve_icon_path(icon: &str, idx: &HashMap<String, PathBuf>) -> Option<PathBuf> {
    if icon.trim().is_empty() {
        return None;
    }
    let p = PathBuf::from(icon);
    if p.is_file() && is_image(&p) {
        return Some(p);
    }
    let base = icon.trim().rsplit('/').next().unwrap_or(icon).to_ascii_lowercase();
    idx.get(&base)
        .cloned()
        .or_else(|| idx.get(base.split('.').next().unwrap_or("")).cloned())
}

fn load_texture(ctx: &egui::Context, key: &str, path: &Path) -> Option<egui::TextureHandle> {
    let bytes = fs::read(path).ok()?;
    let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    let pixels = img.into_vec();
    let color = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
    Some(ctx.load_texture(key.to_string(), color, Default::default()))
}

fn clean_exec(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut skip_next = false;
    for c in s.chars() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if c == '%' {
            skip_next = true;
            continue;
        }
        out.push(c);
    }
    out.trim().to_string()
}

fn run_shell(cmd: &str) -> Result<(), String> {
    Command::new("sh")
        .arg("-lc")
        .arg(cmd)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn power_label(line: &str, commands: &HashMap<String, String>) -> String {
    let line = line.trim().to_string();
    if commands.contains_key(&line) {
        return line;
    }
    let mut parts = line.split_whitespace();
    let _ = parts.next();
    let fallback = parts.collect::<Vec<_>>().join(" ");
    if commands.contains_key(&fallback) {
        fallback
    } else {
        line
    }
}

fn parse_power_pairs(options: &str, fallback: &HashMap<String, String>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for l in options.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some((k, v)) = l.split_once('=') {
            let key = k.trim().to_string();
            let val = v.trim().to_string();
            if !key.is_empty() && !val.is_empty() {
                out.push((key, val));
                continue;
            }
        }
        let label = power_label(l, fallback);
        if let Some(cmd) = fallback.get(&label) {
            out.push((label, cmd.clone()));
        }
    }
    out
}

impl App {
    fn new() -> Self {
        let cfg_path = config_path();
        let layout_path = layout_path();
        let (mut cfg, cfg_status) = ensure_config(&cfg_path);
        let mut scores = HashMap::new();
        let layout_status = load_layout(&layout_path, &mut cfg, &mut scores).err().unwrap_or_default();
        let status = match (cfg_status.is_empty(), layout_status.is_empty()) {
            (true, true) => String::new(),
            (false, true) => cfg_status,
            (true, false) => layout_status,
            (false, false) => format!("{cfg_status}; {layout_status}"),
        };
        if !status.is_empty() {
            eprintln!("{status}");
        }
        let all_entries = load_entries(&cfg);
        let icon_index = build_icon_index();
        Self {
            cfg,
            layout_path,
            scores,
            all_entries,
            icon_index,
            textures: HashMap::new(),
            missing_textures: HashSet::new(),
            picker_open: false,
            picker_target: None,
            drag_from: None,
            status,
        }
    }

    fn icon_for_entry(&mut self, ctx: &egui::Context, e: &Entry) -> Option<egui::TextureHandle> {
        let icon = e.icon.as_deref()?;
        if self.missing_textures.contains(icon) {
            return None;
        }
        if let Some(t) = self.textures.get(icon) {
            return Some(t.clone());
        }
        let Some(path) = resolve_icon_path(icon, &self.icon_index) else {
            self.missing_textures.insert(icon.to_string());
            return None;
        };
        if let Some(t) = load_texture(ctx, icon, &path) {
            self.textures.insert(icon.to_string(), t.clone());
            Some(t)
        } else {
            self.missing_textures.insert(icon.to_string());
            None
        }
    }

    fn launch_entry(&mut self, ctx: &egui::Context, id: &str) {
        if let Some(e) = self.all_entries.get(id).cloned() {
            self.status = run_shell(&clean_exec(&e.exec)).map(|_| format!("launched {}", e.name)).unwrap_or_else(|x| x);
            if self.status.starts_with("launched ") {
                let decay = (1.0 - self.cfg.alpha.clamp(0.0, 1.0)).max(0.0);
                for v in self.scores.values_mut() {
                    *v *= decay;
                }
                *self.scores.entry(id.to_string()).or_insert(0.0) += 1.0;
                self.scores.retain(|_, v| *v > 0.0001);
                if let Err(e) = save_layout(&self.layout_path, &self.cfg, &self.scores) {
                    self.status = e;
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
}

impl eframe::App for App {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let _ = save_layout(&self.layout_path, &self.cfg, &self.scores);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let w = self.cfg.width.max(1);
        let h = self.cfg.height.max(1);
        let tile = self.cfg.icon_size.clamp(40.0, 220.0);
        let gap = self.cfg.tile_gap.clamp(0.0, 24.0);
        let bottom_gap = self.cfg.bottom_gap.clamp(0.0, 24.0);
        let bar_h = 38.0;
        let target_size = egui::vec2(
            w as f32 * tile + (w.saturating_sub(1) as f32 * gap) + 16.0,
            h as f32 * tile + (h.saturating_sub(1) as f32 * gap) + bottom_gap + bar_h + 14.0,
        );
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(target_size));

        let alpha = (self.cfg.opacity.clamp(0.15, 1.0) * 255.0) as u8;
        ctx.style_mut(|s| {
            let panel = egui::Color32::from_rgba_unmultiplied(18, 18, 18, alpha);
            s.visuals.panel_fill = panel;
            s.visuals.window_fill = panel;
            s.visuals.widgets.noninteractive.bg_fill = panel;
            s.visuals.widgets.inactive.bg_fill = egui::Color32::from_rgba_unmultiplied(45, 45, 45, alpha);
            s.visuals.widgets.hovered.bg_fill = egui::Color32::from_rgba_unmultiplied(70, 70, 70, alpha);
            s.visuals.widgets.active.bg_fill = egui::Color32::from_rgba_unmultiplied(85, 85, 85, alpha);
            s.visuals.widgets.open.bg_fill = egui::Color32::from_rgba_unmultiplied(55, 55, 55, alpha);
        });

        if self.picker_open {
            let mut picker_open = self.picker_open;
            let mut close_picker = false;
            egui::Window::new("Add desktop entry")
                .open(&mut picker_open)
                .resizable(true)
                .show(ctx, |ui| {
                    let mut all = self.all_entries.values().cloned().collect::<Vec<_>>();
                    all.sort_by_key(|e| e.name.to_lowercase());
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        let icon_size = egui::vec2((tile * 0.5).clamp(32.0, 72.0), (tile * 0.5).clamp(32.0, 72.0));
                        for e in all {
                            let clicked = ui
                                .horizontal(|ui| {
                                    let (rect, r) = ui.allocate_exact_size(icon_size, Sense::click());
                                    let fill = ui.style().visuals.widgets.inactive.bg_fill;
                                    ui.painter().rect_filled(rect, 6.0, fill);
                                    if let Some(t) = self.icon_for_entry(ctx, &e) {
                                        ui.painter().image(
                                            t.id(),
                                            rect.shrink(4.0),
                                            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
                                            egui::Color32::WHITE,
                                        );
                                    }
                                    let rr = ui.add(egui::Label::new(&e.name).sense(Sense::click()));
                                    r.clicked() || rr.clicked()
                                })
                                .inner;
                            if clicked {
                                let target = self.picker_target.take().or_else(|| self.cfg.tiles.iter().position(Option::is_none));
                                if let Some(i) = target {
                                    self.cfg.tiles[i] = Some(e.id);
                                    self.status = save_layout(&self.layout_path, &self.cfg, &self.scores)
                                        .map(|_| "saved layout".into())
                                        .unwrap_or_else(|x| x);
                                    if self.status != "saved layout" {
                                        eprintln!("{}", self.status);
                                    }
                                } else {
                                    self.status = "no empty tile".into();
                                }
                                close_picker = true;
                            }
                        }
                    });
                });
            self.picker_open = picker_open && !close_picker;
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            if self.cfg.mode.eq_ignore_ascii_case("fuzzel") {
                let mut ids = self.all_entries.keys().cloned().collect::<Vec<_>>();
                ids.sort_by(|a, b| {
                    let sa = self.scores.get(a).copied().unwrap_or(0.0);
                    let sb = self.scores.get(b).copied().unwrap_or(0.0);
                    sb.total_cmp(&sa).then_with(|| a.cmp(b))
                });
                let cols = w.max(1);
                ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
                let mut idx = 0usize;
                while idx < ids.len() {
                    ui.horizontal(|ui| {
                        for _ in 0..cols {
                            if idx >= ids.len() {
                                break;
                            }
                            let id = ids[idx].clone();
                            idx += 1;
                            let (rect, r) = ui.allocate_exact_size(egui::vec2(tile, tile), Sense::click());
                            let fill = ui.style().visuals.widgets.inactive.bg_fill;
                            ui.painter().rect_filled(rect, 8.0, fill);
                            if let Some(e) = self.all_entries.get(&id).cloned() {
                                if let Some(t) = self.icon_for_entry(ctx, &e) {
                                    ui.painter().image(
                                        t.id(),
                                        rect.shrink(8.0),
                                        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
                                        egui::Color32::WHITE,
                                    );
                                }
                            }
                            if r.clicked() {
                                self.launch_entry(ctx, &id);
                            }
                        }
                    });
                    if idx < ids.len() && gap > 0.0 {
                        ui.add_space(gap);
                    }
                }
                return;
            }
            self.cfg.tiles.resize(w * h, None);
            let mut drop_target = None;
            let mut changed = false;
            ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
            for y in 0..h {
                ui.horizontal(|ui| {
                    for x in 0..w {
                        let i = y * w + x;
                        let (rect, r) = ui.allocate_exact_size(egui::vec2(tile, tile), Sense::click_and_drag());
                        let fill = ui.style().visuals.widgets.inactive.bg_fill;
                        ui.painter().rect_filled(rect, 8.0, fill);
                        if let Some(e) = self.cfg.tiles[i].as_ref().and_then(|id| self.all_entries.get(id)).cloned() {
                            if let Some(t) = self.icon_for_entry(ctx, &e) {
                                ui.painter().image(
                                    t.id(),
                                    rect.shrink(8.0),
                                    egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
                                    egui::Color32::WHITE,
                                );
                            }
                        }
                        if r.drag_started() {
                            self.drag_from = Some(i);
                        }
                        if r.hovered() {
                            drop_target = Some(i);
                        }
                        if r.clicked() && self.drag_from.is_none() {
                            if let Some(id) = &self.cfg.tiles[i] {
                                let id = id.clone();
                                self.launch_entry(ctx, &id);
                            } else {
                                self.picker_target = Some(i);
                                self.picker_open = true;
                            }
                        }
                        if r.secondary_clicked() {
                            self.cfg.tiles[i] = None;
                            changed = true;
                        }
                    }
                });
                if y + 1 < h && gap > 0.0 {
                    ui.add_space(gap);
                }
            }
            if ctx.input(|inp| inp.pointer.button_released(egui::PointerButton::Primary)) {
                if let (Some(from), Some(to)) = (self.drag_from.take(), drop_target) {
                    self.cfg.tiles.swap(from, to);
                    changed = true;
                }
            }
            if changed {
                self.status = save_layout(&self.layout_path, &self.cfg, &self.scores)
                    .map(|_| "saved layout".into())
                    .unwrap_or_else(|x| x);
                if self.status != "saved layout" {
                    eprintln!("{}", self.status);
                }
            }
        });

        egui::TopBottomPanel::bottom("bottom").exact_height(bar_h + bottom_gap).show(ctx, |ui| {
            if bottom_gap > 0.0 {
                ui.add_space(bottom_gap);
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                for (label, cmd) in parse_power_pairs(&self.cfg.bottom_bar.options, &self.cfg.bottom_bar.commands) {
                    if ui.button(&label).clicked() {
                        self.status = run_shell(&cmd).map(|_| format!("ran {label}")).unwrap_or_else(|e| e);
                    }
                }
            });
        });
    }
}

fn native_options_from_cfg(cfg: &Config) -> eframe::NativeOptions {
    let w = cfg.width.max(1);
    let h = cfg.height.max(1);
    let tile = cfg.icon_size.clamp(40.0, 220.0);
    let gap = cfg.tile_gap.clamp(0.0, 24.0);
    let bottom_gap = cfg.bottom_gap.clamp(0.0, 24.0);
    let bar_h = 38.0;
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([
                w as f32 * tile + (w.saturating_sub(1) as f32 * gap) + 16.0,
                h as f32 * tile + (h.saturating_sub(1) as f32 * gap) + bottom_gap + bar_h + 14.0,
            ])
            .with_resizable(false)
            .with_transparent(true),
        ..Default::default()
    }
}

fn backend_debug_snapshot() -> String {
    let vars = [
        "XDG_SESSION_TYPE",
        "WAYLAND_DISPLAY",
        "DISPLAY",
        "WINIT_UNIX_BACKEND",
        "XDG_CURRENT_DESKTOP",
        "HYPRLAND_INSTANCE_SIGNATURE",
    ];
    vars.into_iter()
        .map(|k| format!("{k}={}", env::var(k).unwrap_or_else(|_| "<unset>".into())))
        .collect::<Vec<_>>()
        .join(" | ")
}

fn run_app(cfg: &Config) -> Result<(), eframe::Error> {
    let native = native_options_from_cfg(cfg);
    eframe::run_native("wlgrid", native, Box::new(|_| Ok(Box::new(App::new()))))
}

fn main() -> Result<(), eframe::Error> {
    let (cfg, _) = ensure_config(&config_path());
    eprintln!("wlgrid startup env: {}", backend_debug_snapshot());
    match run_app(&cfg) {
        Ok(()) => Ok(()),
        Err(e) => {
            let msg = e.to_string();
            eprintln!("wlgrid first backend error: {msg}");
            let lower = msg.to_ascii_lowercase();
            let wayland_load_fail = msg.contains("NoWaylandLib")
                || lower.contains("wayland library could not be loaded")
                || (lower.contains("wayland") && lower.contains("could not be loaded"));
            if wayland_load_fail {
                eprintln!("Wayland backend unavailable, forcing X11 fallback");
                unsafe { env::set_var("WINIT_UNIX_BACKEND", "x11") };
                unsafe { env::set_var("XDG_SESSION_TYPE", "x11") };
                unsafe { env::remove_var("WAYLAND_DISPLAY") };
                eprintln!("wlgrid retry env: {}", backend_debug_snapshot());
                match run_app(&cfg) {
                    Ok(()) => Ok(()),
                    Err(e2) => {
                        eprintln!("wlgrid x11 retry error: {}", e2);
                        Err(e2)
                    }
                }
            } else {
                Err(e)
            }
        }
    }
}
