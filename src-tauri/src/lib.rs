mod audio;
mod ffmpeg;
mod recorder;

use audio::{Capture, Kind};
use recorder::{CamTrack, ExportOpts, Screen, Session, Track};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Default)]
struct Levels {
    mic: Arc<AtomicU32>,
    sys: Arc<AtomicU32>,
}

struct Recording {
    dir: PathBuf,
    screen: Screen,
    mic: Option<Capture>,
    sys: Option<Capture>,
    opts: StartOpts,
    started_ms: u64,
}

#[derive(Default)]
struct Inner {
    rec: Option<Recording>,
    mon_mic: Option<Capture>,
    mon_sys: Option<Capture>,
    /// Directory the camera window streams chunks into.
    cam_dir: Option<PathBuf>,
    cam_finished: Option<u64>,
    last_session: Option<PathBuf>,
}

#[derive(Default)]
struct AppState {
    inner: Mutex<Inner>,
    levels: Levels,
}

fn e2s(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn recly_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let d = app.path().video_dir().map_err(e2s)?.join("Recly");
    std::fs::create_dir_all(&d).map_err(e2s)?;
    Ok(d)
}

// ---------- windows ----------

#[cfg(windows)]
fn exclude_from_capture(win: &WebviewWindow) {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn SetWindowDisplayAffinity(hwnd: isize, affinity: u32) -> i32;
    }
    const WDA_EXCLUDEFROMCAPTURE: u32 = 0x11;
    if let Ok(h) = win.hwnd() {
        unsafe {
            SetWindowDisplayAffinity(h.0 as isize, WDA_EXCLUDEFROMCAPTURE);
        }
    }
}
#[cfg(not(windows))]
fn exclude_from_capture(_: &WebviewWindow) {}

/// Logical work area of the primary monitor: (x, y, w, h).
fn primary_area(app: &AppHandle) -> (f64, f64, f64, f64) {
    if let Ok(Some(m)) = app.primary_monitor() {
        let s = m.scale_factor();
        let p = m.position().to_logical::<f64>(s);
        let z = m.size().to_logical::<f64>(s);
        return (p.x, p.y, z.width, z.height);
    }
    (0.0, 0.0, 1920.0, 1080.0)
}

/// Serializes window creation so concurrent calls can't create the same label twice.
static WINDOW_LOCK: Mutex<()> = Mutex::new(());

fn overlay(app: &AppHandle, label: &str, w: f64, h: f64, x: f64, y: f64) -> Result<WebviewWindow, String> {
    let _guard = WINDOW_LOCK.lock().unwrap();
    if let Some(win) = app.get_webview_window(label) {
        let _ = win.show();
        return Ok(win);
    }
    let win = WebviewWindowBuilder::new(app, label, WebviewUrl::App("index.html".into()))
        .title("Recly")
        .inner_size(w, h)
        .position(x, y)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .shadow(false)
        .focused(false)
        .build()
        .map_err(e2s)?;
    exclude_from_capture(&win);
    Ok(win)
}

const CAM_WIN: (f64, f64) = (232.0, 268.0);
const CAM_BUBBLE: (f64, f64, f64) = (16.0, 16.0, 200.0); // offset x, y, size inside the window

// Async: creating a window from a sync command (main thread) deadlocks on Windows.
#[tauri::command]
async fn show_camera(app: AppHandle, show: bool) -> Result<(), String> {
    if show {
        let (x, y, _, h) = primary_area(&app);
        overlay(&app, "camera", CAM_WIN.0, CAM_WIN.1, x + 24.0, y + h - CAM_WIN.1 - 64.0)?;
    } else if let Some(w) = app.get_webview_window("camera") {
        let _ = w.close();
    }
    Ok(())
}

fn open_window(app: &AppHandle, label: &str, w: f64, h: f64) -> Result<(), String> {
    let _guard = WINDOW_LOCK.lock().unwrap();
    if let Some(win) = app.get_webview_window(label) {
        let _ = win.show();
        let _ = win.set_focus();
        let _ = app.emit_to(label, "reload-session", ());
        return Ok(());
    }
    WebviewWindowBuilder::new(app, label, WebviewUrl::App("index.html".into()))
        .title("Recly")
        .inner_size(w, h)
        .center()
        .decorations(false)
        .transparent(true)
        .resizable(false)
        .shadow(false)
        .build()
        .map_err(e2s)?;
    Ok(())
}

#[tauri::command]
fn show_main(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

// ---------- devices & levels ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DisplayInfo {
    index: u32,
    name: String,
    width: u32,
    height: u32,
    primary: bool,
}

#[tauri::command]
fn list_displays(app: AppHandle) -> Vec<DisplayInfo> {
    let primary = app.primary_monitor().ok().flatten().map(|m| *m.position());
    app.available_monitors()
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(i, m)| DisplayInfo {
            index: i as u32,
            name: m.name().cloned().unwrap_or_else(|| format!("Display {}", i + 1)),
            width: m.size().width,
            height: m.size().height,
            primary: Some(*m.position()) == primary,
        })
        .collect()
}

#[tauri::command]
async fn list_audio() -> audio::AudioDevices {
    tauri::async_runtime::spawn_blocking(audio::devices).await.unwrap_or(audio::AudioDevices {
        inputs: vec![],
        default_input: None,
        output: None,
    })
}

/// Live meters on the setup screen (no files written).
#[tauri::command]
async fn set_monitor(app: AppHandle, mic: bool, mic_device: Option<String>, system: bool) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let (old_mic, old_sys) = {
            let mut g = st.inner.lock().unwrap();
            if g.rec.is_some() {
                return Ok(());
            }
            (g.mon_mic.take(), g.mon_sys.take())
        };
        old_mic.map(Capture::stop);
        old_sys.map(Capture::stop);
        let m = mic
            .then(|| Capture::start(Kind::Mic(mic_device), None, st.levels.mic.clone()).ok())
            .flatten();
        let s = system
            .then(|| Capture::start(Kind::System, None, st.levels.sys.clone()).ok())
            .flatten();
        let mut g = st.inner.lock().unwrap();
        g.mon_mic = m;
        g.mon_sys = s;
        Ok(())
    })
    .await
    .map_err(e2s)?
}

// ---------- recording ----------

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct StartOpts {
    display: u32,
    fps: u32,
    mic: bool,
    mic_device: Option<String>,
    system: bool,
    camera: bool,
    shape: String,
    layout: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StartResult {
    started_ms: u64,
    warnings: Vec<String>,
}

#[tauri::command]
async fn start_recording(app: AppHandle, opts: StartOpts) -> Result<StartResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let (old_mic, old_sys) = {
            let mut g = st.inner.lock().unwrap();
            if g.rec.is_some() {
                return Err("กำลังอัดอยู่แล้ว".to_string());
            }
            (g.mon_mic.take(), g.mon_sys.take())
        };
        old_mic.map(Capture::stop);
        old_sys.map(Capture::stop);

        let dir = recly_dir(&app)?
            .join("sessions")
            .join(chrono::Local::now().format("%Y%m%d-%H%M%S").to_string());
        std::fs::create_dir_all(&dir).map_err(e2s)?;

        let fps = if opts.fps == 60 { 60 } else { 30 };
        let screen = Screen::start(opts.display, fps, &dir.join("screen.mp4"))?;

        let mut warnings = vec![];
        let mic = if opts.mic {
            Capture::start(Kind::Mic(opts.mic_device.clone()), Some(dir.join("mic.wav")), st.levels.mic.clone())
                .map_err(|e| warnings.push(format!("ไมโครโฟน: {e}")))
                .ok()
        } else {
            None
        };
        let sys = if opts.system {
            Capture::start(Kind::System, Some(dir.join("system.wav")), st.levels.sys.clone())
                .map_err(|e| warnings.push(format!("เสียงในเครื่อง: {e}")))
                .ok()
        } else {
            None
        };

        let started_ms = now_ms();
        {
            let mut g = st.inner.lock().unwrap();
            g.cam_dir = Some(dir.clone());
            g.cam_finished = None;
            g.rec = Some(Recording { dir: dir.clone(), screen, mic, sys, opts: opts.clone(), started_ms });
        }

        if let Some(w) = app.get_webview_window("main") {
            let _ = w.hide();
        }
        let (x, y, w, h) = primary_area(&app);
        let (bw, bh) = (420.0, 72.0);
        overlay(&app, "recbar", bw, bh, x + (w - bw) / 2.0, y + h - bh - 56.0)?;
        if opts.camera {
            let _ = app.emit_to("camera", "rec-start", started_ms);
        }
        Ok(StartResult { started_ms, warnings })
    })
    .await
    .map_err(e2s)?
}

#[tauri::command]
fn recording_started(app: AppHandle) -> Option<u64> {
    app.state::<AppState>().inner.lock().unwrap().rec.as_ref().map(|r| r.started_ms)
}

struct Finished {
    dir: PathBuf,
    opts: StartOpts,
    screen_start: u64,
    mic_start: Option<u64>,
    sys_start: Option<u64>,
    cam_start: Option<u64>,
}

/// Stop all streams and close the recording bar.
fn finish(app: &AppHandle) -> Option<Finished> {
    let st = app.state::<AppState>();
    let rec = st.inner.lock().unwrap().rec.take()?;
    let _ = app.emit_to("camera", "rec-stop", ());
    let Recording { dir, screen, mic, sys, opts, .. } = rec;
    let screen_start = screen.stop();
    let mic_start = mic.and_then(Capture::stop);
    let sys_start = sys.and_then(Capture::stop);

    // Wait for the camera window to flush its last chunk.
    let mut cam_start = None;
    if opts.camera {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(6) {
            if let Some(ms) = st.inner.lock().unwrap().cam_finished {
                cam_start = Some(ms);
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    st.inner.lock().unwrap().cam_dir = None;
    if let Some(w) = app.get_webview_window("recbar") {
        let _ = w.close();
    }
    Some(Finished { dir, opts, screen_start, mic_start, sys_start, cam_start })
}

/// Camera bubble rect normalised to the recorded display.
fn cam_rect(app: &AppHandle, display: u32) -> (f64, f64, f64) {
    let fallback = (0.03, 0.72, 0.16);
    let Some(win) = app.get_webview_window("camera") else { return fallback };
    let monitors = app.available_monitors().unwrap_or_default();
    let Some(m) = monitors.get(display as usize).or(monitors.first()) else { return fallback };
    let (Ok(p), Ok(sf)) = (win.outer_position(), win.scale_factor()) else { return fallback };
    let (mx, my) = (m.position().x as f64, m.position().y as f64);
    let (mw, mh) = (m.size().width as f64, m.size().height as f64);
    let bx = p.x as f64 + CAM_BUBBLE.0 * sf;
    let by = p.y as f64 + CAM_BUBBLE.1 * sf;
    let bs = CAM_BUBBLE.2 * sf;
    (((bx - mx) / mw).clamp(0.0, 1.0), ((by - my) / mh).clamp(0.0, 1.0), (bs / mw).clamp(0.05, 0.6))
}

#[tauri::command]
async fn stop_recording(app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let Some(Finished { dir, opts, screen_start, mic_start, sys_start, cam_start }) = finish(&app) else {
            return Err("ไม่ได้อัดอยู่".to_string());
        };
        let screen = dir.join("screen.mp4");
        let probe = ffmpeg::probe(&screen).ok_or("อ่านไฟล์วิดีโอไม่ได้")?;
        let track = |name: &str, start: Option<u64>| {
            let p = dir.join(name);
            match start {
                Some(s) if p.exists() => Some(Track { path: p.to_string_lossy().into(), start: s }),
                _ => {
                    let _ = std::fs::remove_file(&p);
                    None
                }
            }
        };
        let cam_file = dir.join("cam.webm");
        let cam = match cam_start {
            Some(s) if cam_file.exists() => {
                let (x, y, size) = cam_rect(&app, opts.display);
                Some(CamTrack { path: cam_file.to_string_lossy().into(), start: s, x, y, size })
            }
            _ => None,
        };
        let session = Session {
            dir: dir.to_string_lossy().into(),
            screen: screen.to_string_lossy().into(),
            screen_start,
            width: probe.width,
            height: probe.height,
            duration: probe.duration,
            mic: track("mic.wav", mic_start),
            system: track("system.wav", sys_start),
            cam,
            layout: opts.layout,
            shape: opts.shape,
        };
        session.save()?;
        app.state::<AppState>().inner.lock().unwrap().last_session = Some(dir);
        open_window(&app, "export", 1120.0, 720.0)
    })
    .await
    .map_err(e2s)?
}

#[tauri::command]
async fn discard_recording(app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        if let Some(f) = finish(&app) {
            let _ = std::fs::remove_dir_all(&f.dir);
        }
        show_main(app);
        Ok(())
    })
    .await
    .map_err(e2s)?
}

/// Raw webm chunk from the camera window's MediaRecorder.
#[tauri::command]
fn cam_chunk(app: AppHandle, request: tauri::ipc::Request<'_>) -> Result<(), String> {
    let tauri::ipc::InvokeBody::Raw(data) = request.body() else {
        return Err("expected raw body".into());
    };
    let dir = app.state::<AppState>().inner.lock().unwrap().cam_dir.clone();
    let Some(dir) = dir else { return Ok(()) };
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("cam.webm"))
        .map_err(e2s)?;
    f.write_all(data).map_err(e2s)
}

#[tauri::command]
fn cam_finished(app: AppHandle, start_ms: u64) {
    app.state::<AppState>().inner.lock().unwrap().cam_finished = Some(start_ms);
}

// ---------- export ----------

#[tauri::command]
fn current_session(app: AppHandle) -> Result<Session, String> {
    let dir = app.state::<AppState>().inner.lock().unwrap().last_session.clone();
    Session::load(&dir.ok_or("ยังไม่มีงานที่อัดไว้")?)
}

#[tauri::command]
async fn waveform(dir: String) -> Vec<f32> {
    tauri::async_runtime::spawn_blocking(move || {
        let d = Path::new(&dir);
        let a = audio::waveform(&d.join("mic.wav"), 160);
        let b = audio::waveform(&d.join("system.wav"), 160);
        if a.is_empty() {
            return b;
        }
        if b.is_empty() {
            return a;
        }
        a.iter().zip(&b).map(|(x, y)| x.max(*y)).collect()
    })
    .await
    .unwrap_or_default()
}

#[tauri::command]
async fn export_video(app: AppHandle, dir: String, opts: ExportOpts) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let session = Session::load(Path::new(&dir))?;
        let tag = if opts.layout == "vertical" { "9x16" } else { "16x9" };
        let name = format!("Recly {} {tag}.mp4", chrono::Local::now().format("%Y-%m-%d %H-%M-%S"));
        let out = recly_dir(&app)?.join(name);
        let a = app.clone();
        recorder::export(&session, &opts, &out, move |p| {
            let _ = a.emit_to("export", "export-progress", p);
        })?;
        Ok(out.to_string_lossy().into())
    })
    .await
    .map_err(e2s)?
}

// ---------- app ----------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        let _ = app.emit("hotkey-toggle", ());
                    }
                })
                .build(),
        )
        .manage(AppState::default())
        .setup(|app| {
            let _ = app
                .global_shortcut()
                .register(Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyR));

            // Probe the hardware encoder early so the first recording starts instantly.
            std::thread::spawn(|| {
                ffmpeg::encoder();
            });

            // Level meters: emit peak values ~12×/s while any capture is running.
            let handle = app.handle().clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_millis(80));
                let st = handle.state::<AppState>();
                let active = {
                    let g = st.inner.lock().unwrap();
                    g.rec.is_some() || g.mon_mic.is_some() || g.mon_sys.is_some()
                };
                if !active {
                    continue;
                }
                let mic = f32::from_bits(st.levels.mic.swap(0, Ordering::Relaxed));
                let sys = f32::from_bits(st.levels.sys.swap(0, Ordering::Relaxed));
                let _ = handle.emit("levels", (mic, sys));
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            show_camera,
            show_main,
            list_displays,
            list_audio,
            set_monitor,
            start_recording,
            recording_started,
            stop_recording,
            discard_recording,
            cam_chunk,
            cam_finished,
            current_session,
            waveform,
            export_video
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
