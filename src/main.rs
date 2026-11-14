#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use chrono::{Datelike, Local};
use eframe::egui::{self, pos2, vec2, Color32, CornerRadius, FontId, RichText, Sense, Stroke, StrokeKind};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

const ACCENT: Color32 = Color32::from_rgb(108, 92, 231);
const DAYS: [&str; 7] = ["Lundi", "Mardi", "Mercredi", "Jeudi", "Vendredi", "Samedi", "Dimanche"];
const COLORS: [&str; 8] = ["#6c5ce7", "#00b8d9", "#e5484d", "#f59f00", "#2fb344", "#e64980", "#4263eb", "#495057"];

fn ab() -> String { "AB".into() }
fn defcol() -> String { COLORS[0].into() }

// Même format JSON que la version web : les sauvegardes sont interchangeables.
#[derive(Serialize, Deserialize, Clone, Default)]
struct Course {
    id: String,
    title: String,
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    date: i64,
}
#[derive(Serialize, Deserialize, Clone)]
struct Event {
    id: String,
    title: String,
    day: usize,
    start: String,
    end: String,
    #[serde(default)]
    room: String,
    #[serde(default = "ab")]
    weeks: String,
    #[serde(default = "defcol")]
    color: String,
}
#[derive(Serialize, Deserialize, Clone, Default)]
struct Data {
    #[serde(default)]
    courses: Vec<Course>,
    #[serde(default)]
    events: Vec<Event>,
    #[serde(default)]
    swap: bool,
}

fn uid() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{:x}{:x}", t, N.fetch_add(1, Ordering::Relaxed))
}
fn hex(s: &str) -> Color32 {
    let s = s.trim_start_matches('#');
    let p = |i: usize| u8::from_str_radix(s.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0);
    Color32::from_rgb(p(0), p(2), p(4))
}
fn ease(t: f32) -> f32 { 1.0 - (1.0 - t).powi(3) }
fn cur_letter(swap: bool) -> &'static str {
    if (Local::now().iso_week().week() % 2 == 1) != swap { "A" } else { "B" }
}

// ---------- Extraction de texte ----------
fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}
fn scan_xml(xml: &str, odt: bool) -> String {
    let (mut out, mut i, mut depth, mut in_t) = (String::new(), 0usize, 0i32, false);
    while i < xml.len() {
        if xml.as_bytes()[i] == b'<' {
            let Some(e) = xml[i..].find('>') else { break };
            let end = i + e;
            let tag = &xml[i + 1..end];
            let closing = tag.starts_with('/');
            let selfc = tag.ends_with('/');
            let name = tag.trim_start_matches('/').split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("");
            if odt {
                match name {
                    "text:p" | "text:h" => {
                        if closing { depth = (depth - 1).max(0); out.push('\n') } else if !selfc { depth += 1 }
                    }
                    "text:s" => out.push(' '),
                    "text:tab" => out.push('\t'),
                    "text:line-break" => out.push('\n'),
                    _ => {}
                }
            } else {
                match name {
                    "w:t" => in_t = !closing && !selfc,
                    "w:p" if closing => out.push('\n'),
                    "w:tab" => out.push('\t'),
                    "w:br" => out.push('\n'),
                    _ => {}
                }
            }
            i = end + 1;
        } else {
            let next = xml[i..].find('<').map(|e| i + e).unwrap_or(xml.len());
            if (odt && depth > 0) || (!odt && in_t) { out.push_str(&unescape(&xml[i..next])) }
            i = next;
        }
    }
    out
}
fn zip_xml(path: &PathBuf, inner: &str, odt: bool) -> Result<String, String> {
    use std::io::Read;
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut z = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
    let mut xml = String::new();
    z.by_name(inner).map_err(|e| e.to_string())?.read_to_string(&mut xml).map_err(|e| e.to_string())?;
    Ok(scan_xml(&xml, odt))
}
fn extract(path: &PathBuf) -> Result<String, String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let text = match ext.as_str() {
        "txt" | "md" => std::fs::read_to_string(path).map_err(|e| e.to_string())?,
        "pdf" => {
            let p = path.clone();
            std::panic::catch_unwind(move || pdf_extract::extract_text(&p))
                .map_err(|_| "PDF illisible".to_string())?
                .map_err(|e| e.to_string())?
        }
        "docx" => zip_xml(path, "word/document.xml", false)?,
        "odt" => zip_xml(path, "content.xml", true)?,
        _ => return Err("format non pris en charge".into()),
    };
    if text.trim().is_empty() { Err("aucun texte trouvé".into()) } else { Ok(text) }
}

// ---------- Application ----------
#[derive(PartialEq, Clone, Copy)]
enum Tab { Courses, Plan, Data }
struct CourseForm { id: String, title: String, text: String }
struct EvForm { id: Option<String>, title: String, day: usize, start: String, end: String, room: String, weeks: String, color: String }
type Msg = (String, Result<String, String>);

struct App {
    data: Data, path: PathBuf, tab: Tab, tab_t: f64, dark: bool,
    search: String, view: String, sel_day: usize,
    course_form: Option<CourseForm>, ev_form: Option<EvForm>,
    tx: Sender<Msg>, rx: Receiver<Msg>, pending: usize,
    toast: Option<(String, f64)>, confirm_reset: bool,
}

fn apply_theme(ctx: &egui::Context, dark: bool) {
    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    let (bg, surf) = if dark { (Color32::from_rgb(15, 17, 25), Color32::from_rgb(24, 27, 39)) } else { (Color32::from_rgb(244, 245, 251), Color32::WHITE) };
    v.panel_fill = bg;
    v.window_fill = surf;
    v.extreme_bg_color = bg;
    v.selection.bg_fill = ACCENT;
    v.window_corner_radius = CornerRadius::same(18);
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.noninteractive] {
        w.corner_radius = CornerRadius::same(10);
    }
    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = vec2(10.0, 10.0);
        s.spacing.button_padding = vec2(14.0, 8.0);
    });
}
fn surface(dark: bool) -> Color32 { if dark { Color32::from_rgb(24, 27, 39) } else { Color32::WHITE } }
fn primary(t: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(t.to_string()).color(Color32::WHITE).strong()).fill(ACCENT).corner_radius(CornerRadius::same(12))
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let path = dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("StudyFlow").join("studyflow.json");
        let data = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let dark = true;
        apply_theme(&cc.egui_ctx, dark);
        let (tx, rx) = channel();
        App { data, path, tab: Tab::Courses, tab_t: 0.0, dark, search: String::new(), view: "cur".into(), sel_day: ((Local::now().weekday().num_days_from_monday()) as usize).min(6),
              course_form: None, ev_form: None, tx, rx, pending: 0, toast: None, confirm_reset: false }
    }
    fn persist(&self) {
        if let Some(d) = self.path.parent() { let _ = std::fs::create_dir_all(d); }
        if let Ok(s) = serde_json::to_string_pretty(&self.data) { let _ = std::fs::write(&self.path, s); }
    }
    fn say(&mut self, ctx: &egui::Context, t: impl Into<String>) {
        self.toast = Some((t.into(), ctx.input(|i| i.time)));
    }
    fn import_paths(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>) {
        if paths.is_empty() { return }
        self.pending += paths.len();
        let (tx, c) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            for p in paths {
                let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let _ = tx.send((name, extract(&p)));
                c.request_repaint();
            }
        });
    }
    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok((name, r)) = self.rx.try_recv() {
            self.pending = self.pending.saturating_sub(1);
            match r {
                Ok(text) => {
                    let (title, kind) = match name.rsplit_once('.') { Some((a, b)) => (a.to_string(), b.to_lowercase()), None => (name.clone(), "txt".into()) };
                    self.data.courses.insert(0, Course { id: uid(), title, kind, text, date: Local::now().timestamp_millis() });
                    self.persist();
                    self.say(ctx, format!("✔ {name} importé"));
                }
                Err(e) => self.say(ctx, format!("✖ {name} : {e}")),
            }
        }
    }
    fn go(&mut self, t: Tab, now: f64) { if self.tab != t { self.tab = t; self.tab_t = now; } }

    fn nav_button(&mut self, ui: &mut egui::Ui, label: &str, tab: Tab, size: egui::Vec2, now: f64) {
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
        let sel = self.tab == tab;
        let a = ui.ctx().animate_bool_with_time(resp.id, sel, 0.25);
        let h = ui.ctx().animate_bool_with_time(resp.id.with("h"), resp.hovered(), 0.15);
        let base = surface(self.dark).gamma_multiply(h * 0.9);
        let fill = base.lerp_to_gamma(ACCENT, a);
        ui.painter().rect_filled(rect.translate(vec2(h * 3.0 * (1.0 - a), 0.0)), CornerRadius::same(14), fill);
        let col = if sel { Color32::WHITE } else { ui.visuals().text_color() };
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, label, FontId::proportional(16.0), col);
        if resp.clicked() { self.go(tab, now) }
    }

    fn view_courses(&mut self, ui: &mut egui::Ui, now: f64, t0: f64) {
        ui.label(RichText::new("Mes cours").size(32.0).strong());
        ui.label(RichText::new("Importe tes documents PDF, TXT, DOCX ou ODT, ou glisse-les dans la fenêtre.").color(ui.visuals().weak_text_color()));
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 84.0), Sense::click());
        let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.2);
        let c = ui.visuals().weak_text_color().lerp_to_gamma(ACCENT, h);
        ui.painter().rect_stroke(rect, CornerRadius::same(18), Stroke::new(2.0, c), StrokeKind::Inside);
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "Cliquer pour importer des fichiers (PDF · TXT · DOCX · ODT)", FontId::proportional(16.0), c);
        if resp.clicked() {
            if let Some(p) = rfd::FileDialog::new().add_filter("Cours", &["pdf", "txt", "md", "docx", "odt"]).pick_files() { self.import_paths(ui.ctx(), p) }
        }
        if self.pending > 0 { ui.horizontal(|ui| { ui.spinner(); ui.label(format!("Lecture de {} fichier(s)…", self.pending)); }); }
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("Rechercher un cours…").desired_width((ui.available_width() - 110.0).max(120.0)));
            if ui.add(primary("+ Note")).clicked() {
                let c = Course { id: uid(), title: "Nouvelle note".into(), kind: "note".into(), text: String::new(), date: Local::now().timestamp_millis() };
                self.course_form = Some(CourseForm { id: c.id.clone(), title: c.title.clone(), text: String::new() });
                self.data.courses.insert(0, c);
                self.persist();
            }
        });
        let q = self.search.to_lowercase();
        let idx: Vec<usize> = (0..self.data.courses.len()).filter(|&i| { let c = &self.data.courses[i]; format!("{}{}", c.title, c.text).to_lowercase().contains(&q) }).collect();
        if idx.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| ui.label(RichText::new("Aucun cours pour l’instant ✨").color(ui.visuals().weak_text_color())));
            return;
        }
        let gap = 14.0;
        ui.spacing_mut().item_spacing = vec2(gap, gap);
        let cols = (((ui.available_width() + gap) / (260.0 + gap)).floor() as usize).max(1);
        let w = (ui.available_width() - gap * (cols as f32 - 1.0)) / cols as f32;
        let mut open = None;
        for (r, row) in idx.chunks(cols).enumerate() {
            ui.horizontal(|ui| {
                for (k, &i) in row.iter().enumerate() {
                    let n = r * cols + k;
                    let t = ease(((now - t0 - n as f64 * 0.04) / 0.35).clamp(0.0, 1.0) as f32);
                    if course_card(ui, &self.data.courses[i], w, t, self.dark) { open = Some(i) }
                }
            });
        }
        if let Some(i) = open {
            let c = &self.data.courses[i];
            self.course_form = Some(CourseForm { id: c.id.clone(), title: c.title.clone(), text: c.text.clone() });
        }
    }

    fn view_plan(&mut self, ui: &mut egui::Ui, now: f64, t0: f64) {
        ui.label(RichText::new("Emploi du temps").size(32.0).strong());
        let cur = cur_letter(self.data.swap);
        ui.horizontal_wrapped(|ui| {
            for (key, lab) in [("cur", "Cette semaine"), ("A", "Semaine A"), ("B", "Semaine B")] {
                let sel = self.view == key;
                let b = egui::Button::new(RichText::new(lab).color(if sel { Color32::WHITE } else { ui.visuals().text_color() })).fill(if sel { ACCENT } else { surface(self.dark) });
                if ui.add(b).clicked() { self.view = key.into() }
            }
            if ui.add(primary("+ Cours")).clicked() {
                self.ev_form = Some(EvForm { id: None, title: String::new(), day: self.sel_day, start: "08:00".into(), end: "09:00".into(), room: String::new(), weeks: "AB".into(), color: COLORS[0].into() });
            }
            ui.label(RichText::new(format!("Semaine en cours : {cur}")).color(ACCENT));
        });
        let w = if self.view == "cur" { cur.to_string() } else { self.view.clone() };
        let evs: Vec<usize> = (0..self.data.events.len()).filter(|&i| { let e = &self.data.events[i]; e.weeks == "AB" || e.weeks == w }).collect();
        let n = if evs.iter().any(|&i| self.data.events[i].day >= 5) { 7 } else { 5 };
        let narrow = ui.available_width() < 640.0;
        if self.sel_day >= n { self.sel_day = 0 }
        let today = Local::now().weekday().num_days_from_monday() as usize;
        if narrow {
            ui.horizontal_wrapped(|ui| {
                for d in 0..n {
                    let sel = self.sel_day == d;
                    let b = egui::Button::new(RichText::new(&DAYS[d][..3]).color(if sel { Color32::WHITE } else { ui.visuals().text_color() })).fill(if sel { ACCENT } else { surface(self.dark) });
                    if ui.add(b).clicked() { self.sel_day = d }
                }
            });
        }
        let days: Vec<usize> = if narrow { vec![self.sel_day] } else { (0..n).collect() };
        let gap = 10.0;
        let colw = (ui.available_width() - gap * (days.len() as f32 - 1.0)) / days.len() as f32;
        let mut edit = None;
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for &d in &days {
                ui.allocate_ui(vec2(colw, 10.0), |ui| {
                    ui.set_width(colw);
                    let hc = if d == today { ACCENT } else { ui.visuals().weak_text_color() };
                    ui.label(RichText::new(DAYS[d].to_uppercase()).small().strong().color(hc));
                    let mut list: Vec<usize> = evs.iter().copied().filter(|&i| self.data.events[i].day == d).collect();
                    list.sort_by(|&a, &b| self.data.events[a].start.cmp(&self.data.events[b].start));
                    for (k, &i) in list.iter().enumerate() {
                        let t = ease(((now - t0 - k as f64 * 0.06) / 0.35).clamp(0.0, 1.0) as f32);
                        if event_block(ui, &self.data.events[i], colw, t) { edit = Some(i) }
                    }
                    if list.is_empty() { ui.label(RichText::new("—").color(ui.visuals().weak_text_color())); }
                });
            }
        });
        if let Some(i) = edit {
            let e = self.data.events[i].clone();
            self.ev_form = Some(EvForm { id: Some(e.id), title: e.title, day: e.day, start: e.start, end: e.end, room: e.room, weeks: e.weeks, color: e.color });
        }
    }

    fn view_data(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.label(RichText::new("Données").size(32.0).strong());
        ui.label(RichText::new(format!("Sauvegarde automatique : {}", self.path.display())).small().color(ui.visuals().weak_text_color()));
        ui.add_space(8.0);
        ui.label(RichText::new("Exporter / importer").strong());
        ui.horizontal_wrapped(|ui| {
            if ui.add(primary("Exporter (JSON)")).clicked() {
                if let Some(p) = rfd::FileDialog::new().set_file_name("studyflow-sauvegarde.json").save_file() {
                    let ok = serde_json::to_string_pretty(&self.data).ok().and_then(|s| std::fs::write(p, s).ok()).is_some();
                    self.say(&ctx, if ok { "✔ Export réussi" } else { "✖ Échec de l’export" });
                }
            }
            for (lab, replace) in [("Importer (fusionner)", false), ("Importer (remplacer)", true)] {
                if ui.button(lab).clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("JSON", &["json"]).pick_file() {
                        match std::fs::read_to_string(p).ok().and_then(|s| serde_json::from_str::<Data>(&s).ok()) {
                            Some(d) => {
                                if replace { self.data = d } else {
                                    for c in d.courses { if !self.data.courses.iter().any(|x| x.id == c.id) { self.data.courses.push(c) } }
                                    for e in d.events { if !self.data.events.iter().any(|x| x.id == e.id) { self.data.events.push(e) } }
                                }
                                self.persist();
                                self.say(&ctx, "✔ Import réussi");
                            }
                            None => self.say(&ctx, "✖ Fichier JSON invalide"),
                        }
                    }
                }
            }
        });
        ui.add_space(12.0);
        ui.label(RichText::new("Semaines A / B").strong());
        ui.label("La semaine A correspond aux semaines ISO impaires.");
        let lab = if self.data.swap { "A = semaines paires (cliquer pour inverser)" } else { "A = semaines impaires (cliquer pour inverser)" };
        if ui.button(lab).clicked() { self.data.swap = !self.data.swap; self.persist() }
        ui.add_space(12.0);
        ui.label(RichText::new("Apparence").strong());
        if ui.button(if self.dark { "Passer en thème clair" } else { "Passer en thème sombre" }).clicked() { self.dark = !self.dark; apply_theme(&ctx, self.dark) }
        ui.add_space(12.0);
        ui.label(RichText::new("Zone sensible").strong());
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.confirm_reset, "Je confirme");
            if ui.add_enabled(self.confirm_reset, egui::Button::new(RichText::new("Tout effacer").color(Color32::from_rgb(229, 72, 77)))).clicked() {
                self.data = Data::default(); self.confirm_reset = false; self.persist();
            }
        });
    }
}

fn course_card(ui: &mut egui::Ui, c: &Course, w: f32, t: f32, dark: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 150.0), Sense::click());
    let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.15);
    let rect = rect.translate(vec2(0.0, (1.0 - t) * 14.0 - h * 4.0));
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(18), surface(dark).gamma_multiply(t));
    p.rect_stroke(rect, CornerRadius::same(18), Stroke::new(1.0 + h, Color32::from_gray(if dark { 50 } else { 225 }).lerp_to_gamma(ACCENT, h).gamma_multiply(t)), StrokeKind::Inside);
    let tc = ui.visuals().text_color().gamma_multiply(t);
    let wc = ui.visuals().weak_text_color().gamma_multiply(t);
    p.text(rect.min + vec2(16.0, 16.0), egui::Align2::LEFT_TOP, c.kind.to_uppercase(), FontId::proportional(11.0), ACCENT.gamma_multiply(t));
    let mut title = egui::text::LayoutJob::simple(c.title.clone(), FontId::proportional(17.0), tc, w - 32.0);
    title.wrap.max_rows = 1;
    let g = p.layout_job(title);
    p.galley(rect.min + vec2(16.0, 36.0), g, tc);
    let preview: String = c.text.chars().take(240).collect::<String>().replace('\n', " ");
    let mut job = egui::text::LayoutJob::simple(preview, FontId::proportional(13.0), wc, w - 32.0);
    job.wrap.max_rows = 3;
    let g = p.layout_job(job);
    p.galley(rect.min + vec2(16.0, 64.0), g, wc);
    let date = chrono::DateTime::from_timestamp_millis(c.date).map(|d| d.with_timezone(&Local).format("%d/%m/%Y").to_string()).unwrap_or_default();
    p.text(rect.left_bottom() + vec2(16.0, -14.0), egui::Align2::LEFT_BOTTOM, format!("{date} · {} mots", c.text.split_whitespace().count()), FontId::proportional(12.0), wc);
    resp.clicked()
}

fn event_block(ui: &mut egui::Ui, e: &Event, w: f32, t: f32) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 76.0), Sense::click());
    let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.15);
    let rect = rect.expand(h * 2.0).translate(vec2(0.0, (1.0 - t) * 10.0));
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(12), hex(&e.color).gamma_multiply(t));
    let white = Color32::WHITE.gamma_multiply(t);
    let mut job = egui::text::LayoutJob::simple(e.title.clone(), FontId::proportional(14.0), white, w - 20.0);
    job.wrap.max_rows = 2;
    let g = p.layout_job(job);
    p.galley(rect.min + vec2(10.0, 8.0), g, white);
    p.text(rect.left_bottom() + vec2(10.0, -8.0), egui::Align2::LEFT_BOTTOM, format!("{} – {}  {}", e.start, e.end, e.room), FontId::proportional(11.0), white);
    if e.weeks != "AB" { p.text(pos2(rect.right() - 8.0, rect.top() + 6.0), egui::Align2::RIGHT_TOP, &e.weeks, FontId::proportional(10.0), white); }
    resp.clicked()
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _f: &mut eframe::Frame) {
        let ctx = &ui.ctx().clone();
        let now = ctx.input(|i| i.time);
        self.poll(ctx);
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        self.import_paths(ctx, dropped);
        let narrow = ctx.content_rect().width() < 760.0;
        let tabs = [("Cours", Tab::Courses), ("Planning", Tab::Plan), ("Données", Tab::Data)];
        if narrow {
            egui::Panel::bottom("nav").show(ui, |ui| {
                ui.horizontal(|ui| {
                    let w = (ui.available_width() - 20.0) / 3.0;
                    for (l, t) in tabs { self.nav_button(ui, l, t, vec2(w, 40.0), now) }
                });
            });
        } else {
            egui::Panel::left("nav").exact_size(210.0).show(ui, |ui| {
                ui.add_space(14.0);
                ui.label(RichText::new("StudyFlow").size(26.0).strong().color(ACCENT));
                ui.add_space(14.0);
                for (l, t) in tabs { self.nav_button(ui, l, t, vec2(ui.available_width(), 44.0), now) }
            });
        }
        let t = ease(((now - self.tab_t) / 0.35).clamp(0.0, 1.0) as f32);
        if t < 1.0 { ctx.request_repaint() }
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.add_space((1.0 - t) * 16.0 + 10.0);
                ui.set_opacity(t);
                match self.tab {
                    Tab::Courses => self.view_courses(ui, now, self.tab_t),
                    Tab::Plan => self.view_plan(ui, now, self.tab_t),
                    Tab::Data => self.view_data(ui),
                }
            });
        });
        // Fenêtre : cours
        if let Some(mut f) = self.course_form.take() {
            let (mut open, mut act) = (true, 0);
            egui::Window::new("Cours").open(&mut open).collapsible(false).default_size([640.0, 480.0]).show(ctx, |ui| {
                ui.add(egui::TextEdit::singleline(&mut f.title).desired_width(f32::INFINITY));
                egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                    ui.add(egui::TextEdit::multiline(&mut f.text).desired_width(f32::INFINITY).desired_rows(14));
                });
                ui.horizontal(|ui| {
                    if ui.add(primary("Enregistrer")).clicked() { act = 1 }
                    if ui.button("Supprimer").clicked() { act = 2 }
                });
            });
            match act {
                1 => { if let Some(c) = self.data.courses.iter_mut().find(|c| c.id == f.id) { c.title = f.title; c.text = f.text; } self.persist(); self.say(ctx, "Enregistré"); }
                2 => { self.data.courses.retain(|c| c.id != f.id); self.persist(); }
                _ => if open { self.course_form = Some(f) },
            }
        }
        // Fenêtre : événement
        if let Some(mut f) = self.ev_form.take() {
            let (mut open, mut act) = (true, 0);
            egui::Window::new(if f.id.is_some() { "Modifier le cours" } else { "Nouveau cours" }).open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
                ui.label("Matière");
                ui.text_edit_singleline(&mut f.title);
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("day").selected_text(DAYS[f.day.min(6)]).show_ui(ui, |ui| { for (i, d) in DAYS.iter().enumerate() { ui.selectable_value(&mut f.day, i, *d); } });
                    egui::ComboBox::from_id_salt("wk").selected_text(match f.weeks.as_str() { "A" => "Semaine A", "B" => "Semaine B", _ => "A et B" }).show_ui(ui, |ui| {
                        for (k, l) in [("AB", "A et B"), ("A", "Semaine A"), ("B", "Semaine B")] { ui.selectable_value(&mut f.weeks, k.to_string(), l); }
                    });
                });
                ui.horizontal(|ui| {
                    ui.label("Début"); ui.add(egui::TextEdit::singleline(&mut f.start).desired_width(60.0));
                    ui.label("Fin"); ui.add(egui::TextEdit::singleline(&mut f.end).desired_width(60.0));
                });
                ui.label("Salle");
                ui.text_edit_singleline(&mut f.room);
                ui.horizontal(|ui| {
                    for c in COLORS {
                        let (r, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click());
                        ui.painter().circle_filled(r.center(), 11.0, hex(c));
                        if f.color == c { ui.painter().circle_stroke(r.center(), 13.0, Stroke::new(2.0, ui.visuals().text_color())); }
                        if resp.clicked() { f.color = c.to_string() }
                    }
                });
                ui.horizontal(|ui| {
                    if ui.add(primary("Enregistrer")).clicked() { act = 1 }
                    if f.id.is_some() && ui.button("Supprimer").clicked() { act = 2 }
                });
            });
            let valid = |s: &str| s.len() == 5 && s.as_bytes()[2] == b':' && s[..2].parse::<u8>().map_or(false, |h| h < 24) && s[3..].parse::<u8>().map_or(false, |m| m < 60);
            match act {
                1 if !valid(&f.start) || !valid(&f.end) || f.end <= f.start => { self.say(ctx, "Heures invalides (format HH:MM, fin après début)"); self.ev_form = Some(f); }
                1 => {
                    let title = if f.title.trim().is_empty() { "Cours".to_string() } else { f.title.trim().to_string() };
                    let ev = Event { id: f.id.clone().unwrap_or_else(uid), title, day: f.day, start: f.start, end: f.end, room: f.room, weeks: f.weeks, color: f.color };
                    self.sel_day = ev.day;
                    match self.data.events.iter().position(|e| Some(&e.id) == f.id.as_ref()) { Some(p) => self.data.events[p] = ev, None => self.data.events.push(ev) }
                    self.persist();
                }
                2 => { self.data.events.retain(|e| Some(&e.id) != f.id.as_ref()); self.persist(); }
                _ => if open { self.ev_form = Some(f) },
            }
        }
        // Notification
        if let Some((msg, t0)) = self.toast.clone() {
            let age = now - t0;
            if age > 3.0 { self.toast = None } else {
                let a = ((3.0 - age).min(0.3) / 0.3).min(age / 0.2).clamp(0.0, 1.0) as f32;
                egui::Area::new(egui::Id::new("toast")).anchor(egui::Align2::CENTER_BOTTOM, vec2(0.0, -70.0 + (1.0 - a) * 20.0)).show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).corner_radius(CornerRadius::same(20)).show(ui, |ui| { ui.set_opacity(a); ui.label(RichText::new(msg).strong()); });
                });
                ctx.request_repaint();
            }
        }
    }
}

fn main() -> eframe::Result {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1100.0, 720.0]).with_min_inner_size([360.0, 520.0]).with_title("StudyFlow").with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("StudyFlow", opts, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    fn mk(name: &str, inner: &str, xml: &str) -> PathBuf {
        let p = std::env::temp_dir().join(name);
        let mut z = zip::ZipWriter::new(std::fs::File::create(&p).unwrap());
        z.start_file(inner, zip::write::SimpleFileOptions::default()).unwrap();
        z.write_all(xml.as_bytes()).unwrap();
        z.finish().unwrap();
        p
    }
    #[test]
    fn docx_odt() {
        let d = mk("t.docx", "word/document.xml", "<w:document><w:body><w:p><w:r><w:t>Bonjour &amp; é</w:t></w:r></w:p><w:p><w:r><w:t xml:space=\"preserve\">Ligne 2</w:t></w:r></w:p></w:body></w:document>");
        assert_eq!(extract(&d).unwrap().trim(), "Bonjour & é\nLigne 2");
        let o = mk("t.odt", "content.xml", "<office:text><text:h>Titre</text:h><text:p>Un<text:s/>mot <text:span>gras</text:span></text:p></office:text>");
        assert_eq!(extract(&o).unwrap().trim(), "Titre\nUn mot gras");
    }
}
