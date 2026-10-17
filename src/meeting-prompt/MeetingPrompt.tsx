import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Mic, X } from "lucide-react";
import { commands, events } from "@/bindings";
import type {
  MeetingPrompt as Prompt,
  MeetingPromptResponse,
} from "@/bindings";
import { syncLanguageFromSettings } from "@/i18n";
import "./MeetingPrompt.css";

/** Detection popup: one prompt at a time, chosen by the backend. */
const MeetingPrompt: React.FC = () => {
  const { t } = useTranslation();
  const [prompt, setPrompt] = useState<Prompt | null>(null);
  const [countdown, setCountdown] = useState<number | null>(null);

  useEffect(() => {
    let active = true;
    syncLanguageFromSettings();
    commands.getMeetingPrompt().then((current) => {
      if (active) setPrompt(current);
    });
    const unlistenPrompt = events.meetingPromptChanged.listen((event) => {
      // A new prompt: pick up a language changed since the last one.
      if (event.payload.prompt) syncLanguageFromSettings();
      setPrompt(event.payload.prompt);
    });
    const unlistenCountdown = events.meetingAutoStopCountdown.listen((event) =>
      setCountdown(event.payload.remaining_secs),
    );
    return () => {
      active = false;
      unlistenPrompt.then((fn) => fn());
      unlistenCountdown.then((fn) => fn());
    };
  }, []);

  if (!prompt) return null;

  const respond = (response: MeetingPromptResponse) =>
    commands.respondMeetingPrompt(response);
  const app = prompt.app_name;

  let title: string;
  let subtitle: string;
  let actions: React.ReactNode;
  switch (prompt.kind) {
    case "detected":
      title = t("meetings.prompt.detected.title", { app });
      subtitle = t("meetings.prompt.detected.subtitle");
      actions = (
        <>
          <button
            type="button"
            className="mp-button mp-primary"
            onClick={() => respond("record")}
          >
            {t("meetings.prompt.detected.record")}
          </button>
          <button
            type="button"
            className="mp-button"
            onClick={() => respond("ignore")}
          >
            {t("meetings.prompt.detected.ignore")}
          </button>
        </>
      );
      break;
    case "stop":
      title = t("meetings.prompt.stop.title", { app });
      subtitle = t("meetings.prompt.stop.subtitle");
      actions = (
        <>
          <button
            type="button"
            className="mp-button mp-danger"
            onClick={() => respond("stop")}
          >
            {t("meetings.prompt.stop.stop")}
          </button>
          <button
            type="button"
            className="mp-button"
            onClick={() => respond("keep_recording")}
          >
            {t("meetings.prompt.stop.keep")}
          </button>
        </>
      );
      break;
    case "auto_stop":
      title = t("meetings.prompt.autoStop.title", {
        seconds: countdown ?? 0,
      });
      subtitle = t("meetings.prompt.autoStop.subtitle", { app });
      actions = (
        <button
          type="button"
          className="mp-button"
          onClick={() => respond("keep_recording")}
        >
          {t("meetings.prompt.autoStop.cancel")}
        </button>
      );
      break;
    case "started":
      title = t("meetings.prompt.started.title", { app });
      subtitle = t("meetings.prompt.started.subtitle");
      actions = (
        <button
          type="button"
          className="mp-close"
          onClick={() => respond("dismiss")}
          title={t("meetings.prompt.close")}
          aria-label={t("meetings.prompt.close")}
        >
          <X size={14} aria-hidden="true" />
        </button>
      );
      break;
  }

  return (
    <div className="mp-card" role="alertdialog" aria-label={title}>
      <span className="mp-icon" aria-hidden="true">
        <Mic size={16} />
      </span>
      <div className="mp-text">
        <span className="mp-title">{title}</span>
        <span className="mp-subtitle">{subtitle}</span>
      </div>
      <div className="mp-actions">{actions}</div>
    </div>
  );
};

export default MeetingPrompt;
