//! Locating and invoking the FFmpeg binaries.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// Prefer a bundled copy next to the executable, then fall back to PATH.
pub fn bin(name: &str) -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for c in [
                dir.join(format!("{name}.exe")),
                dir.join("binaries").join(format!("{name}.exe")),
            ] {
                if c.exists() {
                    return c;
                }
            }
        }
    }
    PathBuf::from(name)
}

pub fn cmd(name: &str) -> Command {
    let mut c = Command::new(bin(name));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

static ENCODER: OnceLock<String> = OnceLock::new();

/// First hardware H.264 encoder that can actually open on this machine, else libx264.
///
/// Integrated-GPU encoders go first: on hybrid laptops the iGPU drives the
/// display, so AMF/QSV avoid a cross-GPU copy. Measured on an AMD+NVIDIA
/// laptop: AMF ~125 MB RAM at full 30 fps vs NVENC ~520 MB with dropped frames.
pub fn encoder() -> &'static str {
    ENCODER.get_or_init(|| {
        for e in ["h264_amf", "h264_qsv", "h264_nvenc"] {
            let ok = cmd("ffmpeg")
                .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
                .arg("color=s=256x256:d=0.2")
                .args(["-c:v", e, "-f", "null", "-"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if ok {
                return e.to_string();
            }
        }
        "libx264".to_string()
    })
}

pub fn encoder_args(enc: &str) -> Vec<String> {
    let a: &[&str] = match enc {
        "h264_nvenc" => &["-preset", "p4", "-rc", "vbr", "-cq", "23", "-b:v", "0"],
        "h264_amf" => &["-quality", "speed", "-rc", "cqp", "-qp_i", "22", "-qp_p", "24"],
        "h264_qsv" => &["-preset", "veryfast", "-global_quality", "24"],
        _ => &["-preset", "veryfast", "-crf", "23"],
    };
    a.iter().map(|s| s.to_string()).collect()
}

pub struct Probe {
    pub width: u32,
    pub height: u32,
    pub duration: f64,
}

pub fn probe(path: &Path) -> Option<Probe> {
    let out = cmd("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height:format=duration",
            "-of",
            "default=nw=1",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (mut w, mut h, mut d) = (0, 0, 0.0);
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("width=") {
            w = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("height=") {
            h = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("duration=") {
            d = v.trim().parse().unwrap_or(0.0);
        }
    }
    (w > 0 && h > 0).then_some(Probe { width: w, height: h, duration: d })
}
