import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { emitTo, listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { ChevronDown, Mic, Minus, Monitor, Video, Volume2, X } from "lucide-react";
import {
  Layout, SHAPES, Settings, Shape, loadSettings, meterSegments, saveSettings, shapeRadius,
} from "../lib/shared";

interface DisplayInfo { index: number; name: string; width: number; height: number; primary: boolean }
interface AudioDevices { inputs: string[]; defaultInput: string | null; output: string | null }

const win = getCurrentWindow();
const RESOLUTIONS = [[0, "ต้นฉบับ"], [1440, "1440p"], [1080, "1080p"], [720, "720p"]] as const;

function Toggle({ on, onChange }: { on: boolean; onChange: (v: boolean) => void }) {
  return <button className={`toggle ${on ? "on" : ""}`} onClick={() => onChange(!on)} aria-pressed={on} />;
}

function Meter({ peak, active }: { peak: number; active: boolean }) {
  const lit = active ? meterSegments(peak, 20) : 0;
  return (
    <div className="meter">
      {Array.from({ length: 20 }, (_, i) => <i key={i} className={i < lit ? "lit" : ""} />)}
    </div>
  );
}

function LayoutThumb({ layout }: { layout: Layout }) {
  const line = (w: number, x: number, y: number, h = 5) => (
    <div style={{ position: "absolute", left: x, top: y, width: w, height: h, borderRadius: 3, background: "rgba(139,139,148,.35)" }} />
  );
  if (layout === "horizontal") {
    return (
      <div className="layout-thumb">
        <div style={{ position: "absolute", left: 2, top: 4, width: 136, height: 76, borderRadius: 6, background: "var(--surface-2)" }} />
        {line(70, 46, 18)}{line(52, 46, 29)}{line(60, 46, 40)}
        <div style={{ position: "absolute", left: 10, top: 48, width: 26, height: 26, borderRadius: "50%", background: "var(--text)" }} />
      </div>
    );
  }
  return (
    <div className="layout-thumb">
      <div style={{ position: "absolute", left: 46, top: 0, width: 48, height: 84, borderRadius: 6, overflow: "hidden", background: "var(--surface-2)" }}>
        {line(32, 8, 10, 4)}{line(24, 8, 19, 4)}{line(28, 8, 28, 4)}
        <div style={{ position: "absolute", left: 0, top: 50, width: 48, height: 34, background: "var(--text)" }} />
      </div>
    </div>
  );
}

export default function Setup() {
  const [s, setS] = useState<Settings>(loadSettings);
  const [displays, setDisplays] = useState<DisplayInfo[]>([]);
  const [audio, setAudio] = useState<AudioDevices>({ inputs: [], defaultInput: null, output: null });
  const [levels, setLevels] = useState<[number, number]>([0, 0]);
  const [camLabel, setCamLabel] = useState<string>("");
  const [count, setCount] = useState<number | null>(null);
  const [error, setError] = useState("");
  const starting = useRef(false);

  const update = (patch: Partial<Settings>) => setS((p) => ({ ...p, ...patch }));

  useEffect(() => saveSettings(s), [s]);

  useEffect(() => {
    invoke<DisplayInfo[]>("list_displays").then(setDisplays);
    invoke<AudioDevices>("list_audio").then(setAudio);
    const un = [
      listen<[number, number]>("levels", (e) => setLevels(e.payload)),
      listen<string>("cam-info", (e) => setCamLabel(e.payload)),
      listen<Shape>("cam-shape-picked", (e) => update({ shape: e.payload })),
    ];
    return () => un.forEach((p) => p.then((f) => f()));
  }, []);

  // Live meters while the setup screen is open.
  useEffect(() => {
    invoke("set_monitor", { mic: s.mic, micDevice: s.micDevice, system: s.system }).catch(() => {});
  }, [s.mic, s.micDevice, s.system]);

  useEffect(() => {
    invoke("show_camera", { show: s.camera });
  }, [s.camera]);

  useEffect(() => {
    emitTo("camera", "cam-shape", s.shape);
  }, [s.shape]);

  const start = async () => {
    if (starting.current) return;
    starting.current = true;
    setError("");
    for (let i = 3; i > 0; i--) {
      setCount(i);
      await new Promise((r) => setTimeout(r, 800));
    }
    setCount(null);
    try {
      await invoke("start_recording", {
        opts: {
          display: s.display, fps: s.fps, resolution: outRes, mic: s.mic, micDevice: s.micDevice,
          system: s.system, camera: s.camera, shape: s.shape, layout: s.layout,
        },
      });
    } catch (e) {
      setError(String(e));
      invoke("set_monitor", { mic: s.mic, micDevice: s.micDevice, system: s.system }).catch(() => {});
    } finally {
      starting.current = false;
    }
  };

  // Global hotkey (Ctrl+Shift+R) starts only while this window is showing.
  const startRef = useRef(start);
  startRef.current = start;
  useEffect(() => {
    const un = listen("hotkey-toggle", async () => {
      if (await win.isVisible()) startRef.current();
    });
    return () => { un.then((f) => f()); };
  }, []);

  // When the window comes back after a recording, restart the meters.
  useEffect(() => {
    const un = win.onFocusChanged(({ payload }) => {
      if (payload) invoke("set_monitor", { mic: s.mic, micDevice: s.micDevice, system: s.system }).catch(() => {});
    });
    return () => { un.then((f) => f()); };
  }, [s.mic, s.micDevice, s.system]);

  const close = async () => {
    await invoke("show_camera", { show: false });
    await win.close();
  };

  const display = displays[s.display] ?? displays[0];
  // Only offer downscales; a choice at or above the display's height means native.
  const resOptions = RESOLUTIONS.filter(([r]) => r === 0 || !display || r < display.height);
  const outRes = display && s.resolution > 0 && s.resolution < display.height ? s.resolution : 0;
  const outSize = display && (outRes
    ? `${Math.round((display.width * outRes) / display.height / 2) * 2} × ${outRes}`
    : `${display.width} × ${display.height}`);
  const micName = s.micDevice ?? audio.defaultInput ?? "ไม่พบไมโครโฟน";

  return (
    <div className="window" style={{ position: "relative" }}>
      {count !== null && <div className="countdown">{count}</div>}
      <div className="setup" style={{ height: "100%" }}>
        <div className="titlebar" data-tauri-drag-region>
          <div className="brand"><span className="dot" />Recly</div>
          <div className="win-actions">
            <button className="icon-btn" onClick={() => win.minimize()} title="ย่อ"><Minus size={16} /></button>
            <button className="icon-btn" onClick={close} title="ปิด"><X size={16} /></button>
          </div>
        </div>

        <div className="setup-grid">
        <div className="col">
        <div className="section">
          <div className="section-head">
            <span className="label">แหล่งภาพ</span>
            <div style={{ display: "flex", gap: 6 }}>
              <div className="seg mini" title="ความชัด">
                {resOptions.map(([r, label]) => (
                  <button key={r} className={outRes === r ? "active" : ""} onClick={() => update({ resolution: r })}>{label}</button>
                ))}
              </div>
              <div className="seg mini">
                {([30, 60] as const).map((f) => (
                  <button key={f} className={s.fps === f ? "active" : ""} onClick={() => update({ fps: f })}>{f} fps</button>
                ))}
              </div>
            </div>
          </div>
          <div className="card">
            <div className="icon-box"><Monitor size={18} /></div>
            <div className="grow">
              <div className="title">
                {display ? `Display ${display.index + 1}${display.primary ? " · จอหลัก" : ""}` : "ทั้งหน้าจอ"}
              </div>
              <div className="sub">{display ? `${outSize} · ${s.fps} fps` : "กำลังค้นหาจอ…"}</div>
            </div>
            <ChevronDown size={16} color="var(--muted)" />
            {displays.length > 1 && (
              <select className="overlay-select" value={s.display} onChange={(e) => update({ display: +e.target.value })}>
                {displays.map((d) => (
                  <option key={d.index} value={d.index}>Display {d.index + 1} ({d.width}×{d.height})</option>
                ))}
              </select>
            )}
          </div>
        </div>

        <div className="section">
          <span className="label">เสียง</span>
          <div className="card">
            <div className="icon-box"><Mic size={18} /></div>
            <div className="grow" style={{ position: "relative" }}>
              <div className="title">ไมโครโฟน <span className="sub">{micName}</span></div>
              <Meter peak={levels[0]} active={s.mic} />
              {audio.inputs.length > 1 && (
                <select
                  className="overlay-select"
                  style={{ position: "absolute", inset: 0, opacity: 0, cursor: "pointer" }}
                  value={s.micDevice ?? ""}
                  onChange={(e) => update({ micDevice: e.target.value || null })}
                >
                  <option value="">ค่าเริ่มต้นของระบบ</option>
                  {audio.inputs.map((n) => <option key={n} value={n}>{n}</option>)}
                </select>
              )}
            </div>
            <Toggle on={s.mic} onChange={(v) => update({ mic: v })} />
          </div>
          <div className="card">
            <div className="icon-box"><Volume2 size={18} /></div>
            <div className="grow">
              <div className="title">เสียงในเครื่อง <span className="sub">{audio.output ?? "ลำโพง"}</span></div>
              <Meter peak={levels[1]} active={s.system} />
            </div>
            <Toggle on={s.system} onChange={(v) => update({ system: v })} />
          </div>
        </div>

        <div className="section record">
          <button className="btn-primary" onClick={start} disabled={count !== null}>
            <span className="rec-dot" />เริ่มอัด
          </button>
          {error ? <div className="error">{error}</div> : <div className="hint">Ctrl + Shift + R เริ่ม / หยุดได้ทุกที่</div>}
        </div>
        </div>

        <div className="col">
        <div className="section">
          <span className="label">กล้อง</span>
          <div className="card">
            <div className="icon-box"><Video size={18} /></div>
            <div className="grow">
              <div className="title">เว็บแคม</div>
              <div className="sub">{s.camera ? camLabel || "กำลังเปิดกล้อง…" : "ปิดอยู่"}</div>
            </div>
            <Toggle on={s.camera} onChange={(v) => update({ camera: v })} />
          </div>
          <div className="tiles">
            {SHAPES.map(({ id, label }) => (
              <button
                key={id}
                disabled={!s.camera}
                className={`tile ${s.shape === id ? "active" : ""}`}
                onClick={() => update({ shape: id as Shape })}
              >
                <span className="shape-swatch" style={{ borderRadius: shapeRadius(id, 28) }} />
                {label}
              </button>
            ))}
          </div>
        </div>

        <div className="section">
          <span className="label">เลย์เอาต์วิดีโอ</span>
          <div className="tiles">
            {([
              ["horizontal", "แนวนอน", "16:9 · YouTube"],
              ["vertical", "แนวตั้ง", "9:16 · TikTok / Reels"],
            ] as const).map(([id, label, sub]) => (
              <button key={id} className={`tile ${s.layout === id ? "active" : ""}`} onClick={() => update({ layout: id })}>
                <LayoutThumb layout={id} />
                <div>
                  <div>{label}</div>
                  <div className="sub">{sub}</div>
                </div>
              </button>
            ))}
          </div>
        </div>
        </div>
        </div>
      </div>
    </div>
  );
}
