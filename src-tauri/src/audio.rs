//! Microphone and system-audio (WASAPI loopback) capture to WAV.
//!
//! cpal streams are not `Send`, so each capture lives on its own thread and is
//! stopped through a channel.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use crate::now_ms;

pub enum Kind {
    Mic(Option<String>),
    System,
}

struct Sink {
    writer: Option<hound::WavWriter<std::io::BufWriter<std::fs::File>>>,
    frames: u64,
    start: Option<Instant>,
    start_ms: Option<u64>,
    sample_rate: u32,
    channels: u16,
}

impl Sink {
    /// Append silence so the file stays aligned with wall-clock time. Loopback
    /// delivers nothing while no sound is playing.
    fn pad_to(&mut self, frames: u64) {
        if frames <= self.frames {
            return;
        }
        if let Some(w) = self.writer.as_mut() {
            for _ in 0..(frames - self.frames) * self.channels as u64 {
                let _ = w.write_sample(0.0f32);
            }
        }
        self.frames = frames;
    }
}

pub struct Capture {
    stop: mpsc::Sender<()>,
    handle: Option<JoinHandle<Option<u64>>>,
}

impl Capture {
    /// Start capturing. With `out == None` only the level meter is updated.
    pub fn start(kind: Kind, out: Option<PathBuf>, level: Arc<AtomicU32>) -> Result<Self, String> {
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();

        let handle = std::thread::spawn(move || {
            let fail = |e: String| {
                let _ = ready_tx.send(Err(e));
                None
            };
            let host = cpal::default_host();
            let (device, config) = match open(&host, &kind) {
                Ok(v) => v,
                Err(e) => return fail(e),
            };
            let sample_rate = config.sample_rate().0;
            let channels = config.channels();
            let writer = match &out {
                Some(p) => match hound::WavWriter::create(
                    p,
                    hound::WavSpec {
                        channels,
                        sample_rate,
                        bits_per_sample: 32,
                        sample_format: hound::SampleFormat::Float,
                    },
                ) {
                    Ok(w) => Some(w),
                    Err(e) => return fail(e.to_string()),
                },
                None => None,
            };
            let sink = Arc::new(Mutex::new(Sink {
                writer,
                frames: 0,
                start: None,
                start_ms: None,
                sample_rate,
                channels,
            }));
            let s2 = sink.clone();
            let lvl = level.clone();
            let err_fn = |e| eprintln!("audio stream error: {e}");
            let stream = match config.sample_format() {
                cpal::SampleFormat::F32 => device.build_input_stream(
                    &config.config(),
                    move |d: &[f32], _: &_| push(&s2, &lvl, d.iter().copied()),
                    err_fn,
                    None,
                ),
                cpal::SampleFormat::I16 => device.build_input_stream(
                    &config.config(),
                    move |d: &[i16], _: &_| push(&s2, &lvl, d.iter().map(|s| *s as f32 / 32768.0)),
                    err_fn,
                    None,
                ),
                cpal::SampleFormat::U16 => device.build_input_stream(
                    &config.config(),
                    move |d: &[u16], _: &_| {
                        push(&s2, &lvl, d.iter().map(|s| (*s as f32 - 32768.0) / 32768.0))
                    },
                    err_fn,
                    None,
                ),
                f => return fail(format!("unsupported sample format {f:?}")),
            };
            let stream = match stream {
                Ok(s) => s,
                Err(e) => return fail(e.to_string()),
            };
            if let Err(e) = stream.play() {
                return fail(e.to_string());
            }
            let _ = ready_tx.send(Ok(()));

            let _ = stop_rx.recv();
            drop(stream);

            let mut s = sink.lock().unwrap();
            if let Some(start) = s.start {
                let expected = (start.elapsed().as_secs_f64() * s.sample_rate as f64) as u64;
                s.pad_to(expected);
            }
            let start_ms = s.start_ms;
            if let Some(w) = s.writer.take() {
                let _ = w.finalize();
            }
            start_ms
        });

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Capture { stop: stop_tx, handle: Some(handle) }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("audio thread exited".into()),
        }
    }

    /// Stop and return the wall-clock time (ms) of the first captured sample.
    pub fn stop(mut self) -> Option<u64> {
        let _ = self.stop.send(());
        self.handle.take().and_then(|h| h.join().ok()).flatten()
    }
}

fn push(sink: &Mutex<Sink>, level: &AtomicU32, data: impl Iterator<Item = f32>) {
    let samples: Vec<f32> = data.collect();
    let mut s = sink.lock().unwrap();
    let now = Instant::now();
    let frames = (samples.len() / s.channels.max(1) as usize) as u64;
    match s.start {
        None => {
            s.start = Some(now);
            s.start_ms = Some(now_ms());
        }
        Some(start) => {
            // This buffer ends "now"; fill any gap (>100 ms) before it with silence.
            let expected = (now.duration_since(start).as_secs_f64() * s.sample_rate as f64) as u64;
            let before = expected.saturating_sub(frames);
            if before > s.frames + s.sample_rate as u64 / 10 {
                s.pad_to(before);
            }
        }
    }
    let mut peak = 0f32;
    for v in &samples {
        peak = peak.max(v.abs());
    }
    if let Some(w) = s.writer.as_mut() {
        for v in &samples {
            let _ = w.write_sample(*v);
        }
    }
    s.frames += frames;
    // Non-negative f32 bit patterns order like integers, so fetch_max works.
    level.fetch_max(peak.min(1.0).to_bits(), Ordering::Relaxed);
}

fn open(host: &cpal::Host, kind: &Kind) -> Result<(cpal::Device, cpal::SupportedStreamConfig), String> {
    match kind {
        Kind::Mic(name) => {
            let named = name.as_ref().and_then(|n| {
                host.input_devices()
                    .ok()?
                    .find(|d| d.name().map(|x| &x == n).unwrap_or(false))
            });
            let dev = named
                .or_else(|| host.default_input_device())
                .ok_or_else(|| "ไม่พบไมโครโฟน".to_string())?;
            let cfg = dev.default_input_config().map_err(|e| e.to_string())?;
            Ok((dev, cfg))
        }
        Kind::System => {
            // Building an input stream on an output device = WASAPI loopback.
            let dev = host
                .default_output_device()
                .ok_or_else(|| "ไม่พบอุปกรณ์เสียงขาออก".to_string())?;
            let cfg = dev.default_output_config().map_err(|e| e.to_string())?;
            Ok((dev, cfg))
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDevices {
    pub inputs: Vec<String>,
    pub default_input: Option<String>,
    pub output: Option<String>,
}

pub fn devices() -> AudioDevices {
    let host = cpal::default_host();
    AudioDevices {
        inputs: host
            .input_devices()
            .map(|it| it.filter_map(|d| d.name().ok()).collect())
            .unwrap_or_default(),
        default_input: host.default_input_device().and_then(|d| d.name().ok()),
        output: host.default_output_device().and_then(|d| d.name().ok()),
    }
}

/// Peak envelope of a WAV file, `bins` values in 0..1.
pub fn waveform(path: &std::path::Path, bins: usize) -> Vec<f32> {
    let Ok(mut r) = hound::WavReader::open(path) else { return vec![] };
    let ch = r.spec().channels.max(1) as usize;
    let total = r.duration() as usize;
    if total == 0 {
        return vec![0.0; bins];
    }
    let per = (total / bins).max(1) * ch;
    let mut out = vec![0f32; bins];
    for (i, s) in r.samples::<f32>().enumerate() {
        let b = (i / per).min(bins - 1);
        let v = s.unwrap_or(0.0).abs();
        if v > out[b] {
            out[b] = v;
        }
    }
    out
}
