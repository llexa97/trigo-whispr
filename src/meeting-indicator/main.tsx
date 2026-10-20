import React from "react";
import ReactDOM from "react-dom/client";
import "@fontsource-variable/geist";
import { listen } from "@tauri-apps/api/event";
import MeetingIndicator from "./MeetingIndicator";
import {
  applyTheme,
  getStoredTheme,
  syncThemeFromSettings,
} from "@/lib/utils/theme";
import type { Theme } from "@/bindings";
import "@/i18n";

// Separate webview: follow the app theme the same way the dictation overlay
// does (see src/overlay/main.tsx).
applyTheme(getStoredTheme());
syncThemeFromSettings();
listen<Theme>("theme-changed", (event) => applyTheme(event.payload));

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <MeetingIndicator />
  </React.StrictMode>,
);
