import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { emitTo, listen } from "@tauri-apps/api/event";
import { SHAPES, Shape, loadSettings, saveSettings, shapeClip, shapeRadius } from "../lib/shared";

// Prefer H.264 (often hardware-encoded in WebView2), fall back to VP8.
const MIME = ["video/webm;codecs=h264", "video/webm;codecs=vp8", "video/webm"].find((m) =>
  MediaRecorder.isTypeSupported(m),
);

export default function Camera() {
  const video = useRef<HTMLVideoElement>(null);
  const stream = useRef<MediaStream | null>(null);
  const recorder = useRef<MediaRecorder | null>(null);
  const [shape, setShape] = useState<Shape>(() => loadSettings().shape);
  const [error, setError] = useState("");
  const [recording, setRecording] = useState(false);

  useEffect(() => {
    navigator.mediaDevices
      .getUserMedia({ video: { width: { ideal: 1280 }, height: { ideal: 720 }, frameRate: { ideal: 30 } }, audio: false })
      .then((ms) => {
        stream.current = ms;
        if (video.current) video.current.srcObject = ms;
        emitTo("main", "cam-info", ms.getVideoTracks()[0]?.label.replace(/\s*\(.*\)$/, "") || "เว็บแคม");
      })
      .catch((e) => {
        setError("เปิดกล้องไม่ได้");
        emitTo("main", "cam-info", `เปิดกล้องไม่ได้ (${e.name})`);
      });

    const un = [
      listen<Shape>("cam-shape", (e) => setShape(e.payload)),
      listen<number>("rec-start", () => startRecording()),
      listen("rec-stop", () => recorder.current?.stop()),
    ];
    return () => {
      un.forEach((p) => p.then((f) => f()));
      stream.current?.getTracks().forEach((t) => t.stop());
    };
  }, []);

  function startRecording() {
    if (!stream.current || recorder.current?.state === "recording") return;
    const rec = new MediaRecorder(stream.current, { mimeType: MIME, videoBitsPerSecond: 4_000_000 });
    let startMs = 0;
    // Chunks are appended to cam.webm in order, so keep the uploads serialized.
    let chain = Promise.resolve();
    rec.onstart = () => {
      startMs = Date.now();
      setRecording(true);
    };
    rec.ondataavailable = (e) => {
      if (!e.data.size) return;
      chain = chain.then(async () => {
        await invoke("cam_chunk", new Uint8Array(await e.data.arrayBuffer()));
      });
    };
    rec.onstop = async () => {
      setRecording(false);
      await chain;
      await invoke("cam_finished", { startMs });
      recorder.current = null;
    };
    recorder.current = rec;
    rec.start(1000);
  }

  const pick = (s: Shape) => {
    setShape(s);
    saveSettings({ ...loadSettings(), shape: s });
    emitTo("main", "cam-shape-picked", s);
  };

  return (
    <div className="cam-root">
      <div className="bubble" style={{ borderRadius: shapeRadius(shape, 200) }}>
        <video ref={video} autoPlay muted playsInline style={{ clipPath: shapeClip(shape, 200) }} />
        {error && <div className="cam-msg">{error}</div>}
        <div className="drag" data-tauri-drag-region />
      </div>
      {recording && <span className="rec-badge" />}
      <div className="cam-tools">
        {SHAPES.map(({ id, label }) => (
          <button key={id} title={label} className={shape === id ? "active" : ""} onClick={() => pick(id)}>
            <i style={{ borderRadius: shapeRadius(id, 13) }} />
          </button>
        ))}
      </div>
    </div>
  );
}
