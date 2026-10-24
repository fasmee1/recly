//! Locating and invoking the FFmpeg binaries.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

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

/// Filters applied after `ddagrab`, cheapest first. Converting BGRA→NV12 on the
/// GPU (scale_d3d11) avoids a full-resolution swscale pass per frame; measured
/// on an AMD iGPU at 1080p30: zero-copy ~0.04 s CPU per second of video vs
/// ~0.75 s for hwdownload + swscale.
#[derive(Clone, Copy, PartialEq)]
pub enum Chain {
    /// Frames stay on the GPU all the way into the encoder.
    ZeroCopy,
    /// GPU colour conversion, then hand system-memory NV12 to the encoder.
    GpuConvert,
    /// Plain CPU path; works wherever ddagrab does.
    Cpu,
}

impl Chain {
    pub const ALL: [Chain; 3] = [Chain::ZeroCopy, Chain::GpuConvert, Chain::Cpu];

    /// Filters after `ddagrab`. `height > 0` downscales to that height (never up),
    /// on the GPU when the chain allows it.
    pub fn filter(self, height: u32) -> String {
        let (w, h) = (
            format!("trunc(iw*min(ih\\,{height})/ih/2)*2"),
            format!("trunc(min(ih\\,{height})/2)*2"),
        );
        let d3d = if height > 0 { format!("scale_d3d11=width={w}:height={h}:format=nv12") } else { "scale_d3d11=format=nv12".into() };
        match self {
            Chain::ZeroCopy => d3d,
            Chain::GpuConvert => format!("{d3d},hwdownload,format=nv12"),
            Chain::Cpu if height > 0 => format!("hwdownload,format=bgra,scale={w}:{h}"),
            Chain::Cpu => "hwdownload,format=bgra".into(),
        }
    }
}

static CAPTURE: Mutex<Vec<(u32, Chain)>> = Mutex::new(Vec::new());

/// Cheapest capture chain that works with the chosen encoder on this display.
/// Zero-copy fails e.g. with NVENC when the iGPU drives the display.
pub fn capture_chain(display: u32) -> Chain {
    if let Some(&(_, c)) = CAPTURE.lock().unwrap().iter().find(|(d, _)| *d == display) {
        return c;
    }
    let enc = encoder();
    let chain = Chain::ALL
        .into_iter()
        // libx264 can't take GPU frames.
        .filter(|&c| enc != "libx264" || c != Chain::ZeroCopy)
        .find(|c| {
            cmd("ffmpeg")
                .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
                .arg(format!("ddagrab=output_idx={display}:framerate=10,{}", c.filter(0)))
                .args(["-frames:v", "5", "-c:v", enc])
                .args(encoder_args(enc))
                .args(["-f", "null", "-"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        })
        .unwrap_or(Chain::Cpu);
    CAPTURE.lock().unwrap().push((display, chain));
    chain
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
