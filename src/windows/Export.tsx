import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { ChevronLeft, Download, FolderOpen, Mic, Pause, Play, Volume2, X } from "lucide-react";
import { Layout, SHAPES, Shape, fmtTime, shapeClip, shapeRadius } from "../lib/shared";

interface Track { path: string; start: number }
interface CamTrack extends Track { x: number; y: number; size: number }
interface Session {
  dir: string; screen: string; screenStart: number; width: number; height: number; duration: number;
  mic: Track | null; system: Track | null; cam: CamTrack | null; layout: Layout; shape: Shape;
}

const win = getCurrentWindow();
const QUALITIES = [[720, "720p"], [1080, "1080p"], [2160, "4K"]] as const;
const MBPS: Record<number, number> = { 720: 5, 1080: 8, 2160: 24 };
const clamp = (v: number, a: number, b: number) => Math.min(b, Math.max(a, v));

function Slider({ value, min, max, step = 0.01, onChange }: {
  value: number; min: number; max: number; step?: number; onChange: (v: number) => void;
}) {
  const p = ((value - min) / (max - min)) * 100;
  return (
    <input
      type="range" className="slider" min={min} max={max} step={step} value={value}
      style={{ ["--p" as string]: `${p}%` }}
      onChange={(e) => onChange(+e.target.value)}
    />
  );
}

/** Track a pointer drag; calls onMove with the pixel delta since pointerdown. */
function startDrag(e: React.PointerEvent, onMove: (dx: number, dy: number) => void) {
  e.preventDefault();
  e.stopPropagation();
  const x0 = e.clientX, y0 = e.clientY;
  const move = (m: PointerEvent) => onMove(m.clientX - x0, m.clientY - y0);
  const up = () => {
    window.removeEventListener("pointermove", move);
    window.removeEventListener("pointerup", up);
  };
  window.addEventListener("pointermove", move);
  window.addEventListener("pointerup", up);
}

export default function Export() {
  const [session, setSession] = useState<Session | null>(null);
  const [loadError, setLoadError] = useState("");
  const [wave, setWave] = useState<number[]>([]);

  const [layout, setLayout] = useState<Layout>("horizontal");
  const [shape, setShape] = useState<Shape>("circle");
  const [includeCam, setIncludeCam] = useState(true);
  const [ratio, setRatio] = useState(0.6);
  const [focus, setFocus] = useState({ x: 0.5, y: 0.5 });
  const [cam, setCam] = useState({ x: 0.03, y: 0.7, size: 0.16 });
  const [micVol, setMicVol] = useState(1);
  const [sysVol, setSysVol] = useState(0.8);
  const [quality, setQuality] = useState(1080);
  const [trim, setTrim] = useState<[number, number]>([0, 0]);

  const [t, setT] = useState(0);
  const [playing, setPlaying] = useState(false);
  const [progress, setProgress] = useState<number | null>(null);
  const [result, setResult] = useState("");
  const [exportError, setExportError] = useState("");

  const screenV = useRef<HTMLVideoElement>(null);
  const camV = useRef<HTMLVideoElement>(null);
  const micA = useRef<HTMLAudioElement>(null);
  const sysA = useRef<HTMLAudioElement>(null);
  const canvasRef = useRef<HTMLDivElement>(null);
  const trackRef = useRef<HTMLDivElement>(null);
  const [box, setBox] = useState({ w: 700, h: 480 });

  const load = useCallback(async () => {
    try {
      const s = await invoke<Session>("current_session");
      setSession(s);
      setLayout(s.layout);
      setShape(s.shape);
      setIncludeCam(!!s.cam);
      if (s.cam) setCam({ x: s.cam.x, y: s.cam.y, size: s.cam.size });
      setTrim([0, s.duration]);
      setT(0);
      setResult("");
      setProgress(null);
      setExportError("");
      invoke<number[]>("waveform", { dir: s.dir }).then(setWave);
    } catch (e) {
      setLoadError(String(e));
    }
  }, []);

  useEffect(() => {
    load();
    const un = [
      listen("reload-session", load),
      listen<number>("export-progress", (e) => setProgress(e.payload)),
    ];
    return () => un.forEach((p) => p.then((f) => f()));
  }, [load]);

  useLayoutEffect(() => {
    const el = canvasRef.current;
    if (!el) return;
    const ro = new ResizeObserver(([e]) => setBox({ w: e.contentRect.width, h: e.contentRect.height }));
    ro.observe(el);
    return () => ro.disconnect();
  }, [session]);

  // ----- playback sync: the screen video is the clock, the rest follow -----
  const offset = (tr: Track | null) => (session && tr ? (tr.start - session.screenStart) / 1000 : 0);
  const followers = () =>
    [
      [camV.current, offset(session?.cam ?? null)],
      [micA.current, offset(session?.mic ?? null)],
      [sysA.current, offset(session?.system ?? null)],
    ].filter(([el]) => el) as [HTMLMediaElement, number][];

  const sync = (force = false) => {
    const sv = screenV.current;
    if (!sv) return;
    for (const [el, off] of followers()) {
      const target = sv.currentTime - off;
      if (target < 0) {
        if (!el.paused) el.pause();
        continue;
      }
      if (force || Math.abs(el.currentTime - target) > 0.15) el.currentTime = target;
      if (!sv.paused && el.paused) el.play().catch(() => {});
      if (sv.paused && !el.paused) el.pause();
    }
  };

  useEffect(() => {
    if (!playing) return;
    let raf = 0;
    const tick = () => {
      const sv = screenV.current;
      if (sv) {
        setT(sv.currentTime);
        if (sv.currentTime >= trim[1]) {
          sv.pause();
          setPlaying(false);
        }
        sync();
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  });

  useEffect(() => {
    if (micA.current) micA.current.volume = clamp(micVol, 0, 1);
    if (sysA.current) sysA.current.volume = clamp(sysVol, 0, 1);
  }, [micVol, sysVol, session]);

  // Camera element remounts when the layout changes; re-align it.
  useEffect(() => {
    const id = setTimeout(() => sync(true), 50);
    return () => clearTimeout(id);
  }, [layout, includeCam]);

  const togglePlay = async () => {
    const sv = screenV.current;
    if (!sv) return;
    if (playing) {
      sv.pause();
      setPlaying(false);
      sync();
      return;
    }
    if (sv.currentTime < trim[0] || sv.currentTime >= trim[1] - 0.05) sv.currentTime = trim[0];
    await sv.play();
    setPlaying(true);
    sync(true);
  };

  const seek = (time: number) => {
    const sv = screenV.current;
    if (!sv) return;
    sv.currentTime = clamp(time, 0, session?.duration ?? 0);
    setT(sv.currentTime);
    sync(true);
  };

  // ----- timeline -----
  const dur = session?.duration || 1;
  const trackDrag = (which: 0 | 1) => (e: React.PointerEvent) => {
    const rect = trackRef.current!.getBoundingClientRect();
    const start = trim[which];
    startDrag(e, (dx) => {
      const v = clamp(start + (dx / rect.width) * dur, 0, dur);
      setTrim((tr) => (which === 0 ? [Math.min(v, tr[1] - 0.5), tr[1]] : [tr[0], Math.max(v, tr[0] + 0.5)]));
    });
  };

  // ----- preview geometry -----
  const aspect = layout === "vertical" ? 9 / 16 : session ? session.width / session.height : 16 / 9;
  let fw = box.w, fh = box.w / aspect;
  if (fh > box.h) { fh = box.h; fw = box.h * aspect; }
  const hasCam = !!session?.cam && includeCam;
  const topH = hasCam ? fh * ratio : fh;

  const camPx = cam.size * fw;
  const dragCam = (e: React.PointerEvent) => {
    const c0 = cam;
    startDrag(e, (dx, dy) =>
      setCam((c) => ({
        ...c,
        x: clamp(c0.x + dx / fw, 0, 1 - c.size),
        y: clamp(c0.y + dy / fh, 0, 1 - (c.size * fw) / fh),
      })),
    );
  };
  const dragFocus = (e: React.PointerEvent) => {
    const f0 = focus;
    startDrag(e, (dx, dy) =>
      setFocus({ x: clamp(f0.x - dx / fw, 0, 1), y: clamp(f0.y - dy / topH, 0, 1) }),
    );
  };
  const dragSplit = (e: React.PointerEvent) => {
    const r0 = ratio;
    startDrag(e, (_, dy) => setRatio(clamp(r0 + dy / fh, 0.3, 0.85)));
  };

  // ----- export -----
  const clipDur = Math.max(0, trim[1] - trim[0]);
  const estMB = Math.max(1, Math.round((clipDur * (MBPS[quality] + 0.2)) / 8));

  const doExport = async () => {
    if (!session) return;
    screenV.current?.pause();
    setPlaying(false);
    setExportError("");
    setResult("");
    setProgress(0);
    try {
      const out = await invoke<string>("export_video", {
        dir: session.dir,
        opts: {
          layout, shape, includeCam: hasCam, ratio, focusX: focus.x, focusY: focus.y,
          camX: cam.x, camY: cam.y, camSize: cam.size,
          micVol, sysVol, quality, trimStart: trim[0], trimEnd: trim[1],
        },
      });
      setResult(out);
    } catch (e) {
      setExportError(String(e));
    } finally {
      setProgress(null);
    }
  };

  const close = async () => {
    screenV.current?.pause();
    await invoke("show_main");
    await win.close();
  };

  if (!session) {
    return (
      <div className="window" style={{ display: "grid", placeItems: "center" }} data-tauri-drag-region>
        <div className="hint">{loadError || "กำลังโหลด…"}</div>
      </div>
    );
  }

  const src = (p: string) => convertFileSrc(p);
  const camVideo = session.cam && (
    <video ref={camV} src={src(session.cam.path)} muted preload="auto" className="cover" />
  );

  return (
    <div className="window export">
      <div className="stage">
        <div className="stage-top" data-tauri-drag-region>
          <button className="icon-btn" onClick={close} title="กลับ"><ChevronLeft size={18} /></button>
          <span className="title" style={{ pointerEvents: "none" }}>
            บันทึก {session.dir.split(/[\\/]/).pop()?.replace(/^(\d{4})(\d{2})(\d{2})-(\d{2})(\d{2})(\d{2})$/, "$1-$2-$3 $4:$5")}
          </span>
          <span className="spacer" />
          <span className="sub" style={{ pointerEvents: "none" }}>
            {fmtTime(clipDur)} · {layout === "vertical" ? `${quality} × ${Math.round((quality * 16) / 9)}` : `${Math.round((Math.min(quality, session.height) * session.width) / session.height)} × ${Math.min(quality, session.height)}`}
          </span>
        </div>

        <div className="canvas" ref={canvasRef}>
          <div className="frame" style={{ width: fw, height: fh }}>
            {layout === "horizontal" ? (
              <>
                <video ref={screenV} src={src(session.screen)} muted preload="auto" />
                {hasCam && (
                  <div
                    className="cam-overlay"
                    style={{
                      left: cam.x * fw, top: cam.y * fh, width: camPx, height: camPx,
                      borderRadius: shapeRadius(shape, camPx), clipPath: shapeClip(shape, camPx),
                    }}
                    onPointerDown={dragCam}
                  >
                    {camVideo}
                  </div>
                )}
              </>
            ) : (
              <>
                <div
                  className="split-top" style={{ height: topH }}
                  onPointerDown={dragFocus}
                >
                  <video
                    ref={screenV} src={src(session.screen)} muted preload="auto" className="cover"
                    style={{ objectPosition: `${focus.x * 100}% ${focus.y * 100}%` }}
                  />
                </div>
                {hasCam && (
                  <>
                    <div className="split-bottom" style={{ height: fh - topH }}>{camVideo}</div>
                    <div
                      className="split-handle" style={{ top: topH }}
                      onPointerDown={dragSplit}
                    >
                      <span />
                    </div>
                  </>
                )}
                <div className="chip" style={{ top: 10, bottom: "auto" }}>ลากภาพจอเพื่อเลือกส่วนที่จะแสดง</div>
              </>
            )}
          </div>
        </div>
        {session.mic && <audio ref={micA} src={src(session.mic.path)} preload="auto" />}
        {session.system && <audio ref={sysA} src={src(session.system.path)} preload="auto" />}

        <div className="timeline">
          <button className="play" onClick={togglePlay}>
            {playing ? <Pause size={14} fill="currentColor" /> : <Play size={14} fill="currentColor" style={{ marginLeft: 2 }} />}
          </button>
          <span className="time">{fmtTime(t)}</span>
          <div
            className="track" ref={trackRef}
            onPointerDown={(e) => {
              const r = trackRef.current!.getBoundingClientRect();
              seek(((e.clientX - r.left) / r.width) * dur);
            }}
          >
            <div className="wave">
              {(wave.length ? wave : Array(160).fill(0)).map((v, i, a) => {
                const pos = (i / a.length) * dur;
                return <i key={i} className={pos >= trim[0] && pos <= trim[1] ? "in" : ""} style={{ height: `${Math.max(8, Math.sqrt(v) * 100)}%` }} />;
              })}
            </div>
            <div className="trim-box" style={{ left: `${(trim[0] / dur) * 100}%`, right: `${100 - (trim[1] / dur) * 100}%` }} />
            <div className="trim-handle" style={{ left: `calc(${(trim[0] / dur) * 100}% - 6px)` }} onPointerDown={trackDrag(0)} />
            <div className="trim-handle" style={{ left: `calc(${(trim[1] / dur) * 100}% - 6px)` }} onPointerDown={trackDrag(1)} />
            <div className="playhead" style={{ left: `${(t / dur) * 100}%` }} />
          </div>
          <span className="time" style={{ color: "var(--muted)" }}>{fmtTime(dur)}</span>
        </div>
      </div>

      <div className="panel">
        <div className="row" data-tauri-drag-region>
          <span style={{ fontSize: 16, fontWeight: 600, pointerEvents: "none" }}>ส่งออกวิดีโอ</span>
          <button className="icon-btn" onClick={close} title="ปิด"><X size={16} /></button>
        </div>

        <div className="section">
          <span className="label">เลย์เอาต์</span>
          <div className="seg">
            <button className={layout === "horizontal" ? "active" : ""} onClick={() => setLayout("horizontal")}>แนวนอน 16:9</button>
            <button className={layout === "vertical" ? "active" : ""} onClick={() => setLayout("vertical")}>แนวตั้ง 9:16</button>
          </div>
        </div>

        {session.cam && (
          <div className="section">
            <div className="section-head">
              <span className="label">กล้อง</span>
              <button className={`toggle ${includeCam ? "on" : ""}`} onClick={() => setIncludeCam(!includeCam)} />
            </div>
            {includeCam && layout === "horizontal" && (
              <>
                <div className="seg">
                  {SHAPES.map(({ id, label }) => (
                    <button key={id} className={shape === id ? "active" : ""} onClick={() => setShape(id)}>{label}</button>
                  ))}
                </div>
                <div className="row"><span>ขนาด</span><span className="sub">{Math.round(cam.size * 100)}%</span></div>
                <Slider value={cam.size} min={0.08} max={0.35} onChange={(v) =>
                  setCam((c) => ({ size: v, x: clamp(c.x, 0, 1 - v), y: clamp(c.y, 0, 1 - (v * fw) / fh) }))} />
              </>
            )}
            {includeCam && layout === "vertical" && (
              <>
                <div className="row"><span>สัดส่วน จอ : กล้อง</span><span className="sub">{Math.round(ratio * 100)} : {100 - Math.round(ratio * 100)}</span></div>
                <Slider value={ratio} min={0.3} max={0.85} onChange={setRatio} />
              </>
            )}
          </div>
        )}

        {(session.mic || session.system) && (
          <div className="section">
            <span className="label">ระดับเสียง</span>
            {session.mic && (
              <div className="vol">
                <Mic size={16} color="var(--muted)" />
                <div className="grow">
                  <div className="row"><span>ไมโครโฟน</span><span className="sub">{Math.round(micVol * 100)}%</span></div>
                  <Slider value={micVol} min={0} max={2} onChange={setMicVol} />
                </div>
              </div>
            )}
            {session.system && (
              <div className="vol">
                <Volume2 size={16} color="var(--muted)" />
                <div className="grow">
                  <div className="row"><span>เสียงในเครื่อง</span><span className="sub">{Math.round(sysVol * 100)}%</span></div>
                  <Slider value={sysVol} min={0} max={2} onChange={setSysVol} />
                </div>
              </div>
            )}
          </div>
        )}

        <div className="section">
          <span className="label">คุณภาพ</span>
          <div className="seg">
            {QUALITIES.map(([q, label]) => (
              <button key={q} className={quality === q ? "active" : ""} onClick={() => setQuality(q)}>{label}</button>
            ))}
          </div>
        </div>

        <div className="spacer" />

        {progress !== null ? (
          <div className="section">
            <div className="row"><span>กำลังส่งออก…</span><span className="sub">{Math.round(progress * 100)}%</span></div>
            <div className="progress"><div style={{ width: `${progress * 100}%` }} /></div>
          </div>
        ) : result ? (
          <div className="section">
            <div className="info"><FolderOpen size={16} /><span className="grow sub">{result.split(/[\\/]/).pop()}</span></div>
            <div className="row" style={{ gap: 8 }}>
              <button className="btn-ghost" style={{ flex: 1 }} onClick={() => revealItemInDir(result)}>เปิดโฟลเดอร์</button>
              <button className="btn-ghost" style={{ flex: 1 }} onClick={close}>อัดใหม่</button>
            </div>
          </div>
        ) : (
          <div className="info"><FolderOpen size={16} />MP4 · H.264 · ประมาณ {estMB} MB</div>
        )}
        {exportError && <div className="error" style={{ textAlign: "left" }}>{exportError}</div>}
        <button className="btn-primary" onClick={doExport} disabled={progress !== null} style={{ fontSize: 15 }}>
          <Download size={18} />ส่งออก MP4
        </button>
      </div>
    </div>
  );
}
