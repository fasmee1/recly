//! Screen capture (FFmpeg ddagrab), session files and final composition.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::ffmpeg::{self, Chain};
use crate::now_ms;

// ---------- screen ----------

pub struct Screen {
    child: Child,
    start_ms: u64,
    log: Arc<Mutex<Vec<String>>>,
}

impl Screen {
    /// `height > 0` records downscaled to that height; 0 keeps the display's resolution.
    pub fn start(display: u32, fps: u32, height: u32, out: &Path) -> Result<Self, String> {
        let chain = ffmpeg::capture_chain(display);
        match Self::start_with(display, fps, height, out, chain) {
            Err(_) if chain != Chain::Cpu => Self::start_with(display, fps, height, out, Chain::Cpu),
            r => r,
        }
    }

    fn start_with(display: u32, fps: u32, height: u32, out: &Path, chain: Chain) -> Result<Self, String> {
        let enc = ffmpeg::encoder();
        let mut c = ffmpeg::cmd("ffmpeg");
        c.args(["-hide_banner", "-nostats", "-loglevel", "info", "-f", "lavfi", "-i"])
            .arg(format!("ddagrab=output_idx={display}:framerate={fps}:draw_mouse=1,{}", chain.filter(height)))
            .args(["-c:v", enc])
            .args(ffmpeg::encoder_args(enc));
        if chain == Chain::Cpu {
            c.args(["-pix_fmt", "yuv420p"]);
        }
        c.arg("-g")
            .arg((fps * 2).to_string())
            // Fragmented MP4 stays playable even if the app is killed mid-recording.
            .args(["-movflags", "+frag_keyframe+empty_moov+default_base_moof", "-y"])
            .arg(out)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let spawned = Instant::now();
        let spawned_ms = now_ms();
        let mut child = c.spawn().map_err(|e| format!("เปิด ffmpeg ไม่ได้: {e}"))?;
        let started: Arc<Mutex<Option<u64>>> = Arc::new(Mutex::new(None));
        let log = Arc::new(Mutex::new(Vec::new()));

        let mut stderr = child.stderr.take().unwrap();
        let (st, lg) = (started.clone(), log.clone());
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            let mut line = Vec::new();
            while let Ok(n) = stderr.read(&mut buf) {
                if n == 0 {
                    break;
                }
                for &b in &buf[..n] {
                    if b == b'\n' || b == b'\r' {
                        let s = String::from_utf8_lossy(&line).trim().to_string();
                        line.clear();
                        if s.is_empty() {
                            continue;
                        }
                        // FFmpeg prints this right as the encode loop begins.
                        if s.contains("Press [q]") {
                            st.lock().unwrap().get_or_insert(now_ms());
                        }
                        let mut l = lg.lock().unwrap();
                        l.push(s);
                        if l.len() > 40 {
                            l.remove(0);
                        }
                    } else {
                        line.push(b);
                    }
                }
            }
        });

        loop {
            if let Some(ms) = *started.lock().unwrap() {
                return Ok(Screen { child, start_ms: ms, log });
            }
            if let Ok(Some(_)) = child.try_wait() {
                std::thread::sleep(Duration::from_millis(100));
                let tail = log.lock().unwrap().iter().rev().take(4).cloned().collect::<Vec<_>>();
                return Err(format!(
                    "อัดหน้าจอไม่สำเร็จ: {}",
                    tail.into_iter().rev().collect::<Vec<_>>().join(" | ")
                ));
            }
            if spawned.elapsed() > Duration::from_secs(5) {
                return Ok(Screen { child, start_ms: spawned_ms + 300, log });
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Ask FFmpeg to finish cleanly; returns the capture start time (epoch ms).
    pub fn stop(mut self) -> u64 {
        if let Some(mut stdin) = self.child.stdin.take() {
            let _ = stdin.write_all(b"q\n");
            let _ = stdin.flush();
        }
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(10) {
            if let Ok(Some(_)) = self.child.try_wait() {
                return self.start_ms;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        eprintln!("ffmpeg did not exit, killing. log: {:?}", self.log.lock().unwrap());
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.start_ms
    }
}

// ---------- session ----------

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub path: String,
    pub start: u64,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CamTrack {
    pub path: String,
    pub start: u64,
    /// Bubble rect, normalised to the recorded display (0..1).
    pub x: f64,
    pub y: f64,
    pub size: f64,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub dir: String,
    pub screen: String,
    pub screen_start: u64,
    pub width: u32,
    pub height: u32,
    pub duration: f64,
    pub mic: Option<Track>,
    pub system: Option<Track>,
    pub cam: Option<CamTrack>,
    pub layout: String,
    pub shape: String,
}

impl Session {
    pub fn load(dir: &Path) -> Result<Self, String> {
        let s = std::fs::read_to_string(dir.join("session.json")).map_err(|e| e.to_string())?;
        serde_json::from_str(&s).map_err(|e| e.to_string())
    }
    pub fn save(&self) -> Result<(), String> {
        let s = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(Path::new(&self.dir).join("session.json"), s).map_err(|e| e.to_string())
    }
}

// ---------- export ----------

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ExportOpts {
    pub layout: String, // "horizontal" | "vertical"
    pub shape: String,  // "circle" | "square" | "rounded"
    pub include_cam: bool,
    pub ratio: f64,   // vertical: share of height for the screen
    pub focus_x: f64, // vertical: horizontal crop position 0..1
    pub focus_y: f64,
    /// vertical: 1 = screen fits the output width; >1 zooms in. Independent of `ratio`.
    #[serde(default)]
    pub zoom: f64,
    pub cam_x: f64, // horizontal: bubble rect, normalised
    pub cam_y: f64,
    pub cam_size: f64,
    pub mic_vol: f64,
    pub sys_vol: f64,
    pub quality: u32, // short side: 720 | 1080 | 2160
    pub trim_start: f64,
    pub trim_end: f64,
    /// Reduce steady background noise on the microphone track.
    #[serde(default)]
    pub denoise: bool,
    /// Ranges (screen timeline, seconds) to remove, e.g. silences.
    #[serde(default)]
    pub cut: Vec<(f64, f64)>,
}

/// Speech keeps this much lead-in / tail inside a cut so words aren't clipped.
const SILENCE_PAD: f64 = 0.15;
/// Level meter resolution for silence detection.
const WIN: f64 = 0.05;
/// Speech must stand out from the noise floor by this much (dB) to be trusted.
const MIN_SPEECH_GAP_DB: f64 = 12.0;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Silence {
    /// Level (dB RMS) below which audio counts as silent: the manual value, else the automatic one.
    pub threshold_db: f64,
    /// False when speech can't be told apart from the noise floor; nothing is cut then.
    pub speech_found: bool,
    pub ranges: Vec<(f64, f64)>,
}

/// RMS level in dB for every `WIN` seconds of an audio file.
fn rms_levels(path: &str) -> Result<Vec<f64>, String> {
    let n = (48000.0 * WIN) as u32;
    let out = ffmpeg::cmd("ffmpeg")
        .args(["-hide_banner", "-nostats", "-loglevel", "error", "-i"])
        .arg(path)
        .arg("-af")
        .arg(format!(
            "aresample=48000,asetnsamples=n={n}:p=0,astats=metadata=1:reset=1,ametadata=print:key=lavfi.astats.Overall.RMS_level:file=-"
        ))
        .args(["-f", "null", "-"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err("วัดระดับเสียงไม่สำเร็จ".into());
    }
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    Ok(text
        .lines()
        .filter_map(|l| l.split("RMS_level=").nth(1))
        .map(|v| v.trim().parse::<f64>().unwrap_or(-90.0).max(-90.0)) // "-inf" = digital silence
        .collect())
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * p) as usize]
}

/// Which `WIN`-sized windows of a track carry sound, the threshold used (dB), and
/// whether the track has a clear difference between noise and sound at all.
/// Mics differ a lot in level, so unless `manual_db` is given the threshold is
/// derived from the file: between its noise floor and its speech level.
fn active_windows(levels: &[f64], manual_db: Option<f64>) -> (Vec<bool>, f64, bool) {
    if levels.is_empty() {
        return (vec![], -90.0, false);
    }
    let mut sorted = levels.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let (noise, loud) = (percentile(&sorted, 0.10), percentile(&sorted, 0.95));
    let auto = (noise + 0.35 * (loud - noise)).clamp(-90.0, -20.0);
    if manual_db.is_none() && loud - noise < MIN_SPEECH_GAP_DB {
        return (vec![false; levels.len()], auto, false);
    }
    let thr = manual_db.unwrap_or(auto);

    // Loud windows shorter than 150 ms are clicks or key taps, not speech.
    let mut active: Vec<bool> = levels.iter().map(|&l| l >= thr).collect();
    let n = active.len();
    let mut i = 0;
    while i < n {
        if !active[i] {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < n && active[j] {
            j += 1;
        }
        if j - i < 3 {
            active[i..j].fill(false);
        }
        i = j;
    }
    (active, thr, true)
}

/// Stretches (screen timeline, seconds) where both the microphone and the
/// computer's own audio are silent, so sound playing on the PC is never cut.
/// Each range is shrunk by `SILENCE_PAD` where it borders sound, so `min_dur`
/// seconds of silence removes `min_dur - 2*pad`. `manual_db` applies to the mic;
/// the system track always uses its own automatic threshold.
pub fn detect_silence(session: &Session, manual_db: Option<f64>, min_dur: f64) -> Result<Silence, String> {
    if session.mic.is_none() && session.system.is_none() {
        return Err("ไม่มีเสียงให้ตรวจช่วงเงียบ".into());
    }
    let n = (session.duration / WIN).ceil() as usize;
    let mut busy = vec![false; n];
    let mut found = false;
    let mut threshold_db = None;
    for (track, manual) in [(&session.mic, manual_db), (&session.system, None)] {
        let Some(t) = track else { continue };
        let (active, thr, clear) = active_windows(&rms_levels(&t.path)?, manual);
        threshold_db.get_or_insert(thr); // the slider follows the mic, or the system track without one
        found |= clear;
        let shift = ((t.start as f64 - session.screen_start as f64) / 1000.0 / WIN).round() as i64;
        for (i, _) in active.iter().enumerate().filter(|(_, a)| **a) {
            let k = i as i64 + shift;
            if k >= 0 && (k as usize) < n {
                busy[k as usize] = true;
            }
        }
    }
    let threshold_db = threshold_db.unwrap_or(-50.0);
    if !found {
        return Ok(Silence { threshold_db, speech_found: false, ranges: vec![] });
    }

    let min_win = (min_dur / WIN).ceil() as usize;
    let mut ranges = Vec::new();
    let mut i = 0;
    while i < n {
        if busy[i] {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < n && !busy[j] {
            j += 1;
        }
        if j - i >= min_win {
            let a = if i == 0 { 0.0 } else { i as f64 * WIN + SILENCE_PAD };
            let b = if j == n { session.duration } else { j as f64 * WIN - SILENCE_PAD };
            if b - a >= 0.1 {
                ranges.push((a.min(session.duration), b.min(session.duration)));
            }
        }
        i = j;
    }
    Ok(Silence { threshold_db, speech_found: true, ranges })
}

/// Cut ranges clipped to [t0, t1], shifted to clip time, sorted and merged.
fn clip_cuts(cuts: &[(f64, f64)], t0: f64, t1: f64) -> Vec<(f64, f64)> {
    let mut v: Vec<(f64, f64)> = cuts
        .iter()
        .map(|&(a, b)| (a.max(t0) - t0, b.min(t1) - t0))
        .filter(|&(a, b)| b - a > 0.01)
        .collect();
    v.sort_by(|x, y| x.0.total_cmp(&y.0));
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for (a, b) in v {
        match merged.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    merged
}

fn even(v: f64) -> u32 {
    ((v / 2.0).round() as u32 * 2).max(2)
}

/// Time-shift filter for a stream that started `off` seconds after the screen.
fn shift_v(off: f64) -> String {
    if off > 0.0 {
        format!("tpad=start_duration={off:.3}:start_mode=clone,")
    } else if off < 0.0 {
        format!("trim=start={:.3},setpts=PTS-STARTPTS,", -off)
    } else {
        String::new()
    }
}

fn shift_a(off: f64) -> String {
    if off > 0.0 {
        format!("adelay=delays={}:all=1,", (off * 1000.0).round() as u64)
    } else if off < 0.0 {
        format!("atrim=start={:.3},asetpts=PTS-STARTPTS,", -off)
    } else {
        String::new()
    }
}

fn mask_expr(shape: &str, s: u32) -> String {
    let r = match shape {
        "circle" => return "255*clip(W/2-hypot(X+0.5-W/2\\,Y+0.5-H/2)+0.5\\,0\\,1)".into(),
        "square" => s as f64 * 0.06,
        _ => s as f64 * 0.22,
    };
    format!(
        "255*clip({r:.1}-hypot(max(abs(X+0.5-W/2)-(W/2-{r:.1})\\,0)\\,max(abs(Y+0.5-H/2)-(H/2-{r:.1})\\,0))+0.5\\,0\\,1)"
    )
}

fn make_mask(dir: &Path, shape: &str, s: u32) -> Result<PathBuf, String> {
    let out = dir.join(format!("mask-{shape}-{s}.png"));
    let st = ffmpeg::cmd("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("color=c=black:s={s}x{s}:d=1"))
        .arg("-vf")
        .arg(format!("format=gray,geq=lum={}", mask_expr(shape, s)))
        .args(["-frames:v", "1"])
        .arg(&out)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| e.to_string())?;
    if !st.success() {
        return Err("สร้าง mask กล้องไม่สำเร็จ".into());
    }
    Ok(out)
}

pub fn export(
    session: &Session,
    o: &ExportOpts,
    out: &Path,
    mut progress: impl FnMut(f64),
) -> Result<(), String> {
    let dir = Path::new(&session.dir);
    let (sw, sh) = (session.width as f64, session.height as f64);
    let fps = 30;
    let t0 = o.trim_start.max(0.0);
    let t1 = if o.trim_end > t0 { o.trim_end.min(session.duration.max(t0 + 0.1)) } else { session.duration };
    let clip_len = (t1 - t0).max(0.1);
    let cuts = clip_cuts(&o.cut, t0, t1);
    let removed: f64 = cuts.iter().map(|(a, b)| b - a).sum();
    // Length of the finished video, used for -t and the progress bar.
    let dur = (clip_len - removed).max(0.1);
    // Drops frames/samples inside the cut ranges (times are clip-relative at that point).
    let keep = (!cuts.is_empty()).then(|| {
        let gaps: Vec<String> = cuts.iter().map(|(a, b)| format!("between(t,{a:.3},{b:.3})")).collect();
        format!("not({})", gaps.join("+"))
    });
    let off =|start: u64| (start as f64 - session.screen_start as f64) / 1000.0;

    let mut args: Vec<String> = vec!["-hide_banner".into(), "-y".into()];
    fn input(a: &mut Vec<String>, pre: &[&str], path: &str) {
        a.extend(pre.iter().map(|s| s.to_string()));
        a.push("-i".into());
        a.push(path.into());
    }
    let mut n = 0usize;
    let mut fg: Vec<String> = Vec::new();

    // GPU decoding roughly halves export CPU; FFmpeg falls back to software if unavailable.
    input(&mut args, &["-hwaccel", "d3d11va"], &session.screen);
    let screen_idx = n;
    n += 1;

    let cam = session.cam.as_ref().filter(|_| o.include_cam);
    let vertical = o.layout == "vertical";

    // ----- video -----
    let (ow, oh);
    if vertical {
        ow = even(o.quality as f64);
        oh = even(o.quality as f64 * 16.0 / 9.0);
        let top_h = if cam.is_some() { even(oh as f64 * o.ratio.clamp(0.3, 0.85)) } else { oh };
        // Source pixels → output pixels; zoom 1 = full screen width.
        let k = ow as f64 * o.zoom.clamp(1.0, 4.0) / sw;
        let (cw, ch) = (even(ow as f64 / k).min(sw as u32 / 2 * 2), even((top_h as f64 / k).min(sh)));
        let cx = ((sw - cw as f64) * o.focus_x.clamp(0.0, 1.0)).round() as u32;
        let cy = ((sh - ch as f64) * o.focus_y.clamp(0.0, 1.0)).round() as u32;
        // Letterbox if the zoomed screen is shorter than the top area.
        let vh = even(ch as f64 * k).min(top_h);
        fg.push(format!(
            "[{screen_idx}:v]fps={fps},crop={cw}:{ch}:{cx}:{cy},scale={ow}:{vh}:flags=bicubic,pad={ow}:{top_h}:0:{}:black,setsar=1[top]",
            (top_h - vh) / 2 / 2 * 2
        ));
        if let Some(c) = cam {
            input(&mut args, &[], &c.path);
            let ci = n;
            n += 1;
            let bot_h = oh - top_h;
            let ba = ow as f64 / bot_h as f64;
            fg.push(format!(
                "[{ci}:v]setpts=PTS-STARTPTS,{}fps={fps},crop='if(gt(iw/ih,{ba:.5}),ih*{ba:.5},iw)':'if(gt(iw/ih,{ba:.5}),ih,iw/{ba:.5})',scale={ow}:{bot_h}:flags=bicubic,setsar=1[bot]",
                shift_v(off(c.start))
            ));
            fg.push("[top][bot]vstack=inputs=2[v0]".into());
        } else {
            fg.push("[top]null[v0]".into());
        }
    } else {
        let target_h = (o.quality as f64).min(sh);
        oh = even(target_h);
        ow = even(sw * target_h / sh);
        fg.push(format!(
            "[{screen_idx}:v]fps={fps},scale={ow}:{oh}:flags=bicubic,setsar=1[bg]"
        ));
        if let Some(c) = cam {
            let s = even(ow as f64 * o.cam_size.clamp(0.05, 0.6));
            let x = (ow as f64 * o.cam_x).round().clamp(0.0, (ow - s) as f64) as u32;
            let y = (oh as f64 * o.cam_y).round().clamp(0.0, (oh - s) as f64) as u32;
            let mask = make_mask(dir, &o.shape, s)?;
            input(&mut args, &[], &c.path);
            let ci = n;
            n += 1;
            input(&mut args, &["-loop", "1", "-framerate", "30"], &mask.to_string_lossy());
            let mi = n;
            n += 1;
            fg.push(format!(
                "[{ci}:v]setpts=PTS-STARTPTS,{}fps={fps},crop='min(iw,ih)':'min(iw,ih)',scale={s}:{s}:flags=bicubic,format=rgba[cam]",
                shift_v(off(c.start))
            ));
            fg.push(format!("[{mi}:v]format=gray[mask]"));
            fg.push("[cam][mask]alphamerge=shortest=1[camm]".into());
            fg.push(format!("[bg][camm]overlay={x}:{y}:eof_action=repeat[v0]"));
        } else {
            fg.push("[bg]null[v0]".into());
        }
    }
    let cut_v = keep.as_ref().map(|k| format!(",select='{k}',setpts=N/FRAME_RATE/TB")).unwrap_or_default();
    fg.push(format!(
        "[v0]trim=start={t0:.3}:end={t1:.3},setpts=PTS-STARTPTS{cut_v},format=yuv420p[v]"
    ));

    // ----- audio -----
    let mut alabels = Vec::new();
    for (track, vol, is_mic) in [(&session.mic, o.mic_vol, true), (&session.system, o.sys_vol, false)] {
        let Some(t) = track else { continue };
        if vol <= 0.001 {
            continue;
        }
        input(&mut args, &[], &t.path);
        let ai = n;
        n += 1;
        let l = format!("a{ai}");
        // Noise reduction is for the microphone only; system audio is left as recorded.
        let clean = if is_mic && o.denoise { "highpass=f=80,afftdn=nr=20:nf=-75:tn=1," } else { "" };
        fg.push(format!(
            "[{ai}:a]asetpts=PTS-STARTPTS,aresample=48000,{clean}{}volume={vol:.2}[{l}]",
            shift_a(off(t.start))
        ));
        alabels.push(l);
    }
    let has_audio = !alabels.is_empty();
    if has_audio {
        let mix = if alabels.len() > 1 {
            format!(
                "{}amix=inputs={}:normalize=0:duration=longest,",
                alabels.iter().map(|l| format!("[{l}]")).collect::<String>(),
                alabels.len()
            )
        } else {
            format!("[{}]anull,", alabels[0])
        };
        let cut_a = keep.as_ref().map(|k| format!(",aselect='{k}',asetpts=N/SR/TB")).unwrap_or_default();
        fg.push(format!("{mix}atrim=start={t0:.3}:end={t1:.3},asetpts=PTS-STARTPTS{cut_a}[a]"));
    }

    let enc = ffmpeg::encoder();
    args.push("-filter_complex".into());
    args.push(fg.join(";"));
    args.extend(["-map".into(), "[v]".into()]);
    if has_audio {
        args.extend(["-map".into(), "[a]".into(), "-c:a".into(), "aac".into(), "-b:a".into(), "192k".into()]);
    }
    args.extend(["-c:v".into(), enc.into()]);
    args.extend(ffmpeg::encoder_args(enc));
    args.extend([
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-r".into(),
        fps.to_string(),
        "-t".into(),
        format!("{dur:.3}"),
        "-movflags".into(),
        "+faststart".into(),
        "-progress".into(),
        "pipe:1".into(),
        "-nostats".into(),
        "-loglevel".into(),
        "error".into(),
    ]);
    args.push(out.to_string_lossy().into());

    let mut child = ffmpeg::cmd("ffmpeg")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stderr = child.stderr.take().unwrap();
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    for line in BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok) {
        if let Some(v) = line.strip_prefix("out_time_us=") {
            if let Ok(us) = v.trim().parse::<f64>() {
                progress((us / 1e6 / dur).clamp(0.0, 1.0));
            }
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let err = err_thread.join().unwrap_or_default();
    if !status.success() {
        return Err(format!("ส่งออกไม่สำเร็จ: {}", err.lines().rev().take(3).collect::<Vec<_>>().join(" | ")));
    }
    progress(1.0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A quiet laptop mic: noise floor around -78 dB, speech only around -55 dB.
    fn quiet_mic(name: &str, speech: &str) -> Session {
        let path = std::env::temp_dir().join(format!("recly-test-{name}.wav"));
        let st = ffmpeg::cmd("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "lavfi", "-i", "anoisesrc=d=10:a=0.0002:c=white:r=48000"])
            .args(["-f", "lavfi", "-i", "sine=f=300:d=10:r=48000"])
            .arg("-filter_complex")
            .arg(format!(
                "[1]volume='if({speech},0.02,0)':eval=frame[s];[0][s]amix=inputs=2:normalize=0,aformat=sample_fmts=flt"
            ))
            .arg(&path)
            .status()
            .unwrap();
        assert!(st.success());
        Session {
            dir: String::new(),
            screen: String::new(),
            screen_start: 1000,
            width: 1920,
            height: 1080,
            duration: 10.0,
            mic: Some(Track { path: path.to_string_lossy().into(), start: 1000 }),
            system: None,
            cam: None,
            layout: "horizontal".into(),
            shape: "circle".into(),
        }
    }

    #[test]
    fn quiet_mic_keeps_speech_and_cuts_gaps() {
        let s = quiet_mic("speech", "between(t,2,4)+between(t,6,8)");
        let r = detect_silence(&s, None, 0.6).unwrap();
        assert!(r.speech_found);
        assert!(r.threshold_db < -60.0 && r.threshold_db > -78.0, "threshold {}", r.threshold_db);
        assert_eq!(r.ranges.len(), 3, "{:?}", r.ranges);
        for (got, want) in r.ranges.iter().zip([(0.0, 1.85), (4.15, 5.85), (8.15, 10.0)]) {
            assert!((got.0 - want.0).abs() < 0.15 && (got.1 - want.1).abs() < 0.15, "{got:?} vs {want:?}");
        }
    }

    #[test]
    fn sound_from_the_computer_is_never_cut() {
        let mut s = quiet_mic("withsys", "between(t,2,4)");
        let path = std::env::temp_dir().join("recly-test-system.wav");
        let st = ffmpeg::cmd("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "sine=f=440:d=10:r=48000"])
            .arg("-af")
            .arg("volume='if(between(t,4,6),1,0)':eval=frame,aformat=sample_fmts=flt")
            .arg(&path)
            .status()
            .unwrap();
        assert!(st.success());
        s.system = Some(Track { path: path.to_string_lossy().into(), start: 1000 });
        // Mic speaks 2-4 s, the PC plays sound 4-6 s: only 0-2 s and 6-10 s are silent in both.
        let r = detect_silence(&s, None, 0.6).unwrap();
        assert_eq!(r.ranges.len(), 2, "{:?}", r.ranges);
        for (got, want) in r.ranges.iter().zip([(0.0, 1.85), (6.15, 10.0)]) {
            assert!((got.0 - want.0).abs() < 0.15 && (got.1 - want.1).abs() < 0.15, "{got:?} vs {want:?}");
        }
    }

    #[test]
    fn no_speech_means_no_cuts() {
        let s = quiet_mic("nospeech", "0");
        let r = detect_silence(&s, None, 0.6).unwrap();
        assert!(!r.speech_found);
        assert!(r.ranges.is_empty());
    }

    #[test]
    fn cuts_are_clipped_to_trim_and_merged() {
        let c = clip_cuts(&[(1.0, 3.0), (2.5, 4.0), (9.0, 12.0), (0.0, 0.5)], 1.0, 10.0);
        assert_eq!(c, vec![(0.0, 3.0), (8.0, 9.0)]);
    }
}
