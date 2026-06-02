use std::{
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
        Arc,
    },
    thread,
};

use eframe::egui;
use rusty_bridge_lib::{
    vtspc::{CalcFn, VtsPc},
    vtsphone::{TrackingResponce, VtsPhone},
};

const APP_NAME: &str = "Rusty Bridger";
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn app_dir() -> PathBuf {
    let base = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join(".rusty-bridge");
    let _ = fs::create_dir_all(&dir);
    dir
}

// ── Persist config ────────────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "camelCase")]
struct Config {
    transform_path: Option<String>,
    ip: Option<String>,
}

impl Config {
    fn path() -> PathBuf { app_dir().join("ui-cfg.json") }
    fn load() -> Self {
        fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
    fn save(&self) {
        if let Ok(s) = serde_json::to_string(self) {
            let _ = fs::write(Self::path(), s);
        }
    }
}

// ── App settings ──────────────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
struct AppSettings {
    vts_port: u16,
    auto_reconnect: bool,
    reconnect_delay_secs: u32,
    log_level: String,
    theme: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            vts_port: 8001,
            auto_reconnect: false,
            reconnect_delay_secs: 3,
            log_level: "info".into(),
            theme: "dark".into(),
        }
    }
}

impl AppSettings {
    fn path() -> PathBuf { app_dir().join("settings.json") }
    fn load() -> Self {
        fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
    fn save(&self) {
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = fs::write(Self::path(), s);
        }
    }
}

// ── Tabs ──────────────────────────────────────────────────────────────────────

#[derive(PartialEq, Clone, Copy)]
enum Tab { Bridge, Config, Settings, About }

// ── Config editor ─────────────────────────────────────────────────────────────

#[derive(Default)]
struct Editor {
    params:      Vec<CalcFn>,
    selected:    Option<usize>,
    buf_name:    String,
    buf_func:    String,
    buf_min:     String,
    buf_max:     String,
    buf_default: String,
    formula_ok:  Option<bool>,
    name_dup:    bool,
    dirty:       bool,
    status:      String,
}

impl Editor {
    fn select(&mut self, idx: usize) {
        if idx >= self.params.len() { return; }
        let p = &self.params[idx];
        self.buf_name    = p.name.clone();
        self.buf_func    = p.func.clone();
        self.buf_min     = p.min.to_string();
        self.buf_max     = p.max.to_string();
        self.buf_default = p.default_value.to_string();
        self.selected    = Some(idx);
        self.validate_buffers();
    }

    fn validate_buffers(&mut self) {
        let f = self.buf_func.trim();
        self.formula_ok = if f.is_empty() { None } else {
            Some(evalexpr::build_operator_tree(f).is_ok())
        };
        let name = self.buf_name.trim();
        self.name_dup = self.params.iter().enumerate()
            .any(|(i, p)| Some(i) != self.selected && p.name == name);
    }

    fn apply_edit(&mut self) {
        let Some(idx) = self.selected else { return };
        let Some(p) = self.params.get_mut(idx) else { return };
        let name = self.buf_name.trim().to_string();
        if !name.is_empty() { p.name = name; }
        p.func          = self.buf_func.trim().to_string();
        p.min           = self.buf_min.parse().unwrap_or(p.min);
        p.max           = self.buf_max.parse().unwrap_or(p.max);
        p.default_value = self.buf_default.parse().unwrap_or(p.default_value);
        self.dirty = true;
    }

    fn add_param(&mut self) {
        self.apply_edit();
        let idx = self.params.len();
        self.params.push(CalcFn {
            name: format!("Param{}", idx + 1),
            func: "0".into(),
            min: -1.0, max: 1.0, default_value: 0.0,
        });
        self.dirty = true;
        self.select(idx);
    }

    fn delete_selected(&mut self) {
        let Some(idx) = self.selected else { return };
        self.params.remove(idx);
        self.dirty = true;
        self.selected = if self.params.is_empty() {
            None
        } else {
            let new = idx.min(self.params.len() - 1);
            self.select(new);
            Some(new)
        };
    }

    fn move_selected(&mut self, up: bool) {
        let Some(idx) = self.selected else { return };
        let new_idx = if up {
            if idx == 0 { return; } idx - 1
        } else {
            if idx + 1 >= self.params.len() { return; } idx + 1
        };
        self.apply_edit();
        self.params.swap(idx, new_idx);
        self.selected = Some(new_idx);
        self.dirty = true;
    }

    fn load_file(&mut self, path: &str) {
        match fs::read_to_string(path).and_then(|s| {
            serde_json::from_str::<Vec<CalcFn>>(&s)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        }) {
            Ok(params) => {
                self.params   = params;
                self.selected = None;
                self.buf_name.clear(); self.buf_func.clear();
                self.dirty  = false;
                self.status = format!("Loaded {} params", self.params.len());
            }
            Err(e) => self.status = format!("Load error: {e}"),
        }
    }

    fn save_file(&mut self, path: &str) {
        self.apply_edit();
        match serde_json::to_string_pretty(&self.params) {
            Ok(s) => match fs::write(path, s) {
                Ok(_)  => { self.dirty = false; self.status = "Saved".into(); }
                Err(e) => self.status = format!("Save error: {e}"),
            },
            Err(e) => self.status = format!("Serialize error: {e}"),
        }
    }
}

// ── App ───────────────────────────────────────────────────────────────────────

struct App {
    cfg:             Config,
    settings:        AppSettings,
    settings_draft:  AppSettings,
    settings_status: String,
    tab:             Tab,
    transform_path:  String,
    phone_ip:        String,
    active:          Arc<AtomicBool>,
    pending_path:    Option<Receiver<Option<String>>>,
    editor:          Editor,
}

impl App {
    fn new(cc: &eframe::CreationContext) -> Self {
        let cfg      = Config::load();
        let settings = AppSettings::load();
        apply_theme(&cc.egui_ctx, &settings.theme);
        let settings_draft = settings.clone();
        Self {
            transform_path:  cfg.transform_path.clone().unwrap_or_default(),
            phone_ip:        cfg.ip.clone().unwrap_or_default(),
            tab:             Tab::Bridge,
            active:          Arc::new(AtomicBool::new(false)),
            pending_path:    None,
            editor:          Editor::default(),
            settings_status: String::new(),
            settings_draft,
            settings,
            cfg,
        }
    }

    fn save_config(&mut self) {
        self.cfg.transform_path = Some(self.transform_path.clone());
        self.cfg.ip             = Some(self.phone_ip.clone());
        self.cfg.save();
    }

    fn connect(&mut self) {
        self.active.store(true, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel::<TrackingResponce>();
        let flag  = Arc::clone(&self.active);
        let flag2 = Arc::clone(&self.active);
        let path  = self.transform_path.clone();
        let ip    = self.phone_ip.clone();
        thread::spawn(move || VtsPhone::run(ip, tx, flag2));
        thread::spawn(move || VtsPc::run(rx, path, flag));
    }

    fn disconnect(&mut self) {
        self.active.store(false, Ordering::Relaxed);
    }

    fn open_file_dialog(&mut self) {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let r = rfd::FileDialog::new()
                .add_filter("JSON", &["json"])
                .pick_file()
                .map(|p| p.to_string_lossy().into_owned());
            let _ = tx.send(r);
        });
        self.pending_path = Some(rx);
    }

    fn can_connect(&self) -> bool {
        !self.transform_path.is_empty() && !self.phone_ip.is_empty()
    }
}

// ── eframe::App ───────────────────────────────────────────────────────────────

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        apply_theme(ctx, &self.settings_draft.theme);

        // Poll async file dialog
        if let Some(rx) = self.pending_path.take() {
            match rx.try_recv() {
                Ok(Some(path)) => {
                    self.transform_path = path.clone();
                    self.save_config();
                    self.editor.load_file(&path);
                }
                Ok(None) => {}
                Err(_) => { self.pending_path = Some(rx); }
            }
        }

        let connected = self.active.load(Ordering::Relaxed);

        // ── Tab bar ────────────────────────────────────────────────────────
        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Bridge, "Bridge");
                let cfg_label = if self.editor.dirty { "Config ●" } else { "Config" };
                ui.selectable_value(&mut self.tab, Tab::Config, cfg_label);
                ui.selectable_value(&mut self.tab, Tab::Settings, "Settings");
                ui.selectable_value(&mut self.tab, Tab::About, "About");
            });
            ui.add_space(2.0);
        });

        // ── Status bar ─────────────────────────────────────────────────────
        egui::TopBottomPanel::bottom("statusbar").show(ctx, |ui| {
            ui.add_space(3.0);
            ui.horizontal(|ui| {
                let (col, txt) = if connected {
                    (egui::Color32::from_rgb(80, 200, 100), "Connected")
                } else {
                    (egui::Color32::from_gray(110), "Disconnected")
                };
                ui.label(egui::RichText::new("●").color(col).small());
                ui.label(egui::RichText::new(txt).small());

                if !self.transform_path.is_empty() {
                    ui.separator();
                    let fname = PathBuf::from(&self.transform_path)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    ui.label(egui::RichText::new(fname).small().color(egui::Color32::from_gray(150)));
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(format!("v{VERSION}"))
                        .small().color(egui::Color32::from_gray(80)));
                });
            });
            ui.add_space(3.0);
        });

        // ── Central panel ──────────────────────────────────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
            match self.tab {
                Tab::Bridge   => bridge_ui(ui, self, connected),
                Tab::Config   => config_editor_ui(ui, &mut self.editor, &mut self.transform_path, &mut self.cfg),
                Tab::Settings => settings_ui(ui, &mut self.settings, &mut self.settings_draft, &mut self.settings_status),
                Tab::About    => about_ui(ui),
            }
        });
    }
}

// ── Theme ─────────────────────────────────────────────────────────────────────

fn apply_theme(ctx: &egui::Context, theme: &str) {
    match theme {
        "light" => ctx.set_visuals(egui::Visuals::light()),
        _       => ctx.set_visuals(egui::Visuals::dark()),
    }
}

// ── Bridge tab ────────────────────────────────────────────────────────────────

fn bridge_ui(ui: &mut egui::Ui, app: &mut App, connected: bool) {
    ui.add_space(10.0);

    // Transform path
    ui.horizontal(|ui| {
        let r = ui.add_sized(
            [ui.available_width() - 42.0, 22.0],
            egui::TextEdit::singleline(&mut app.transform_path)
                .hint_text("Transform config (.json)")
                .interactive(!connected),
        );
        if r.changed() { app.save_config(); }
        if ui.add_enabled(!connected,
            egui::Button::new("📂").min_size([36.0, 22.0].into())
        ).on_hover_text("Browse…").clicked() {
            app.open_file_dialog();
        }
    });

    ui.add_space(6.0);

    // iPhone IP
    let r = ui.add_sized(
        [ui.available_width(), 22.0],
        egui::TextEdit::singleline(&mut app.phone_ip)
            .hint_text("iPhone IP  (e.g. 192.168.1.10)")
            .interactive(!connected),
    );
    if r.changed() { app.save_config(); }

    ui.add_space(12.0);

    // Connect / Disconnect
    let label = if connected { "Disconnect" } else { "Connect" };
    if ui.add_enabled(connected || app.can_connect(),
        egui::Button::new(label).min_size([ui.available_width(), 32.0].into())
    ).clicked() {
        if connected { app.disconnect(); } else { app.connect(); }
    }

    ui.add_space(10.0);

    if !connected {
        let hint = if app.transform_path.is_empty() {
            "① Browse or paste a transform config path"
        } else if app.phone_ip.is_empty() {
            "② Enter your iPhone's IP address"
        } else {
            "② Press Connect — make sure VTube Studio is open"
        };
        ui.label(egui::RichText::new(hint).small().color(egui::Color32::from_gray(130)));
    }

    ui.add_space(8.0);
    ui.separator();
    ui.add_space(6.0);
    ui.label(egui::RichText::new("github.com/LakoMoor/rusty-bridger")
        .small().color(egui::Color32::from_gray(85)));
}

// ── Config editor tab ─────────────────────────────────────────────────────────

fn config_editor_ui(
    ui: &mut egui::Ui,
    ed: &mut Editor,
    path: &mut String,
    cfg: &mut Config,
) {
    ui.horizontal(|ui| {
        if ui.button("📂 Load").clicked() {
            if let Some(p) = rfd::FileDialog::new().add_filter("JSON", &["json"]).pick_file() {
                let s = p.to_string_lossy().into_owned();
                *path = s.clone();
                cfg.transform_path = Some(s.clone());
                cfg.save();
                ed.load_file(&s);
            }
        }
        if ui.button("💾 Save").clicked() {
            if path.is_empty() {
                if let Some(p) = rfd::FileDialog::new().add_filter("JSON", &["json"]).save_file() {
                    *path = p.to_string_lossy().into_owned();
                    cfg.transform_path = Some(path.clone());
                    cfg.save();
                }
            }
            if !path.is_empty() { ed.save_file(path); }
        }
        if ui.button("📋 New").clicked() {
            ed.apply_edit();
            ed.params.clear();
            ed.selected = None;
            ed.dirty = false;
            ed.status = "New config — save when ready".into();
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let col = if ed.status.contains("error") || ed.status.contains("Error") {
                egui::Color32::from_rgb(220, 80, 80)
            } else {
                egui::Color32::from_gray(150)
            };
            ui.label(egui::RichText::new(&ed.status).small().color(col));
        });
    });

    ui.add_space(2.0);
    ui.separator();
    ui.add_space(4.0);

    let avail = ui.available_height();
    ui.horizontal(|ui| {
        // ── Left: list ──────────────────────────────────────────────────
        ui.vertical(|ui| {
            ui.set_width(170.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Parameters").strong().small());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let has       = ed.selected.is_some();
                    let not_last  = ed.selected.map_or(false, |i| i + 1 < ed.params.len());
                    let not_first = ed.selected.map_or(false, |i| i > 0);
                    if ui.add_enabled(has && not_last,  egui::Button::new("↓").small()).clicked() { ed.move_selected(false); }
                    if ui.add_enabled(has && not_first, egui::Button::new("↑").small()).clicked() { ed.move_selected(true); }
                    if ui.add_enabled(has, egui::Button::new("🗑").small()).clicked() { ed.delete_selected(); }
                    if ui.small_button("＋").clicked() { ed.add_param(); }
                });
            });
            ui.separator();
            egui::ScrollArea::vertical().max_height(avail - 36.0).show(ui, |ui| {
                ui.set_width(160.0);
                for i in 0..ed.params.len() {
                    let sel = ed.selected == Some(i);
                    let col = if sel { egui::Color32::WHITE } else { egui::Color32::from_gray(200) };
                    let lbl = egui::RichText::new(&ed.params[i].name).monospace().small().color(col);
                    if ui.selectable_label(sel, lbl).clicked() && !sel {
                        ed.apply_edit(); ed.select(i);
                    }
                }
                if ed.params.is_empty() {
                    ui.label(egui::RichText::new("No params.\nPress ＋ to add one.")
                        .small().color(egui::Color32::from_gray(130)));
                }
            });
        });

        ui.separator();

        // ── Right: editor ───────────────────────────────────────────────
        ui.vertical(|ui| {
            if ed.selected.is_none() {
                ui.add_space(60.0);
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new("← Select a parameter to edit")
                        .color(egui::Color32::from_gray(140)));
                });
                return;
            }
            let w = ui.available_width();
            egui::Grid::new("param_grid").num_columns(2).spacing([8.0, 5.0]).min_col_width(56.0).show(ui, |ui| {
                ui.label("Name");
                ui.horizontal(|ui| {
                    let r = ui.add_sized([w - 80.0, 22.0], egui::TextEdit::singleline(&mut ed.buf_name));
                    if r.changed() { ed.apply_edit(); ed.validate_buffers(); }
                    if ed.name_dup {
                        ui.label(egui::RichText::new("⚠ dup").small()
                            .color(egui::Color32::from_rgb(240, 160, 30)));
                    }
                });
                ui.end_row();

                ui.label("Formula");
                ui.horizontal(|ui| {
                    let r = ui.add_sized([w - 80.0, 22.0],
                        egui::TextEdit::singleline(&mut ed.buf_func)
                            .font(egui::TextStyle::Monospace)
                            .hint_text("HeadRotY * -1"));
                    if r.changed() { ed.apply_edit(); ed.validate_buffers(); }
                    match ed.formula_ok {
                        Some(true)  => { ui.label(egui::RichText::new("✓").color(egui::Color32::from_rgb(80, 200, 100))); }
                        Some(false) => { ui.label(egui::RichText::new("✗").color(egui::Color32::from_rgb(220, 80, 80))); }
                        None => {}
                    }
                });
                ui.end_row();

                ui.label("Range");
                ui.horizontal(|ui| {
                    let fw = (w - 100.0) / 3.0;
                    let mut changed = false;
                    ui.label(egui::RichText::new("min").small().color(egui::Color32::from_gray(160)));
                    changed |= ui.add_sized([fw, 20.0], egui::TextEdit::singleline(&mut ed.buf_min)).changed();
                    ui.label(egui::RichText::new("max").small().color(egui::Color32::from_gray(160)));
                    changed |= ui.add_sized([fw, 20.0], egui::TextEdit::singleline(&mut ed.buf_max)).changed();
                    ui.label(egui::RichText::new("def").small().color(egui::Color32::from_gray(160)));
                    changed |= ui.add_sized([fw, 20.0], egui::TextEdit::singleline(&mut ed.buf_default)).changed();
                    if changed { ed.apply_edit(); ed.validate_buffers(); }
                });
                ui.end_row();
            });

            ui.add_space(6.0);

            // Range bar
            let min_v: f64 = ed.buf_min.parse().unwrap_or(-1.0);
            let max_v: f64 = ed.buf_max.parse().unwrap_or(1.0);
            let def_v: f64 = ed.buf_default.parse().unwrap_or(0.0);
            let t = if (max_v - min_v).abs() > 1e-9 {
                ((def_v - min_v) / (max_v - min_v)).clamp(0.0, 1.0) as f32
            } else { 0.5 };
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!("{:.2}", min_v)).small().monospace().color(egui::Color32::from_gray(160)));
                let bw = ui.available_width() - 44.0;
                let (rect, _) = ui.allocate_exact_size(egui::vec2(bw, 8.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 3.0, egui::Color32::from_gray(50));
                let fill = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * t, rect.height()));
                ui.painter().rect_filled(fill, 3.0, egui::Color32::from_rgb(80, 160, 240));
                let tx = rect.min.x + rect.width() * t;
                ui.painter().line_segment(
                    [egui::pos2(tx, rect.min.y - 2.0), egui::pos2(tx, rect.max.y + 2.0)],
                    egui::Stroke::new(1.5, egui::Color32::WHITE),
                );
                ui.label(egui::RichText::new(format!("{:.2}", max_v)).small().monospace().color(egui::Color32::from_gray(160)));
            });

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);

            egui::CollapsingHeader::new(egui::RichText::new("Available variables").small())
                .default_open(false)
                .show(ui, |ui| {
                    let vars = [
                        ("Head rotation", "HeadRotX  HeadRotY  HeadRotZ"),
                        ("Head position", "HeadPosX  HeadPosY  HeadPosZ"),
                        ("Eyes",          "EyeBlinkLeft  EyeBlinkRight"),
                        ("Mouth",         "JawOpen  MouthSmileLeft  MouthSmileRight\nMouthFrownLeft  MouthFrownRight"),
                        ("Brows",         "BrowOuterUpLeft  BrowOuterUpRight"),
                    ];
                    egui::Grid::new("vars_grid").num_columns(2).spacing([8.0, 2.0]).show(ui, |ui| {
                        for (cat, names) in &vars {
                            ui.label(egui::RichText::new(*cat).small().color(egui::Color32::from_gray(140)));
                            ui.label(egui::RichText::new(*names).small().monospace().color(egui::Color32::from_gray(200)));
                            ui.end_row();
                        }
                    });
                    ui.label(egui::RichText::new("Operators: + - * / ^ ( ) math functions")
                        .small().color(egui::Color32::from_gray(130)));
                });
        });
    });
}

// ── Settings tab ──────────────────────────────────────────────────────────────

fn settings_ui(
    ui: &mut egui::Ui,
    settings: &mut AppSettings,
    draft: &mut AppSettings,
    status: &mut String,
) {
    ui.add_space(4.0);
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.label(egui::RichText::new("Connection").strong());
        ui.separator();
        egui::Grid::new("conn_grid").num_columns(2).spacing([12.0, 6.0]).min_col_width(120.0).show(ui, |ui| {
            ui.label("VTube Studio port");
            let mut port_str = draft.vts_port.to_string();
            if ui.add_sized([80.0, 20.0], egui::TextEdit::singleline(&mut port_str).hint_text("8001")).changed() {
                if let Ok(p) = port_str.parse::<u16>() { draft.vts_port = p; }
            }
            ui.end_row();
            ui.label("Auto-reconnect");
            ui.checkbox(&mut draft.auto_reconnect, "");
            ui.end_row();
            ui.add_enabled(draft.auto_reconnect, egui::Label::new("Reconnect delay (s)"));
            ui.add_enabled(draft.auto_reconnect,
                egui::Slider::new(&mut draft.reconnect_delay_secs, 1..=30).suffix("s"));
            ui.end_row();
        });

        ui.add_space(10.0);
        ui.label(egui::RichText::new("Appearance").strong());
        ui.separator();
        egui::Grid::new("appear_grid").num_columns(2).spacing([12.0, 6.0]).min_col_width(120.0).show(ui, |ui| {
            ui.label("Theme");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut draft.theme, "dark".into(),  "Dark");
                ui.selectable_value(&mut draft.theme, "light".into(), "Light");
            });
            ui.end_row();
        });

        ui.add_space(10.0);
        ui.label(egui::RichText::new("Logging").strong());
        ui.separator();
        egui::Grid::new("log_grid").num_columns(2).spacing([12.0, 6.0]).min_col_width(120.0).show(ui, |ui| {
            ui.label("Log level");
            ui.horizontal(|ui| {
                for lvl in ["error", "warn", "info", "debug"] {
                    ui.selectable_value(&mut draft.log_level, lvl.to_string(), lvl.to_ascii_uppercase());
                }
            });
            ui.end_row();
        });

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.add_enabled(*draft != *settings, egui::Button::new("Save Settings")).clicked() {
                *settings = draft.clone();
                settings.save();
                *status = "Saved".into();
            }
            if ui.button("Reset Defaults").clicked() {
                *draft = AppSettings::default();
                *status = "Reset to defaults (not saved)".into();
            }
        });
        if !status.is_empty() {
            ui.add_space(4.0);
            let col = if status.starts_with("Saved") {
                egui::Color32::from_rgb(80, 200, 100)
            } else {
                egui::Color32::from_gray(150)
            };
            ui.label(egui::RichText::new(status.as_str()).small().color(col));
        }

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(6.0);
        ui.label(egui::RichText::new("Config files").strong());
        ui.add_space(3.0);
        let dir = app_dir().to_string_lossy().into_owned();
        egui::Grid::new("paths_grid").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
            for (label, file) in [
                ("App settings", "settings.json"),
                ("UI config",    "ui-cfg.json"),
                ("Auth token",   "token"),
                ("Logs",         "log/log.log"),
            ] {
                ui.label(egui::RichText::new(label).small().color(egui::Color32::from_gray(140)));
                ui.label(egui::RichText::new(format!("{dir}/{file}")).small().monospace().color(egui::Color32::from_gray(110)));
                ui.end_row();
            }
        });
    });
}

// ── About tab ─────────────────────────────────────────────────────────────────

fn about_ui(ui: &mut egui::Ui) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(16.0);
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new(APP_NAME).size(24.0).strong()
                .color(egui::Color32::from_rgb(100, 170, 255)));
            ui.add_space(4.0);
            ui.label(egui::RichText::new(format!("v{VERSION}")).size(13.0)
                .color(egui::Color32::from_gray(160)));
            ui.add_space(8.0);
            ui.label(egui::RichText::new(
                "Cross-platform bridge between face tracking\nsources and VTube Studio."
            ).color(egui::Color32::from_gray(200)));
            ui.add_space(4.0);
            ui.label(egui::RichText::new("Free & open-source alternative to VBridger.")
                .small().color(egui::Color32::from_gray(140)));
        });

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(10.0);

        ui.label(egui::RichText::new("Fork & Authors").strong());
        ui.add_space(4.0);
        egui::Grid::new("authors_grid").num_columns(2).spacing([12.0, 5.0]).show(ui, |ui| {
            for (label, val) in [
                ("Original project", "rusty-bridge by ovROG"),
                ("This fork",        "rusty-bridger by LakoMoor"),
                ("Repository",       "github.com/LakoMoor/rusty-bridger"),
                ("Upstream",         "github.com/ovROG/rusty-bridge"),
            ] {
                ui.label(egui::RichText::new(label).color(egui::Color32::from_gray(150)));
                ui.label(egui::RichText::new(val).monospace().small().color(egui::Color32::from_gray(210)));
                ui.end_row();
            }
        });

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(10.0);

        ui.label(egui::RichText::new("License").strong());
        ui.add_space(4.0);
        ui.label(egui::RichText::new("GNU General Public License v3.0")
            .color(egui::Color32::from_gray(210)));
        ui.add_space(2.0);
        ui.label(egui::RichText::new(
            "This program is free software: you can redistribute it and/or\n\
             modify it under the terms of the GNU General Public License as\n\
             published by the Free Software Foundation, either version 3, or\n\
             (at your option) any later version. Distributed WITHOUT WARRANTY."
        ).small().color(egui::Color32::from_gray(130)));

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(10.0);

        ui.label(egui::RichText::new("Built with").strong());
        ui.add_space(4.0);
        egui::Grid::new("tech_grid").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
            for (name, desc) in [
                ("Rust",          "Systems programming language"),
                ("egui / eframe", "Immediate-mode GUI framework"),
                ("tungstenite",   "WebSocket client for VTS API"),
                ("evalexpr",      "Transform formula evaluation"),
            ] {
                ui.label(egui::RichText::new(name).monospace().small().color(egui::Color32::from_gray(200)));
                ui.label(egui::RichText::new(desc).small().color(egui::Color32::from_gray(140)));
                ui.end_row();
            }
        });

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(8.0);
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new("Copyright © 2024-2025  LakoMoor / ovROG")
                .small().color(egui::Color32::from_gray(95)));
        });
        ui.add_space(8.0);
    });
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    let data_dir = app_dir();
    let _ = std::env::set_current_dir(&data_dir);

    let log_dir  = data_dir.join("log");
    let _ = fs::create_dir_all(&log_dir);
    let log_file = log_dir.join("log.log").to_string_lossy().into_owned();
    let log_cfg  = include_str!("../../configs/log_cfg.yml").replace("log/log.log", &log_file);
    if let Ok(raw) = serde_yaml::from_str(&log_cfg) {
        let _ = log4rs::init_raw_config(raw);
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_inner_size([420.0, 360.0])
            .with_min_inner_size([360.0, 300.0])
            .with_resizable(true),
        ..Default::default()
    };

    eframe::run_native(APP_NAME, options, Box::new(|cc| Ok(Box::new(App::new(cc))))).unwrap();
}
