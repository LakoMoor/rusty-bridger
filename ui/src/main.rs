use std::{
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError, Receiver},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use eframe::egui;
use rusty_bridge_lib::{
    vtspc::{CalcFn, VtsPc},
    vtsphone::{TrackingResponce, VtsPhone},
    webcam::WebcamTracker,
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
    source: Option<u8>,
    camera_index: Option<u32>,
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

// ── Live tracking state for the rig ──────────────────────────────────────────

#[derive(Default, Clone)]
struct RigState {
    face_found: bool,
    rot_y:   f32,  // yaw   degrees
    rot_x:   f32,  // pitch degrees
    rot_z:   f32,  // roll  degrees
    eye_l:   f32,  // 0 = open, 1 = closed
    eye_r:   f32,
    jaw:     f32,  // 0 = closed, 1 = open
    smile:   f32,  // -1 = frown, 1 = smile
    brow_l:  f32,  // 0 = down, 1 = raised
    brow_r:  f32,
}

impl RigState {
    fn from_tracking(t: &TrackingResponce) -> Self {
        let mut s = RigState {
            face_found: t.face_found,
            rot_x: t.rotation.x as f32,
            rot_y: t.rotation.y as f32,
            rot_z: t.rotation.z as f32,
            brow_l: 0.5,
            brow_r: 0.5,
            ..Default::default()
        };
        let (mut sl, mut sr, mut fl, mut fr) = (0.0f32, 0.0, 0.0, 0.0);
        for shape in &t.blend_shapes {
            match shape.k.as_str() {
                "EyeBlinkLeft"     => s.eye_l  = shape.v as f32,
                "EyeBlinkRight"    => s.eye_r  = shape.v as f32,
                "JawOpen"          => s.jaw    = shape.v as f32,
                "MouthSmileLeft"   => sl       = shape.v as f32,
                "MouthSmileRight"  => sr       = shape.v as f32,
                "MouthFrownLeft"   => fl       = shape.v as f32,
                "MouthFrownRight"  => fr       = shape.v as f32,
                "BrowOuterUpLeft"  => s.brow_l = shape.v as f32,
                "BrowOuterUpRight" => s.brow_r = shape.v as f32,
                _ => {}
            }
        }
        s.smile = (sl + sr) / 2.0 - (fl + fr) / 2.0;
        s
    }
}

// ── Enums ─────────────────────────────────────────────────────────────────────

#[derive(PartialEq, Clone, Copy)]
enum Source { IPhone, Webcam }

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
        self.formula_ok = if f.is_empty() {
            None
        } else {
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
            if idx == 0 { return; }
            idx - 1
        } else {
            if idx + 1 >= self.params.len() { return; }
            idx + 1
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
                self.params  = params;
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
    source:          Source,
    cameras:         Vec<(u32, String)>,
    selected_cam:    u32,
    active:          Arc<AtomicBool>,
    pending_path:    Option<Receiver<Option<String>>>,
    editor:          Editor,
    start_time:      Instant,
    rig_state:       Arc<Mutex<RigState>>,
}

impl App {
    fn new(cc: &eframe::CreationContext) -> Self {
        let cfg      = Config::load();
        let settings = AppSettings::load();
        let source   = match cfg.source.unwrap_or(0) {
            1 => Source::Webcam,
            _ => Source::IPhone,
        };
        apply_theme(&cc.egui_ctx, &settings.theme);
        let settings_draft = settings.clone();
        Self {
            transform_path:  cfg.transform_path.clone().unwrap_or_default(),
            phone_ip:        cfg.ip.clone().unwrap_or_default(),
            selected_cam:    cfg.camera_index.unwrap_or(0),
            source,
            tab:             Tab::Bridge,
            cameras:         WebcamTracker::list_cameras(),
            active:          Arc::new(AtomicBool::new(false)),
            pending_path:    None,
            editor:          Editor::default(),
            start_time:      Instant::now(),
            rig_state:       Arc::new(Mutex::new(RigState::default())),
            settings_status: String::new(),
            settings_draft,
            settings,
            cfg,
        }
    }

    fn save_config(&mut self) {
        self.cfg.transform_path = Some(self.transform_path.clone());
        self.cfg.ip             = Some(self.phone_ip.clone());
        self.cfg.source         = Some(if self.source == Source::Webcam { 1 } else { 0 });
        self.cfg.camera_index   = Some(self.selected_cam);
        self.cfg.save();
    }

    fn connect(&mut self) {
        self.active.store(true, Ordering::Relaxed);

        // tracker → relay channel, relay → vtspc channel
        let (track_tx, track_rx) = mpsc::channel::<TrackingResponce>();
        let (vts_tx,   vts_rx)   = mpsc::channel::<TrackingResponce>();

        // Relay: tap tracking data into shared RigState, then forward to VtsPc
        let relay_flag = Arc::clone(&self.active);
        let rig_state  = Arc::clone(&self.rig_state);
        thread::spawn(move || {
            while relay_flag.load(Ordering::Relaxed) {
                match track_rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(data) => {
                        if let Ok(mut g) = rig_state.lock() {
                            *g = RigState::from_tracking(&data);
                        }
                        let _ = vts_tx.send(data);
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                    Err(RecvTimeoutError::Timeout) => {}
                }
            }
        });

        let flag  = Arc::clone(&self.active);
        let flag2 = Arc::clone(&self.active);
        let path  = self.transform_path.clone();

        match self.source {
            Source::IPhone => {
                let ip = self.phone_ip.clone();
                thread::spawn(move || VtsPhone::run(ip, track_tx, flag2));
            }
            Source::Webcam => {
                let idx = self.selected_cam;
                thread::spawn(move || WebcamTracker::run(idx, track_tx, flag2));
            }
        }
        thread::spawn(move || VtsPc::run(vts_rx, path, flag));
    }

    fn disconnect(&mut self) {
        self.active.store(false, Ordering::Relaxed);
        // Reset rig state so it goes back to idle
        if let Ok(mut g) = self.rig_state.lock() {
            *g = RigState::default();
        }
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
        !self.transform_path.is_empty()
            && (self.source == Source::Webcam || !self.phone_ip.is_empty())
    }
}

// ── eframe::App ───────────────────────────────────────────────────────────────

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        apply_theme(ctx, &self.settings_draft.theme);

        // Continuously repaint while bridge is connected (live rig animation)
        let connected = self.active.load(Ordering::Relaxed);
        if connected && self.tab == Tab::Bridge {
            ctx.request_repaint_after(Duration::from_millis(33));
        }

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

        // Snapshot rig state (non-blocking)
        let rig = self.rig_state.try_lock()
            .map(|g| g.clone())
            .unwrap_or_default();

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
        let elapsed = self.start_time.elapsed().as_secs_f32();
        egui::CentralPanel::default().show(ctx, |ui| {
            match self.tab {
                Tab::Bridge   => bridge_ui(ui, self, connected, elapsed, &rig),
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

fn bridge_ui(ui: &mut egui::Ui, app: &mut App, connected: bool, elapsed: f32, rig: &RigState) {
    ui.add_space(6.0);

    // Source selector
    ui.horizontal(|ui| {
        let prev = app.source;
        ui.selectable_value(&mut app.source, Source::IPhone, "iPhone");
        ui.selectable_value(&mut app.source, Source::Webcam, "Webcam");
        if prev != app.source && !connected { app.save_config(); }
    });

    ui.add_space(6.0);

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

    ui.add_space(4.0);

    // Source-specific input
    match app.source {
        Source::IPhone => {
            let r = ui.add_sized(
                [ui.available_width(), 22.0],
                egui::TextEdit::singleline(&mut app.phone_ip)
                    .hint_text("iPhone IP  (e.g. 192.168.1.10)")
                    .interactive(!connected),
            );
            if r.changed() { app.save_config(); }
        }
        Source::Webcam => {
            let snap: Vec<_> = app.cameras.clone();
            let name = snap.iter().find(|(i, _)| *i == app.selected_cam)
                .map(|(_, n)| n.as_str()).unwrap_or("No cameras found");
            let mut new_cam = app.selected_cam;
            egui::ComboBox::from_id_salt("cam")
                .width(ui.available_width())
                .selected_text(name)
                .show_ui(ui, |ui| {
                    for (idx, n) in &snap {
                        ui.selectable_value(&mut new_cam, *idx, n);
                    }
                });
            if new_cam != app.selected_cam {
                app.selected_cam = new_cam;
                app.save_config();
            }
        }
    }

    ui.add_space(8.0);

    // Connect / Disconnect button
    let can   = app.can_connect();
    let label = if connected { "Disconnect" } else { "Connect" };
    if ui.add_enabled(connected || can,
        egui::Button::new(label).min_size([ui.available_width(), 30.0].into())
    ).clicked() {
        if connected { app.disconnect(); } else { app.connect(); }
    }

    ui.add_space(6.0);

    // Status line
    if !connected {
        let hint = if app.transform_path.is_empty() {
            "① Browse or paste a transform config path"
        } else if app.source == Source::IPhone && app.phone_ip.is_empty() {
            "② Enter your iPhone's IP address"
        } else {
            "② Press Connect — make sure VTube Studio is open"
        };
        ui.label(egui::RichText::new(hint).small().color(egui::Color32::from_gray(130)));
    } else if app.source == Source::Webcam {
        let det = app_dir().join("face_det.onnx");
        let lmk = app_dir().join("face_lmk.onnx");
        let models_ready = det.exists()
            && det.metadata().map(|m| m.len() > 4096).unwrap_or(false)
            && lmk.exists()
            && lmk.metadata().map(|m| m.len() > 4096).unwrap_or(false);

        let (col, msg) = if !models_ready {
            (egui::Color32::from_rgb(240, 180, 50), "Downloading ONNX models… (first run, ~3 MB)")
        } else if rig.face_found {
            (egui::Color32::from_rgb(80, 200, 100), "Face tracking active")
        } else {
            (egui::Color32::from_gray(140), "No face detected — look at the camera")
        };
        ui.label(egui::RichText::new(msg).small().color(col));
    } else if connected {
        ui.label(egui::RichText::new("Waiting for VTube Studio…").small().color(egui::Color32::from_gray(130)));
    }

    ui.add_space(4.0);

    // Webcam rig preview — fills remaining space
    if app.source == Source::Webcam {
        ui.separator();
        let avail = ui.available_size();
        draw_webcam_rig(ui, rig, elapsed, avail);
    } else {
        ui.separator();
        ui.add_space(4.0);
        ui.label(egui::RichText::new("github.com/LakoMoor/rusty-bridger")
            .small().color(egui::Color32::from_gray(85)));
    }
}

// ── Webcam face rig ───────────────────────────────────────────────────────────

fn draw_webcam_rig(ui: &mut egui::Ui, rig: &RigState, t: f32, avail: egui::Vec2) {
    let rig_w = avail.x;
    let rig_h = avail.y.max(160.0);

    let (rect, _) = ui.allocate_exact_size(egui::vec2(rig_w, rig_h), egui::Sense::hover());
    let painter   = ui.painter_at(rect);

    // Background
    painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(10, 14, 22));

    // Scale helper — relative to the smaller dimension so the rig fits
    let base   = rig_w.min(rig_h);
    let s      = |v: f32| v * (base / 280.0);
    let center = rect.center();

    // Colors — green when tracking, blue when idle
    let (rig_col, dim) = if rig.face_found {
        let c = egui::Color32::from_rgb(55, 210, 100);
        (c, c.linear_multiply(0.30))
    } else {
        let c = egui::Color32::from_rgb(60, 130, 210);
        (c, c.linear_multiply(0.30))
    };
    let lmk = rig_col.linear_multiply(0.65);

    // ── Head position offset from rotation ────────────────────────────────
    let yaw_off   = (rig.rot_y / 40.0).clamp(-1.0, 1.0) * s(30.0);
    let pitch_off = (rig.rot_x / 40.0).clamp(-1.0, 1.0) * s(22.0);
    let fc        = center + egui::vec2(yaw_off, pitch_off);

    // Perspective squish: face appears narrower when turned
    let yaw_t  = (rig.rot_y.abs() / 60.0).clamp(0.0, 0.55);
    let head_rx = s(62.0) * (1.0 - yaw_t * 0.35);

    // ── Head outline ──────────────────────────────────────────────────────
    painter.add(egui::Shape::Ellipse(egui::epaint::EllipseShape {
        center: fc + egui::vec2(0.0, s(4.0)),
        radius: egui::vec2(head_rx, s(74.0)),
        fill:   egui::Color32::TRANSPARENT,
        stroke: egui::Stroke::new(1.5, dim),
    }));

    // ── Eyes ──────────────────────────────────────────────────────────────
    // When not tracking fall back to idle blink animation
    let (open_l, open_r) = if rig.face_found {
        (1.0 - rig.eye_l, 1.0 - rig.eye_r)
    } else {
        let blink = (t * 1.3).sin() > 0.992;
        let v = if blink { 0.0_f32 } else { 1.0 };
        (v, v)
    };

    for (ex, eye_open) in [(-1.0_f32, open_l), (1.0, open_r)] {
        let ep   = fc + egui::vec2(s(ex * 22.0), s(-8.0));
        let ew   = s(13.0);
        let eh   = (s(9.5) * eye_open.max(0.04)).max(1.0);

        painter.add(egui::Shape::Ellipse(egui::epaint::EllipseShape {
            center: ep, radius: egui::vec2(ew, eh),
            fill: egui::Color32::TRANSPARENT,
            stroke: egui::Stroke::new(1.5, rig_col),
        }));

        if eye_open > 0.25 {
            // Iris ring
            painter.add(egui::Shape::Ellipse(egui::epaint::EllipseShape {
                center: ep + egui::vec2(s(0.5), s(0.5)),
                radius: egui::vec2(s(5.5), eh * 0.65),
                fill: egui::Color32::TRANSPARENT,
                stroke: egui::Stroke::new(1.0, rig_col.linear_multiply(0.55)),
            }));
            // Pupil
            painter.circle_filled(ep + egui::vec2(s(0.5), s(0.5)), s(2.8), rig_col);
        }

        // Eyebrow — raised by actual brow blend shape value
        let brow_raise = if rig.face_found {
            if ex < 0.0 { rig.brow_l } else { rig.brow_r }
        } else {
            0.5 + (t * 0.6).sin() * 0.05  // subtle idle sway
        };
        let by  = fc.y + s(-26.0) - (brow_raise - 0.5) * s(10.0);
        let bx  = ex * s(22.0);
        painter.add(egui::Shape::line(
            vec![
                egui::pos2(fc.x + bx - s(11.0) * ex, by + s(3.0)),
                egui::pos2(fc.x + bx,                  by),
                egui::pos2(fc.x + bx + s(11.0) * ex,   by + s(2.0)),
            ],
            egui::Stroke::new(1.8, rig_col.linear_multiply(0.88)),
        ));
    }

    // ── Nose ──────────────────────────────────────────────────────────────
    let nt = fc + egui::vec2(0.0, s(-1.0));
    let nl = fc + egui::vec2(s(-6.5), s(14.0));
    let nr = fc + egui::vec2(s(6.5),  s(14.0));
    for (a, b) in [(nt, nl), (nt, nr)] {
        painter.add(egui::Shape::line(vec![a, b], egui::Stroke::new(1.0, dim)));
    }
    painter.circle_stroke(nl + egui::vec2(s(-3.2), 0.0), s(3.5), egui::Stroke::new(1.0, dim));
    painter.circle_stroke(nr + egui::vec2(s(3.2),  0.0), s(3.5), egui::Stroke::new(1.0, dim));

    // ── Mouth ─────────────────────────────────────────────────────────────
    let jaw_v   = if rig.face_found { rig.jaw   } else { 0.0 };
    let smile_v = if rig.face_found { rig.smile } else { (t * 0.25).sin() * 0.08 };
    let mouth_y = fc.y + s(29.0);
    let open_h  = jaw_v * s(14.0);
    let sc      = smile_v * s(5.0);

    // Upper lip
    let upper: Vec<egui::Pos2> = (-5..=5).map(|i| {
        let tt = i as f32 / 5.0;
        egui::pos2(fc.x + tt * s(19.0), mouth_y - tt * tt * sc.abs() * smile_v.signum() * 0.4)
    }).collect();
    painter.add(egui::Shape::line(upper, egui::Stroke::new(1.6, rig_col)));

    if jaw_v > 0.04 {
        // Lower lip
        let lower: Vec<egui::Pos2> = (-5..=5).map(|i| {
            let tt = i as f32 / 5.0;
            egui::pos2(fc.x + tt * s(19.0), mouth_y + open_h + tt * tt * sc * 0.3)
        }).collect();
        painter.add(egui::Shape::line(lower, egui::Stroke::new(1.3, rig_col.linear_multiply(0.75))));
        painter.line_segment(
            [egui::pos2(fc.x - s(19.0), mouth_y), egui::pos2(fc.x - s(19.0), mouth_y + open_h)],
            egui::Stroke::new(1.0, rig_col.linear_multiply(0.5)),
        );
        painter.line_segment(
            [egui::pos2(fc.x + s(19.0), mouth_y), egui::pos2(fc.x + s(19.0), mouth_y + open_h)],
            egui::Stroke::new(1.0, rig_col.linear_multiply(0.5)),
        );
    }

    // ── Contour landmark dots ─────────────────────────────────────────────
    const DOTS: &[(f32, f32)] = &[
        (-62.0, 4.0), (62.0, 4.0),
        (-46.0, -44.0), (46.0, -44.0),
        (0.0, -74.0),
        (-50.0, 36.0), (50.0, 36.0),
        (-30.0, 64.0), (30.0, 64.0),
        (0.0, 76.0),
    ];
    for &(dx, dy) in DOTS {
        let pulse = if rig.face_found {
            (t * 2.0 + dx.abs() * 0.03).sin() * 0.3 + 0.7
        } else { 0.45 };
        painter.circle_filled(fc + egui::vec2(s(dx), s(dy)), s(2.4), lmk.linear_multiply(pulse));
    }

    // ── Camera-rig corner brackets ─────────────────────────────────────────
    let cs  = s(16.0);
    let cc  = egui::Color32::from_rgb(85, 155, 255).linear_multiply(0.75);
    let brt = [(rect.min, 1.0_f32, 1.0_f32),
               (egui::pos2(rect.max.x, rect.min.y), -1.0, 1.0),
               (egui::pos2(rect.min.x, rect.max.y),  1.0, -1.0),
               (rect.max, -1.0, -1.0)];
    for (pos, sx, sy) in brt {
        painter.line_segment([pos, egui::pos2(pos.x + sx * cs, pos.y)], egui::Stroke::new(2.0, cc));
        painter.line_segment([pos, egui::pos2(pos.x, pos.y + sy * cs)], egui::Stroke::new(2.0, cc));
    }

    // ── Overlays ──────────────────────────────────────────────────────────
    if rig.face_found {
        let lp    = rect.min + egui::vec2(10.0, 10.0);
        let pulse = (t * 2.2).sin() * 0.3 + 0.7;
        painter.circle_filled(lp, 5.0, egui::Color32::from_rgb(70, 220, 100).linear_multiply(pulse));
        painter.text(lp + egui::vec2(10.0, 0.0), egui::Align2::LEFT_CENTER,
            "TRACKING", egui::FontId::monospace(9.0), egui::Color32::from_rgb(70, 220, 100));

        // Compact rotation readout
        painter.text(
            rect.min + egui::vec2(10.0, 22.0),
            egui::Align2::LEFT_CENTER,
            format!("Y{:+.0}° P{:+.0}° R{:+.0}°", rig.rot_y, rig.rot_x, rig.rot_z),
            egui::FontId::monospace(8.5),
            egui::Color32::from_gray(95),
        );
    } else {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "No face detected",
            egui::FontId::proportional(12.0),
            egui::Color32::from_gray(70),
        );
    }

    // Bottom label
    painter.text(
        rect.center_bottom() + egui::vec2(0.0, -8.0),
        egui::Align2::CENTER_CENTER,
        "face rig preview",
        egui::FontId::proportional(9.0),
        egui::Color32::from_gray(45),
    );
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
        ui.vertical(|ui| {
            ui.set_width(170.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Parameters").strong().small());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let has  = ed.selected.is_some();
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
                    ui.label(egui::RichText::new("No params.\nPress ＋ to add one.").small().color(egui::Color32::from_gray(130)));
                }
            });
        });

        ui.separator();

        ui.vertical(|ui| {
            if ed.selected.is_none() {
                ui.add_space(60.0);
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new("← Select a parameter to edit").color(egui::Color32::from_gray(140)));
                });
                return;
            }
            let w = ui.available_width();
            egui::Grid::new("param_grid").num_columns(2).spacing([8.0, 5.0]).min_col_width(56.0).show(ui, |ui| {
                ui.label("Name");
                ui.horizontal(|ui| {
                    let r = ui.add_sized([w - 80.0, 22.0], egui::TextEdit::singleline(&mut ed.buf_name));
                    if r.changed() { ed.apply_edit(); ed.validate_buffers(); }
                    if ed.name_dup { ui.label(egui::RichText::new("⚠ dup").small().color(egui::Color32::from_rgb(240,160,30))); }
                });
                ui.end_row();
                ui.label("Formula");
                ui.horizontal(|ui| {
                    let r = ui.add_sized([w - 80.0, 22.0],
                        egui::TextEdit::singleline(&mut ed.buf_func).font(egui::TextStyle::Monospace).hint_text("HeadRotY * -1"));
                    if r.changed() { ed.apply_edit(); ed.validate_buffers(); }
                    match ed.formula_ok {
                        Some(true)  => { ui.label(egui::RichText::new("✓").color(egui::Color32::from_rgb(80,200,100))); }
                        Some(false) => { ui.label(egui::RichText::new("✗").color(egui::Color32::from_rgb(220,80,80))); }
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
            let min_v: f64 = ed.buf_min.parse().unwrap_or(-1.0);
            let max_v: f64 = ed.buf_max.parse().unwrap_or(1.0);
            let def_v: f64 = ed.buf_default.parse().unwrap_or(0.0);
            let t = if (max_v - min_v).abs() > 1e-9 { ((def_v - min_v) / (max_v - min_v)).clamp(0.0, 1.0) as f32 } else { 0.5 };
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!("{:.2}", min_v)).small().monospace().color(egui::Color32::from_gray(160)));
                let bar_w = ui.available_width() - 44.0;
                let (rect, _) = ui.allocate_exact_size(egui::vec2(bar_w, 8.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 3.0, egui::Color32::from_gray(50));
                let fill = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * t, rect.height()));
                ui.painter().rect_filled(fill, 3.0, egui::Color32::from_rgb(80,160,240));
                let tx = rect.min.x + rect.width() * t;
                ui.painter().line_segment([egui::pos2(tx, rect.min.y-2.0), egui::pos2(tx, rect.max.y+2.0)], egui::Stroke::new(1.5, egui::Color32::WHITE));
                ui.label(egui::RichText::new(format!("{:.2}", max_v)).small().monospace().color(egui::Color32::from_gray(160)));
            });
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            egui::CollapsingHeader::new(egui::RichText::new("Available variables").small()).default_open(false).show(ui, |ui| {
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
                ui.label(egui::RichText::new("Operators: + - * / ^ ( ) math functions").small().color(egui::Color32::from_gray(130)));
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
            let r = ui.add_sized([80.0, 20.0], egui::TextEdit::singleline(&mut port_str).hint_text("8001"));
            if r.changed() { if let Ok(p) = port_str.parse::<u16>() { draft.vts_port = p; } }
            ui.end_row();
            ui.label("Auto-reconnect");
            ui.checkbox(&mut draft.auto_reconnect, "");
            ui.end_row();
            ui.add_enabled(draft.auto_reconnect, egui::Label::new("Reconnect delay (s)"));
            ui.add_enabled(draft.auto_reconnect, egui::Slider::new(&mut draft.reconnect_delay_secs, 1..=30).suffix("s"));
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
            let dirty = *draft != *settings;
            if ui.add_enabled(dirty, egui::Button::new("Save Settings")).clicked() {
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
            let col = if status.starts_with("Saved") { egui::Color32::from_rgb(80,200,100) } else { egui::Color32::from_gray(150) };
            ui.label(egui::RichText::new(status.as_str()).small().color(col));
        }

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(6.0);
        ui.label(egui::RichText::new("Config files").strong());
        ui.add_space(3.0);
        let dir = app_dir().to_string_lossy().into_owned();
        egui::Grid::new("paths_grid").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
            for (label, file) in [("App settings", "settings.json"), ("UI config", "ui-cfg.json"), ("ONNX models", "face_det.onnx / face_lmk.onnx")] {
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
            ui.label(egui::RichText::new(APP_NAME).size(24.0).strong().color(egui::Color32::from_rgb(100, 170, 255)));
            ui.add_space(4.0);
            ui.label(egui::RichText::new(format!("v{VERSION}")).size(13.0).color(egui::Color32::from_gray(160)));
            ui.add_space(8.0);
            ui.label(egui::RichText::new("Cross-platform bridge between face tracking sources and VTube Studio.").color(egui::Color32::from_gray(200)));
            ui.add_space(4.0);
            ui.label(egui::RichText::new("Free & open-source alternative to VBridger.").small().color(egui::Color32::from_gray(140)));
        });

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(10.0);

        ui.label(egui::RichText::new("Fork & Authors").strong());
        ui.add_space(4.0);
        egui::Grid::new("authors_grid").num_columns(2).spacing([12.0, 5.0]).show(ui, |ui| {
            let rows = [
                ("Original project", "rusty-bridge by ovROG"),
                ("This fork",        "rusty-bridger by LakoMoor"),
                ("Repository",       "github.com/LakoMoor/rusty-bridger"),
                ("Upstream",         "github.com/ovROG/rusty-bridge"),
            ];
            for (label, val) in rows {
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
        ui.label(egui::RichText::new("GNU General Public License v3.0").color(egui::Color32::from_gray(210)));
        ui.add_space(2.0);
        ui.label(egui::RichText::new(
            "This program is free software: you can redistribute it and/or modify it\n\
             under the terms of the GNU General Public License as published by the Free\n\
             Software Foundation, either version 3 of the License, or (at your option)\n\
             any later version. Distributed WITHOUT ANY WARRANTY."
        ).small().color(egui::Color32::from_gray(130)));

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(10.0);

        ui.label(egui::RichText::new("Built with").strong());
        ui.add_space(4.0);
        let libs = [
            ("Rust",          "Systems programming language"),
            ("egui / eframe", "Immediate-mode GUI framework"),
            ("ONNX Runtime",  "Neural face tracking (webcam)"),
            ("nokhwa",        "Cross-platform camera capture"),
            ("tungstenite",   "WebSocket client for VTS API"),
            ("evalexpr",      "Transform formula evaluation"),
        ];
        egui::Grid::new("tech_grid").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
            for (name, desc) in libs {
                ui.label(egui::RichText::new(name).monospace().small().color(egui::Color32::from_gray(200)));
                ui.label(egui::RichText::new(desc).small().color(egui::Color32::from_gray(140)));
                ui.end_row();
            }
        });

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(8.0);
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new("Copyright © 2024-2025  LakoMoor / ovROG").small().color(egui::Color32::from_gray(95)));
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

    rusty_bridge_lib::webcam::init_camera_permissions();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_inner_size([480.0, 580.0])
            .with_min_inner_size([380.0, 420.0])
            .with_resizable(true),
        ..Default::default()
    };

    eframe::run_native(APP_NAME, options, Box::new(|cc| Ok(Box::new(App::new(cc))))).unwrap();
}
