import type { TFunction } from "i18next";
import type { Meeting, Speaker } from "@/bindings";

const pad = (n: number) => n.toString().padStart(2, "0");

/** `hh:mm:ss`, as used by transcript timestamps. */
export const formatTimestamp = (ms: number): string => {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  return `${pad(h)}:${pad(m)}:${pad(s)}`;
};

/** `mm:ss`, or `h:mm:ss` past one hour (recording timer, durations). */
export const formatDuration = (ms: number): string => {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${pad(m)}:${pad(s)}`;
};

export const formatMeetingDate = (epochMs: number, locale: string): string => {
  const date = new Date(epochMs);
  if (isNaN(date.getTime())) return "";
  return new Intl.DateTimeFormat(locale, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(date);
};

export const speakerLabel = (t: TFunction, speaker: Speaker): string =>
  t(`meetings.speaker.${speaker}`);

export const sourceLabel = (t: TFunction, meeting: Meeting): string =>
  meeting.source_app ?? t("meetings.source.manual");

/** Converts an RMS level (0–1) to a meter position (0–1) on a -60 dB scale. */
export const levelToMeter = (rms: number): number => {
  if (!(rms > 0)) return 0;
  const db = 20 * Math.log10(rms);
  return Math.min(1, Math.max(0, (db + 60) / 60));
};

/** Safe file name for the Markdown export. */
export const markdownFileName = (title: string): string =>
  `${
    title
      .replace(/[\\/:*?"<>|]+/g, " ")
      .replace(/\s+/g, " ")
      .trim() || "meeting"
  }.md`;
