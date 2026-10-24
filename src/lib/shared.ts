export type Shape = "circle" | "square" | "rounded";
export type Layout = "horizontal" | "vertical";

export interface Settings {
  display: number;
  fps: 30 | 60;
  /** Recorded height; 0 = the display's native resolution. */
  resolution: number;
  mic: boolean;
  micDevice: string | null;
  system: boolean;
  camera: boolean;
  shape: Shape;
  layout: Layout;
}

const KEY = "recly.settings";

export const defaultSettings: Settings = {
  display: 0,
  fps: 30,
  resolution: 0,
  mic: true,
  micDevice: null,
  system: true,
  camera: true,
  shape: "circle",
  layout: "horizontal",
};

// localStorage is shared by all windows (same origin), so the camera bubble
// can read the chosen shape on launch.
export function loadSettings(): Settings {
  try {
    return { ...defaultSettings, ...JSON.parse(localStorage.getItem(KEY) ?? "{}") };
  } catch {
    return defaultSettings;
  }
}

export function saveSettings(s: Settings) {
  localStorage.setItem(KEY, JSON.stringify(s));
}

export const SHAPES: { id: Shape; label: string }[] = [
  { id: "circle", label: "วงกลม" },
  { id: "square", label: "สี่เหลี่ยม" },
  { id: "rounded", label: "มุมมน" },
];

/** CSS border-radius for a camera shape at a given pixel size. */
export function shapeRadius(shape: Shape, size: number): string {
  if (shape === "circle") return "50%";
  return `${Math.round(size * (shape === "square" ? 0.06 : 0.22))}px`;
}

/**
 * CSS clip-path for a camera shape. WebView2 ignores border-radius + overflow
 * clipping on (mirrored) <video> layers, so videos must be clipped directly.
 */
export function shapeClip(shape: Shape, size: number): string {
  return shape === "circle" ? "circle(50%)" : `inset(0 round ${shapeRadius(shape, size)})`;
}

/** Peak (0..1) → number of lit meter segments. */
export function meterSegments(peak: number, segments: number): number {
  if (peak <= 0) return 0;
  const db = 20 * Math.log10(peak);
  return Math.max(0, Math.min(segments, Math.round(((db + 54) / 54) * segments)));
}

export function fmtTime(sec: number): string {
  const s = Math.max(0, Math.floor(sec));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const r = s % 60;
  const mm = String(m).padStart(2, "0");
  const ss = String(r).padStart(2, "0");
  return h ? `${h}:${mm}:${ss}` : `${mm}:${ss}`;
}
