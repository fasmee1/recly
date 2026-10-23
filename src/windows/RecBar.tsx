import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { GripVertical, Mic, Trash2, Volume2 } from "lucide-react";
import { fmtTime, loadSettings, meterSegments } from "../lib/shared";

const HEIGHTS = [6, 10, 14, 9, 5];

function Bars({ peak }: { peak: number }) {
  const lit = meterSegments(peak, HEIGHTS.length);
  return (
    <div className="bars">
      {HEIGHTS.map((h, i) => <i key={i} style={{ height: h }} className={i < lit ? "lit" : ""} />)}
    </div>
  );
}

export default function RecBar() {
  const settings = useRef(loadSettings()).current;
  const [started, setStarted] = useState<number | null>(null);
  const [now, setNow] = useState(Date.now());
  const [levels, setLevels] = useState<[number, number]>([0, 0]);
  const [confirmDiscard, setConfirmDiscard] = useState(false);
  const [busy, setBusy] = useState(false);
  const busyRef = useRef(false);

  useEffect(() => {
    invoke<number | null>("recording_started").then((ms) => setStarted(ms ?? Date.now()));
    const t = setInterval(() => setNow(Date.now()), 250);
    const un = [
      listen<[number, number]>("levels", (e) => setLevels(e.payload)),
      listen("hotkey-toggle", () => stop()),
    ];
    return () => {
      clearInterval(t);
      un.forEach((p) => p.then((f) => f()));
    };
  }, []);

  useEffect(() => {
    if (!confirmDiscard) return;
    const t = setTimeout(() => setConfirmDiscard(false), 3000);
    return () => clearTimeout(t);
  }, [confirmDiscard]);

  async function stop() {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    try {
      await invoke("stop_recording");
    } catch (e) {
      console.error(e);
      busyRef.current = false;
      setBusy(false);
    }
  }

  async function discard() {
    if (!confirmDiscard) return setConfirmDiscard(true);
    busyRef.current = true;
    setBusy(true);
    await invoke("discard_recording");
  }

  return (
    <div className="recbar-root">
      <div className="pill">
        <div className="grip" data-tauri-drag-region><GripVertical size={16} style={{ pointerEvents: "none" }} /></div>
        <div className="timer">
          <span className="pulse" />
          {fmtTime(started ? (now - started) / 1000 : 0)}
        </div>
        {busy ? (
          <div className="saving">กำลังบันทึก…</div>
        ) : (
          <>
            <div className="divider" />
            <div className={`mini-meter ${settings.mic ? "" : "off"}`} title="ไมโครโฟน">
              <Mic size={17} /><Bars peak={settings.mic ? levels[0] : 0} />
            </div>
            <div className={`mini-meter ${settings.system ? "" : "off"}`} title="เสียงในเครื่อง">
              <Volume2 size={17} /><Bars peak={settings.system ? levels[1] : 0} />
            </div>
            <div className="divider" />
            <button className="round stop" onClick={stop} title="หยุดอัด (Ctrl+Shift+R)"><span /></button>
            {confirmDiscard ? (
              <button className="round confirm" onClick={discard}>ทิ้งคลิปนี้?</button>
            ) : (
              <button className="round ghost" onClick={discard} title="ทิ้ง"><Trash2 size={17} /></button>
            )}
          </>
        )}
      </div>
    </div>
  );
}
