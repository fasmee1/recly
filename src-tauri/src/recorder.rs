//! Screen capture (FFmpeg ddagrab), session files and final composition.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::{ffmpeg, now_ms};

// ---------- screen ----------

pub struct Screen {
    child: Child,
    start_ms: u64,
    log: Arc<Mutex<Vec<String>>>,
}

impl Screen {
    pub fn start(display: u32, fps: u32, out: &Path) -> Result<Self, String> {
        let enc = ffmpeg::encoder();
        let mut c = ffmpeg::cmd("ffmpeg");
        c.args(["-hide_banner", "-nostats", "-loglevel", "info", "-f", "lavfi", "-i"])
            .arg(format!(
                "ddagrab=output_idx={display}:framerate={fps}:draw_mouse=1,hwdownload,format=bgra"
            ))
            .args(["-c:v", enc])
            .args(ffmpeg::encoder_args(enc))
            .args(["-pix_fmt", "yuv420p", "-g"])
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
    pub cam_x: f64, // horizontal: bubble rect, normalised
    pub cam_y: f64,
    pub cam_size: f64,
    pub mic_vol: f64,
    pub sys_vol: f64,
    pub quality: u32, // short side: 720 | 1080 | 2160
    pub trim_start: f64,
    pub trim_end: f64,
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
    let dur = (t1 - t0).max(0.1);
    let off = |start: u64| (start as f64 - session.screen_start as f64) / 1000.0;

    let mut args: Vec<String> = vec!["-hide_banner".into(), "-y".into()];
    fn input(a: &mut Vec<String>, pre: &[&str], path: &str) {
        a.extend(pre.iter().map(|s| s.to_string()));
        a.push("-i".into());
        a.push(path.into());
    }
    let mut n = 0usize;
    let mut fg: Vec<String> = Vec::new();

    input(&mut args, &[], &session.screen);
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
        let a = ow as f64 / top_h as f64;
        let (cw, ch) = if sw / sh > a { (even(sh * a), sh as u32 / 2 * 2) } else { (sw as u32 / 2 * 2, even(sw / a)) };
        let cx = ((sw - cw as f64) * o.focus_x.clamp(0.0, 1.0)).round() as u32;
        let cy = ((sh - ch as f64) * o.focus_y.clamp(0.0, 1.0)).round() as u32;
        fg.push(format!(
            "[{screen_idx}:v]fps={fps},crop={cw}:{ch}:{cx}:{cy},scale={ow}:{top_h}:flags=bicubic,setsar=1[top]"
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
    fg.push(format!(
        "[v0]trim=start={t0:.3}:end={t1:.3},setpts=PTS-STARTPTS,format=yuv420p[v]"
    ));

    // ----- audio -----
    let mut alabels = Vec::new();
    for (track, vol) in [(&session.mic, o.mic_vol), (&session.system, o.sys_vol)] {
        let Some(t) = track else { continue };
        if vol <= 0.001 {
            continue;
        }
        input(&mut args, &[], &t.path);
        let ai = n;
        n += 1;
        let l = format!("a{ai}");
        fg.push(format!(
            "[{ai}:a]asetpts=PTS-STARTPTS,aresample=48000,{}volume={vol:.2}[{l}]",
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
        fg.push(format!("{mix}atrim=start={t0:.3}:end={t1:.3},asetpts=PTS-STARTPTS[a]"));
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
