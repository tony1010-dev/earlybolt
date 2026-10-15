mod platform;
mod tools;

use chrono::{DateTime, Duration as CDuration, Local, TimeZone};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, LogicalSize, Manager, State, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_notification::NotificationExt as _;
use tauri_plugin_opener::OpenerExt as _;
use tauri_plugin_positioner::{Position, WindowExt as _};

/// A window starts at the moment of the ping, so ping again 2 minutes after the previous window ends
/// (06:00 → 11:02 → 16:04 → 21:06). The margin avoids pinging just before a reset.
const GAP: u32 = 5 * 60 + 2;
const CALL_TIMEOUT: Duration = Duration::from_secs(180);
const PANEL_WIDTH: f64 = 400.0;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Mode {
    /// Always-on machine (Mac mini, desktop)
    Mini,
    /// Laptop (MacBook, Windows laptop)
    Laptop,
}

#[derive(Serialize, Deserialize, Clone)]
struct ToolResult {
    ok: bool,
    at: i64,
    message: String,
}

/// Outcome of the latest ping round, shown as "✓ 4/4 succeeded"
#[derive(Serialize, Deserialize, Clone)]
struct RunSummary {
    at: i64,
    ok: usize,
    total: usize,
    failed: Vec<String>,
}

#[derive(Serialize, Clone)]
struct Progress {
    done: usize,
    total: usize,
    current: String,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
struct Settings {
    enabled: bool,
    mode: Mode,
    /// "en" or "ko"
    lang: String,
    /// Minutes after midnight
    work_start: u32,
    first_ping: u32,
    pings_per_day: u32,
    tools_on: HashMap<String, bool>,
    show_time_in_tray: bool,
    wake_enabled: bool,
    /// Latest slot already handled, so a slot never fires twice
    last_slot: i64,
    results: HashMap<String, ToolResult>,
    last_run: Option<RunSummary>,
    first_run_done: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            enabled: true,
            mode: Mode::Mini,
            lang: "en".into(),
            work_start: 9 * 60,
            first_ping: 6 * 60,
            pings_per_day: 4,
            tools_on: HashMap::new(),
            show_time_in_tray: false,
            wake_enabled: false,
            last_slot: Local::now().timestamp(),
            results: HashMap::new(),
            last_run: None,
            first_run_done: false,
        }
    }
}

#[derive(Serialize, Clone, Default)]
struct ToolStatus {
    installed: Option<bool>,
    /// None: no reliable way to check (Antigravity)
    logged_in: Option<bool>,
    /// "installing" | "signing_in"
    busy: Option<String>,
}

struct Core {
    settings: Mutex<Settings>,
    status: Mutex<HashMap<String, ToolStatus>>,
    running: AtomicBool,
    progress: Mutex<Option<Progress>>,
    /// When the panel was hidden by losing focus (ms); keeps that same tray click from reopening it
    hidden_at: AtomicI64,
    config_file: PathBuf,
    log_dir: PathBuf,
    mini_awake: Mutex<Option<platform::Awake>>,
    wake_message: Mutex<String>,
}

impl Core {
    fn ko(&self) -> bool {
        self.settings.lock().unwrap().lang == "ko"
    }

    fn t(&self, en: &str, ko: &str) -> String {
        if self.ko() { ko.into() } else { en.into() }
    }

    fn save(&self, s: &Settings) {
        if let Some(dir) = self.config_file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&self.config_file, serde_json::to_vec_pretty(s).unwrap_or_default());
    }

    fn log(&self, line: &str) {
        let _ = std::fs::create_dir_all(&self.log_dir);
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.log_dir.join("ping.log")) {
            let _ = writeln!(f, "{}  {line}", Local::now().format("%Y-%m-%d %H:%M:%S"));
        }
    }

    /// Always-on mode: block system sleep while enabled
    fn apply_power(&self) {
        let want = {
            let s = self.settings.lock().unwrap();
            s.enabled && s.mode == Mode::Mini
        };
        // On Windows the scheduler thread applies this every tick (it is per-thread there)
        if cfg!(not(windows)) {
            let mut a = self.mini_awake.lock().unwrap();
            if want && a.is_none() {
                *a = Some(platform::Awake::hold());
            } else if !want {
                *a = None;
            }
        }
    }
}

// MARK: Schedule

fn schedule(s: &Settings) -> Vec<u32> {
    (0..s.pings_per_day.clamp(1, 4)).map(|k| (s.first_ping + k * GAP) % 1440).collect()
}

fn at(day_offset: i64, minutes: u32) -> DateTime<Local> {
    let day = Local::now().date_naive() + CDuration::days(day_offset);
    let naive = day.and_hms_opt(minutes / 60, minutes % 60, 0).unwrap();
    // A time skipped by daylight saving moves one hour later
    Local.from_local_datetime(&naive).earliest()
        .unwrap_or_else(|| Local.from_local_datetime(&(naive + CDuration::hours(1))).earliest().unwrap())
}

fn slots(s: &Settings, offsets: &[i64]) -> Vec<DateTime<Local>> {
    offsets.iter().flat_map(|&o| schedule(s).into_iter().map(move |m| at(o, m))).collect()
}

fn next_slot(s: &Settings) -> Option<DateTime<Local>> {
    let now = Local::now();
    slots(s, &[0, 1]).into_iter().filter(|d| *d > now).min()
}

fn recent_slot(s: &Settings) -> Option<DateTime<Local>> {
    let now = Local::now();
    slots(s, &[-1, 0]).into_iter().filter(|d| *d <= now).max()
}

/// Laptop wake times: 1 minute before each ping
fn wake_times(s: &Settings) -> Vec<u32> {
    schedule(s).iter().map(|m| (m + 1439) % 1440).collect()
}

// MARK: Pinging

fn tick(app: &AppHandle, core: &Arc<Core>) {
    #[cfg(windows)]
    {
        let s = core.settings.lock().unwrap();
        platform::set_thread_awake(s.enabled && s.mode == Mode::Mini);
    }
    let fire = {
        let mut s = core.settings.lock().unwrap();
        let now = Local::now();
        match recent_slot(&s) {
            // Skip slots more than 10 minutes old: opening a window at a random later time would shift the day
            Some(slot) if s.enabled && !core.running.load(Ordering::SeqCst)
                && slot.timestamp() > s.last_slot && (now - slot).num_seconds() < 600 => {
                s.last_slot = slot.timestamp();
                core.save(&s);
                Some(slot)
            }
            _ => None,
        }
    };
    if let Some(slot) = fire {
        run_all(app, core, &format!("scheduled {}", slot.format("%H:%M")));
    }
    update_tray(app, core);
}

fn check_tools(core: &Core, path: &str) {
    let home = platform::home();
    for t in tools::all() {
        let installed = platform::find_exe(t.id, path).is_some();
        let logged_in = if !installed {
            Some(false)
        } else {
            match t.id {
                "claude" => {
                    let (_, out) = platform::run_tool(&["claude".into(), "auth".into(), "status".into(), "--json".into()],
                                                      path, &core.log_dir, Duration::from_secs(20));
                    Some(out.contains("\"loggedIn\": true") || out.contains("\"loggedIn\":true"))
                }
                "codex" => Some(platform::run_tool(&["codex".into(), "login".into(), "status".into()],
                                                   path, &core.log_dir, Duration::from_secs(20)).0),
                "grok" => Some(home.join(".grok").join("auth.json").exists()),
                _ => None,
            }
        };
        let mut st = core.status.lock().unwrap();
        let e = st.entry(t.id.into()).or_default();
        e.installed = Some(installed);
        e.logged_in = logged_in;
    }
}

fn run_all(app: &AppHandle, core: &Arc<Core>, trigger: &str) {
    if core.running.swap(true, Ordering::SeqCst) {
        return;
    }
    let _awake = platform::Awake::hold();
    let path = platform::user_path();
    core.log(&format!("── {trigger}"));

    let on = core.settings.lock().unwrap().tools_on.clone();
    let targets: Vec<_> = tools::all().into_iter()
        .filter(|t| on.get(t.id).copied().unwrap_or(true) && platform::find_exe(t.id, &path).is_some())
        .collect();
    let total = targets.len();
    let mut failed = Vec::new();

    for (i, t) in targets.iter().enumerate() {
        *core.progress.lock().unwrap() = Some(Progress { done: i, total, current: t.name.into() });
        update_tray(app, core);
        let _ = app.emit("changed", ());

        let start = std::time::Instant::now();
        let run = |cmds: &Vec<Vec<String>>| {
            let mut last = (true, String::new());
            for c in cmds {
                last = platform::run_tool(c, &path, &core.log_dir, CALL_TIMEOUT);
                if !last.0 {
                    break;
                }
            }
            last
        };
        let mut r = run(&t.light);
        if !r.0 {
            r = run(&t.plain);
        }
        // Keep the "ok" reply if present, otherwise the last line (usually the error)
        let lines: Vec<&str> = r.1.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        let msg: String = lines.iter().rev().find(|l| l.eq_ignore_ascii_case("ok")).or(lines.last())
            .copied().unwrap_or("").chars().take(200).collect();
        core.log(&format!("{:<11} {}  {}s  {}", t.name, if r.0 { "OK  " } else { "FAIL" }, start.elapsed().as_secs(), msg));
        if !r.0 {
            failed.push(t.name.to_string());
        }
        let mut s = core.settings.lock().unwrap();
        s.results.insert(t.id.into(), ToolResult { ok: r.0, at: Local::now().timestamp(), message: msg });
        core.save(&s);
    }

    {
        let mut s = core.settings.lock().unwrap();
        s.last_run = Some(RunSummary { at: Local::now().timestamp(), ok: total - failed.len(), total, failed: failed.clone() });
        core.save(&s);
    }
    *core.progress.lock().unwrap() = None;
    check_tools(core, &path);
    core.running.store(false, Ordering::SeqCst);

    if !failed.is_empty() {
        let body = core.t(&format!("Ping failed: {} — check the sign-in", failed.join(", ")),
                          &format!("호출 실패: {} — 로그인 상태를 확인하세요", failed.join(", ")));
        let _ = app.notification().builder().title("Earlybolt").body(body).show();
    }
    update_tray(app, core);
    let _ = app.emit("changed", ());
}

// MARK: Tray

fn tray_icon(core: &Core) -> Image<'static> {
    let (enabled, failed) = {
        let s = core.settings.lock().unwrap();
        (s.enabled, s.last_run.as_ref().map(|r| !r.failed.is_empty()).unwrap_or(false))
    };
    // macOS: monochrome template for the menu bar / Windows: color icon for the taskbar
    let bytes: &[u8] = match (cfg!(target_os = "macos"), enabled, failed) {
        (true, false, _) => include_bytes!("../icons/tray-mac-off.png"),
        (true, true, true) => include_bytes!("../icons/tray-mac-warn.png"),
        (true, true, false) => include_bytes!("../icons/tray-mac-on.png"),
        (false, false, _) => include_bytes!("../icons/tray-win-off.png"),
        (false, true, true) => include_bytes!("../icons/tray-win-warn.png"),
        (false, true, false) => include_bytes!("../icons/tray-win-on.png"),
    };
    Image::from_bytes(bytes).expect("tray icon")
}

fn tray_menu(app: &AppHandle, core: &Core) -> tauri::Result<Menu<tauri::Wry>> {
    Menu::with_items(app, &[
        &MenuItem::with_id(app, "open", core.t("Open Earlybolt", "Earlybolt 열기"), true, None::<&str>)?,
        &MenuItem::with_id(app, "run", core.t("Ping now", "지금 호출"), true, None::<&str>)?,
        &MenuItem::with_id(app, "guide", core.t("Guide", "설명"), true, None::<&str>)?,
        &MenuItem::with_id(app, "quit", core.t("Quit", "종료"), true, None::<&str>)?,
    ])
}

fn update_tray(app: &AppHandle, core: &Arc<Core>) {
    let Some(tray) = app.tray_by_id("main") else { return };
    let (enabled, show_time, next) = {
        let s = core.settings.lock().unwrap();
        (s.enabled, s.show_time_in_tray, next_slot(&s))
    };
    let progress = core.progress.lock().unwrap().clone();
    let next_txt = next.map(|d| d.format("%H:%M").to_string()).unwrap_or_default();
    let tip = if !enabled {
        core.t("Earlybolt · Off", "Earlybolt · 꺼짐")
    } else if let Some(p) = &progress {
        format!("Earlybolt · {}/{} {}", p.done + 1, p.total, p.current)
    } else {
        core.t(&format!("Earlybolt · Next ping {next_txt}"), &format!("Earlybolt · 다음 호출 {next_txt}"))
    };
    let _ = tray.set_icon(Some(tray_icon(core)));
    let _ = tray.set_icon_as_template(cfg!(target_os = "macos"));
    let _ = tray.set_tooltip(Some(&tip));
    #[cfg(target_os = "macos")]
    {
        let title = match (&progress, enabled && show_time) {
            (Some(p), _) => Some(format!("{}/{}", p.done + 1, p.total)),
            (None, true) => Some(next_txt),
            _ => None,
        };
        let _ = tray.set_title(title);
    }
    let _ = show_time;
}

fn place_panel(w: &tauri::WebviewWindow) {
    let pos = if cfg!(target_os = "macos") { Position::TrayCenter } else { Position::TrayBottomCenter };
    // Before the first tray click the tray position is unknown; fall back to a screen corner
    if w.move_window(pos).is_err() {
        let _ = w.move_window(if cfg!(target_os = "macos") { Position::TopRight } else { Position::BottomRight });
    }
}

fn toggle_panel(app: &AppHandle) {
    let Some(w) = app.get_webview_window("panel") else { return };
    let core = app.state::<Arc<Core>>();
    if w.is_visible().unwrap_or(false) {
        let _ = w.hide();
        return;
    }
    // If focus loss just hid it, this click was meant to close it
    if Local::now().timestamp_millis() - core.hidden_at.load(Ordering::SeqCst) < 300 {
        return;
    }
    place_panel(&w);
    let _ = w.show();
    let _ = w.set_focus();
    let _ = app.emit("panel-shown", ());
}

fn open_guide(app: &AppHandle) {
    // Menu bar apps are not activated by default, so the window could open behind others
    #[cfg(target_os = "macos")]
    let _ = app.show();
    if let Some(w) = app.get_webview_window("guide") {
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let title = app.state::<Arc<Core>>().t("Earlybolt Guide", "Earlybolt 설명");
    if let Ok(w) = WebviewWindowBuilder::new(app, "guide", WebviewUrl::App("guide.html".into()))
        .title(title)
        .inner_size(640.0, 760.0)
        .center()
        .build()
    {
        let _ = w.set_focus();
    }
}

// MARK: State for the UI

#[derive(Serialize)]
struct ToolDto {
    id: &'static str,
    name: &'static str,
    on: bool,
    installed: Option<bool>,
    logged_in: Option<bool>,
    busy: Option<String>,
    result: Option<ToolResult>,
}

#[derive(Serialize)]
struct StateDto {
    os: &'static str,
    lang: String,
    now: i64,
    enabled: bool,
    mode: Mode,
    work_start: u32,
    first_ping: u32,
    recommended_first: u32,
    pings_per_day: u32,
    schedule: Vec<u32>,
    next_slot: Option<i64>,
    current_window: Option<(i64, i64)>,
    running: bool,
    progress: Option<Progress>,
    last_run: Option<RunSummary>,
    show_time_in_tray: bool,
    wake_enabled: bool,
    wake_message: String,
    autostart: bool,
    tools: Vec<ToolDto>,
    version: String,
}

#[tauri::command]
fn get_state(app: AppHandle, core: State<Arc<Core>>) -> StateDto {
    let s = core.settings.lock().unwrap().clone();
    let st = core.status.lock().unwrap().clone();
    let now = Local::now();
    let current = recent_slot(&s).filter(|d| now < *d + CDuration::hours(5))
        .map(|d| (d.timestamp(), (d + CDuration::hours(5)).timestamp()));
    StateDto {
        os: if cfg!(windows) { "windows" } else { "macos" },
        lang: s.lang.clone(),
        now: now.timestamp(),
        enabled: s.enabled,
        mode: s.mode,
        work_start: s.work_start,
        first_ping: s.first_ping,
        recommended_first: (s.work_start + 1440 - 180) % 1440,
        pings_per_day: s.pings_per_day,
        schedule: schedule(&s),
        next_slot: next_slot(&s).map(|d| d.timestamp()),
        current_window: current,
        running: core.running.load(Ordering::SeqCst),
        progress: core.progress.lock().unwrap().clone(),
        last_run: s.last_run.clone(),
        show_time_in_tray: s.show_time_in_tray,
        wake_enabled: s.wake_enabled,
        wake_message: core.wake_message.lock().unwrap().clone(),
        autostart: app.autolaunch().is_enabled().unwrap_or(false),
        tools: tools::all().into_iter().map(|t| {
            let ts = st.get(t.id).cloned().unwrap_or_default();
            ToolDto {
                id: t.id,
                name: t.name,
                on: s.tools_on.get(t.id).copied().unwrap_or(true),
                installed: ts.installed,
                logged_in: ts.logged_in,
                busy: ts.busy,
                result: s.results.get(t.id).cloned(),
            }
        }).collect(),
        version: app.package_info().version.to_string(),
    }
}

/// Merge only the fields the UI changed
#[tauri::command]
fn update_settings(app: AppHandle, core: State<Arc<Core>>, patch: serde_json::Value) {
    let core = core.inner().clone();
    let (schedule_changed, rewake, lang_changed) = {
        let mut s = core.settings.lock().unwrap();
        let before = (s.enabled, s.first_ping, s.pings_per_day);
        let lang_before = s.lang.clone();
        let mut v = serde_json::to_value(&*s).unwrap();
        if let (Some(obj), Some(p)) = (v.as_object_mut(), patch.as_object()) {
            for (k, val) in p {
                if k == "tools_on" {
                    if let (Some(dst), Some(src)) = (obj.get_mut(k).and_then(|x| x.as_object_mut()), val.as_object()) {
                        for (tk, tv) in src {
                            dst.insert(tk.clone(), tv.clone());
                        }
                    }
                } else if obj.contains_key(k) {
                    obj.insert(k.clone(), val.clone());
                }
            }
        }
        if let Ok(n) = serde_json::from_value::<Settings>(v) {
            *s = n;
        }
        let changed = before != (s.enabled, s.first_ping, s.pings_per_day);
        // After turning on or changing the schedule, start from the next slot rather than right now
        if changed {
            s.last_slot = Local::now().timestamp();
        }
        core.save(&s);
        (changed, cfg!(windows) && s.wake_enabled && s.mode == Mode::Laptop, lang_before != s.lang)
    };
    core.apply_power();
    // Windows can re-register wake tasks without admin rights, so keep them in sync automatically
    if schedule_changed && rewake {
        let times = wake_times(&core.settings.lock().unwrap());
        let msg = platform::set_wake(&times, true, core.ko()).unwrap_or_else(|e| e);
        *core.wake_message.lock().unwrap() = msg;
    }
    if lang_changed {
        if let (Some(tray), Ok(menu)) = (app.tray_by_id("main"), tray_menu(&app, &core)) {
            let _ = tray.set_menu(Some(menu));
        }
        if let Some(w) = app.get_webview_window("guide") {
            let _ = w.set_title(&core.t("Earlybolt Guide", "Earlybolt 설명"));
        }
        let _ = app.emit("changed", ());
    }
    update_tray(&app, &core);
}

#[tauri::command]
fn run_now(app: AppHandle, core: State<Arc<Core>>) {
    let core = core.inner().clone();
    thread::spawn(move || run_all(&app, &core, "manual"));
}

#[tauri::command]
fn refresh_tools(app: AppHandle, core: State<Arc<Core>>) {
    let core = core.inner().clone();
    thread::spawn(move || {
        check_tools(&core, &platform::user_path());
        let _ = app.emit("changed", ());
    });
}

fn set_busy(core: &Core, id: &str, busy: Option<&str>) {
    core.status.lock().unwrap().entry(id.into()).or_default().busy = busy.map(String::from);
}

#[tauri::command]
fn install_tool(app: AppHandle, core: State<Arc<Core>>, id: String) {
    let core = core.inner().clone();
    let Some(t) = tools::all().into_iter().find(|t| t.id == id) else { return };
    set_busy(&core, t.id, Some("installing"));
    let _ = app.emit("changed", ());
    thread::spawn(move || {
        let (ok, out) = platform::run_installer(t.install_sh, t.install_ps1, &platform::user_path());
        let tail: String = out.lines().rev().take(3).collect::<Vec<_>>().join(" / ");
        core.log(&format!("{:<11} install {}  {}", t.name, if ok { "done" } else { "FAILED" }, tail));
        set_busy(&core, t.id, None);
        check_tools(&core, &platform::user_path());
        if !ok {
            let body = core.t(&format!("Could not install {}. See the log.", t.name),
                              &format!("{} 설치에 실패했어요. 기록을 확인하세요.", t.name));
            let _ = app.notification().builder().title("Earlybolt").body(body).show();
        }
        let _ = app.emit("changed", ());
    });
}

#[tauri::command]
fn login_tool(app: AppHandle, core: State<Arc<Core>>, id: String) -> Result<(), String> {
    let core = core.inner().clone();
    let t = tools::all().into_iter().find(|t| t.id == id).ok_or("unknown tool")?;
    platform::open_login_terminal(t.login, &platform::user_path(), core.ko())?;
    set_busy(&core, t.id, Some("signing_in"));
    let _ = app.emit("changed", ());
    // Watch for the sign-in to complete for up to 5 minutes
    thread::spawn(move || {
        for _ in 0..60 {
            thread::sleep(Duration::from_secs(5));
            check_tools(&core, &platform::user_path());
            if core.status.lock().unwrap().get(t.id).and_then(|s| s.logged_in) == Some(true) {
                break;
            }
            let _ = app.emit("changed", ());
        }
        set_busy(&core, t.id, None);
        let _ = app.emit("changed", ());
    });
    Ok(())
}

#[tauri::command]
fn set_wake(core: State<Arc<Core>>, on: bool) -> String {
    let times = wake_times(&core.settings.lock().unwrap());
    let r = platform::set_wake(&times, on, core.ko());
    let msg = r.clone().unwrap_or_else(|e| e);
    if r.is_ok() {
        let mut s = core.settings.lock().unwrap();
        s.wake_enabled = on;
        core.save(&s);
    }
    *core.wake_message.lock().unwrap() = msg.clone();
    msg
}

#[tauri::command]
fn set_autostart(app: AppHandle, on: bool) {
    let a = app.autolaunch();
    let _ = if on { a.enable() } else { a.disable() };
}

#[tauri::command]
fn open_log(app: AppHandle, core: State<Arc<Core>>) {
    let file = core.log_dir.join("ping.log");
    if !file.exists() {
        core.log("log started");
    }
    let _ = app.opener().open_path(file.display().to_string(), None::<&str>);
}

#[tauri::command]
fn show_guide(app: AppHandle) {
    open_guide(&app);
}

/// The panel sizes itself to its content (Home and Settings differ in height)
#[tauri::command]
fn fit_panel(app: AppHandle, height: f64) {
    if let Some(w) = app.get_webview_window("panel") {
        let _ = w.set_size(LogicalSize::new(PANEL_WIDTH, height.clamp(200.0, 900.0)));
        // On Windows the panel sits above the taskbar, so re-anchor it after the height changes
        if cfg!(windows) && w.is_visible().unwrap_or(false) {
            place_panel(&w);
        }
    }
}

#[tauri::command]
fn hide_panel(app: AppHandle) {
    if let Some(w) = app.get_webview_window("panel") {
        let _ = w.hide();
    }
}

#[tauri::command]
fn quit(app: AppHandle) {
    app.exit(0);
}

// MARK: Startup

pub fn run() {
    // For testing: `earlybolt --run-once` pings once without UI and exits
    let run_once = std::env::args().any(|a| a == "--run-once");
    // Development (EARLYBOLT_DEV): never register the build folder as a login item
    let dev = std::env::var("EARLYBOLT_DEV").is_ok();

    let mut builder = tauri::Builder::default();
    if !run_once {
        // Launching again toggles the panel; `earlybolt --settings` opens it on Settings
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, _| {
            if args.iter().any(|a| a == "--settings") {
                if let Some(w) = app.get_webview_window("panel") {
                    if !w.is_visible().unwrap_or(false) {
                        toggle_panel(app);
                    }
                }
                let _ = app.emit("show-view", "settings");
            } else {
                toggle_panel(app);
            }
        }));
    }
    builder
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, None))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_positioner::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            get_state, update_settings, run_now, refresh_tools, install_tool, login_tool,
            set_wake, set_autostart, open_log, show_guide, fit_panel, hide_panel, quit
        ])
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let config_file = app.path().app_config_dir()?.join("settings.json");
            let log_dir = app.path().app_log_dir()?;
            let settings: Settings = std::fs::read(&config_file).ok()
                .and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
            let core = Arc::new(Core {
                settings: Mutex::new(settings),
                status: Mutex::new(HashMap::new()),
                running: AtomicBool::new(false),
                progress: Mutex::new(None),
                hidden_at: AtomicI64::new(0),
                config_file,
                log_dir,
                mini_awake: Mutex::new(None),
                wake_message: Mutex::new(String::new()),
            });
            app.manage(core.clone());

            if run_once {
                let handle = app.handle().clone();
                thread::spawn(move || {
                    run_all(&handle, &core, "command line");
                    let log = std::fs::read_to_string(core.log_dir.join("ping.log")).unwrap_or_default();
                    let lines: Vec<&str> = log.lines().collect();
                    println!("{}", lines[lines.len().saturating_sub(6)..].join("\n"));
                    handle.exit(0);
                });
                return Ok(());
            }

            platform::cleanup_legacy();

            TrayIconBuilder::with_id("main")
                .icon(tray_icon(&core))
                .icon_as_template(cfg!(target_os = "macos"))
                .menu(&tray_menu(app.handle(), &core)?)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, e| match e.id.as_ref() {
                    "open" => toggle_panel(app),
                    "run" => {
                        let (app, core) = (app.clone(), app.state::<Arc<Core>>().inner().clone());
                        thread::spawn(move || run_all(&app, &core, "manual"));
                    }
                    "guide" => open_guide(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    tauri_plugin_positioner::on_tray_event(tray.app_handle(), &event);
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                        toggle_panel(tray.app_handle());
                    }
                })
                .build(app)?;

            // Clicking outside closes the panel
            if let Some(w) = app.get_webview_window("panel") {
                let handle = app.handle().clone();
                w.on_window_event(move |e| {
                    if let WindowEvent::Focused(false) = e {
                        if let Some(w) = handle.get_webview_window("panel") {
                            let _ = w.hide();
                        }
                        handle.state::<Arc<Core>>().hidden_at.store(Local::now().timestamp_millis(), Ordering::SeqCst);
                    }
                });
            }

            core.apply_power();

            // First launch: turn on launch-at-login and show the guide
            let first = !core.settings.lock().unwrap().first_run_done;
            if first {
                if !dev {
                    let _ = app.autolaunch().enable();
                }
                let mut s = core.settings.lock().unwrap();
                s.first_run_done = true;
                core.save(&s);
                drop(s);
                open_guide(app.handle());
            }

            // Scheduler: check for due slots every 20 seconds
            let handle = app.handle().clone();
            let c = core.clone();
            thread::spawn(move || {
                check_tools(&c, &platform::user_path());
                let _ = handle.emit("changed", ());
                loop {
                    tick(&handle, &c);
                    thread::sleep(Duration::from_secs(20));
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to start Earlybolt")
        .run(|_, e| {
            // A tray app keeps running after its windows close
            if let tauri::RunEvent::ExitRequested { api, code: None, .. } = e {
                api.prevent_exit();
            }
        });
}
