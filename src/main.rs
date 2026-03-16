use eframe::egui::{self, Align, Button, Layout, Sense, TextEdit};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    env,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    id: String,
    name: String,
    exec: String,
    icon: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct BottomBar {
    options: String,
    commands: HashMap<String, String>,
}

#[derive(Serialize, Deserialize)]
struct Config {
    width: usize,
    height: usize,
    tiles: Vec<Option<String>>,
    custom_entries: Vec<Entry>,
    bottom_bar: BottomBar,
}

struct App {
    cfg: Config,
    cfg_path: PathBuf,
    all_entries: HashMap<String, Entry>,
    picker_open: bool,
    picker_filter: String,
    drag_from: Option<usize>,
    status: String,
}

fn config_path() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config/wlgrid/config.toml")
}

fn default_config() -> Config {
    let options = "󰍃    Logout\n󰑓    Reboot\n󰐥    Shutdown\n󰒲    Hibernate\n󰤄    Suspend";
    let commands = [
        ("Logout".to_string(), "loginctl terminate-user \"$USER\"".to_string()),
        ("Reboot".to_string(), "systemctl reboot".to_string()),
        ("Shutdown".to_string(), "systemctl poweroff".to_string()),
        ("Hibernate".to_string(), "systemctl hibernate".to_string()),
        ("Suspend".to_string(), "systemctl suspend".to_string()),
    ]
    .into_iter()
    .collect();
    Config {
        width: 6,
        height: 4,
        tiles: vec![None; 24],
        custom_entries: vec![],
        bottom_bar: BottomBar { options: options.into(), commands },
    }
}

fn ensure_config(path: &Path) -> Config {
    if let Ok(text) = fs::read_to_string(path) {
        if let Ok(mut cfg) = toml::from_str::<Config>(&text) {
            let len = cfg.width.saturating_mul(cfg.height);
            cfg.tiles.resize(len, None);
            return cfg;
        }
    }
    let cfg = default_config();
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(path, toml::to_string_pretty(&cfg).unwrap_or_default());
    cfg
}

fn save_config(path: &Path, cfg: &Config) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    fs::write(path, toml::to_string_pretty(cfg).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
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

fn option_label(line: &str) -> String {
    let mut parts = line.split_whitespace();
    let _ = parts.next();
    parts.collect::<Vec<_>>().join(" ")
}

impl App {
    fn new() -> Self {
        let cfg_path = config_path();
        let cfg = ensure_config(&cfg_path);
        let all_entries = load_entries(&cfg);
        Self {
            cfg,
            cfg_path,
            all_entries,
            picker_open: false,
            picker_filter: String::new(),
            drag_from: None,
            status: String::new(),
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("+ Add").clicked() {
                    self.picker_open = true;
                }
                if ui.button("Reload").clicked() {
                    self.cfg = ensure_config(&self.cfg_path);
                    self.all_entries = load_entries(&self.cfg);
                }
                if ui.button("Save").clicked() {
                    self.status = save_config(&self.cfg_path, &self.cfg).map(|_| "saved".into()).unwrap_or_else(|e| e);
                }
                if !self.status.is_empty() {
                    ui.label(&self.status);
                }
            });
        });

        if self.picker_open {
            let mut picker_open = self.picker_open;
            let mut close_picker = false;
            egui::Window::new("Add desktop entry")
                .open(&mut picker_open)
                .resizable(true)
                .show(ctx, |ui| {
                    ui.add(TextEdit::singleline(&mut self.picker_filter).hint_text("filter"));
                    let mut all = self.all_entries.values().collect::<Vec<_>>();
                    all.sort_by_key(|e| e.name.to_lowercase());
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for e in all.into_iter().filter(|e| {
                            self.picker_filter.is_empty()
                                || e.name.to_lowercase().contains(&self.picker_filter.to_lowercase())
                                || e.id.to_lowercase().contains(&self.picker_filter.to_lowercase())
                        }) {
                            if ui.button(format!("{}{}", e.icon.clone().unwrap_or_default(), if e.icon.is_some() { " " } else { "" }) + &e.name).clicked()
                            {
                                if let Some(i) = self.cfg.tiles.iter().position(Option::is_none) {
                                    self.cfg.tiles[i] = Some(e.id.clone());
                                    self.status = save_config(&self.cfg_path, &self.cfg).map(|_| "added".into()).unwrap_or_else(|x| x);
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
            let w = self.cfg.width.max(1);
            let h = self.cfg.height.max(1);
            self.cfg.tiles.resize(w * h, None);
            for y in 0..h {
                ui.horizontal(|ui| {
                    for x in 0..w {
                        let i = y * w + x;
                        let txt = self.cfg.tiles[i]
                            .as_ref()
                            .and_then(|id| self.all_entries.get(id))
                            .map(|e| format!("{}{}", e.icon.clone().unwrap_or_default(), if e.icon.is_some() { " " } else { "" }) + &e.name)
                            .unwrap_or_else(|| " ".into());
                        let r = ui.add(Button::new(txt).min_size(egui::vec2(130.0, 70.0)).sense(Sense::click_and_drag()));
                        if r.drag_started() {
                            self.drag_from = Some(i);
                        }
                        if self.drag_from.is_some() && r.hovered() && ui.input(|inp| inp.pointer.any_released()) {
                            let from = self.drag_from.unwrap_or(i);
                            self.cfg.tiles.swap(from, i);
                            self.drag_from = None;
                            self.status = save_config(&self.cfg_path, &self.cfg).map(|_| "moved".into()).unwrap_or_else(|e| e);
                        }
                        if r.clicked() && self.drag_from.is_none() {
                            if let Some(id) = &self.cfg.tiles[i] {
                                if let Some(e) = self.all_entries.get(id) {
                                    self.status = run_shell(&clean_exec(&e.exec)).map(|_| format!("launched {}", e.name)).unwrap_or_else(|e| e);
                                }
                            }
                        }
                        if r.secondary_clicked() {
                            self.cfg.tiles[i] = None;
                            self.status = save_config(&self.cfg_path, &self.cfg).map(|_| "removed".into()).unwrap_or_else(|e| e);
                        }
                    }
                });
            }
        });

        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| {
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                for line in self.cfg.bottom_bar.options.lines().map(str::trim).filter(|l| !l.is_empty()) {
                    if ui.button(line).clicked() {
                        let label = option_label(line);
                        if let Some(cmd) = self.cfg.bottom_bar.commands.get(&label) {
                            self.status = run_shell(cmd).map(|_| format!("ran {label}")).unwrap_or_else(|e| e);
                        } else {
                            self.status = format!("missing command for {label}");
                        }
                    }
                }
            });
        });
    }
}

fn main() -> Result<(), eframe::Error> {
    let native = eframe::NativeOptions::default();
    eframe::run_native("wlgrid", native, Box::new(|_| Ok(Box::new(App::new()))))
}
