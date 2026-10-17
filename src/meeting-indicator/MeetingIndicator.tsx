import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { TriangleAlert } from "lucide-react";
import { commands, events } from "@/bindings";
import type { CaptureWarning, MeetingStateChanged } from "@/bindings";
import { syncLanguageFromSettings } from "@/i18n";
import "./MeetingIndicator.css";

const IDLE: MeetingStateChanged = {
  state: "idle",
  meeting_id: null,
  elapsed_ms: 0,
  warnings: [],
  mic_level: 0,
  system_level: 0,
};

const formatElapsed = (ms: number): string => {
  const total = Math.floor(ms / 1000);
  const h = Math.floor(total / 3600);
  const m = Math.floor(total / 60) % 60;
  const s = total % 60;
  const pad = (n: number) => n.toString().padStart(2, "0");
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${pad(m)}:${pad(s)}`;
};

// Levels are RMS (0..1); speech sits far below 1, so stretch the low end.
const meterHeight = (rms: number): string =>
  `${Math.round(Math.min(1, Math.sqrt(Math.max(0, rms)) * 1.8) * 100)}%`;

const Meter: React.FC<{ level: number; label: string }> = ({
  level,
  label,
}) => (
  <div className="mi-meter" title={label} aria-hidden="true">
    <div className="mi-meter-fill" style={{ height: meterHeight(level) }} />
  </div>
);

const MeetingIndicator: React.FC = () => {
  const { t } = useTranslation();
  const [meeting, setMeeting] = useState<MeetingStateChanged>(IDLE);
  const [stopping, setStopping] = useState(false);
  // Auto-stop countdown of a detected meeting whose app left the call.
  const [countdown, setCountdown] = useState<number | null>(null);

  useEffect(() => {
    let active = true;
    syncLanguageFromSettings();
    commands.getMeetingState().then((state) => {
      if (active) setMeeting(state);
    });
    const unlisten = events.meetingStateChanged.listen((event) => {
      setMeeting((previous) => {
        // A new recording: pick up a language changed since the last one.
        if (previous.state === "idle" && event.payload.state !== "idle") {
          syncLanguageFromSettings();
        }
        return event.payload;
      });
      if (event.payload.state !== "recording") setStopping(false);
    });
    const unlistenCountdown = events.meetingAutoStopCountdown.listen((event) =>
      setCountdown(event.payload.remaining_secs),
    );
    return () => {
      active = false;
      unlisten.then((fn) => fn());
      unlistenCountdown.then((fn) => fn());
    };
  }, []);

  const busy = stopping || meeting.state === "stopping";
  const counting = countdown !== null && meeting.state === "recording";

  const stop = async () => {
    if (busy) return;
    setStopping(true);
    const result = await commands.stopMeeting();
    if (result.status === "error") {
      console.error("Failed to stop the meeting:", result.error);
      setStopping(false);
    }
  };

  const warningText = (warning: CaptureWarning) =>
    t(`meetings.indicator.warnings.${warning}`);
  const warnings = meeting.warnings.map(warningText).join("\n");

  return (
    <div
      className="mi-pill"
      data-tauri-drag-region
      role="status"
      aria-label={t("meetings.indicator.recording")}
    >
      <span
        className={`mi-dot${busy ? " mi-dot-idle" : ""}`}
        data-tauri-drag-region
        aria-hidden="true"
      />
      {counting ? (
        <>
          <span className="mi-time mi-countdown" data-tauri-drag-region>
            {t("meetings.indicator.autoStop", { seconds: countdown })}
          </span>
          <button
            type="button"
            className="mi-cancel"
            onClick={() => commands.respondMeetingPrompt("keep_recording")}
          >
            {t("meetings.indicator.cancelAutoStop")}
          </button>
        </>
      ) : (
        <>
          <span className="mi-time" data-tauri-drag-region>
            {formatElapsed(meeting.elapsed_ms)}
          </span>
          <div className="mi-meters" data-tauri-drag-region>
            <Meter
              level={meeting.mic_level}
              label={t("meetings.indicator.microphone")}
            />
            <Meter
              level={meeting.system_level}
              label={t("meetings.indicator.systemAudio")}
            />
          </div>
        </>
      )}
      {warnings && !counting && (
        <span className="mi-warning" title={warnings} aria-label={warnings}>
          <TriangleAlert size={14} aria-hidden="true" />
        </span>
      )}
      <button
        type="button"
        className="mi-stop"
        onClick={stop}
        disabled={busy}
        title={t("meetings.indicator.stop")}
        aria-label={t("meetings.indicator.stop")}
      >
        <span className="mi-stop-square" aria-hidden="true" />
      </button>
    </div>
  );
};

export default MeetingIndicator;
