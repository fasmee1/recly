import React from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import "@fontsource/ibm-plex-sans-thai/400.css";
import "@fontsource/ibm-plex-sans-thai/500.css";
import "@fontsource/ibm-plex-sans-thai/600.css";
import "./styles.css";
import Setup from "./windows/Setup";
import Camera from "./windows/Camera";
import RecBar from "./windows/RecBar";
import Export from "./windows/Export";

// Every window loads the same bundle; the window label picks the screen.
const label = getCurrentWindow().label;
const View = { camera: Camera, recbar: RecBar, export: Export }[label] ?? Setup;
document.documentElement.dataset.window = label;

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <View />
  </React.StrictMode>,
);
