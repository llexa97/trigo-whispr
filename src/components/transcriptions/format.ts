import type { FileTranscription, FileTranscriptionSegment } from "@/bindings";
import { formatTimestamp } from "../meetings/format";

export type ExportFormat = "txt" | "md" | "srt";

/** A segment's speaker label, when speaker detection told people apart. */
export type SpeakerOf = (
  segment: FileTranscriptionSegment,
) => string | undefined;

/** The segment's text, after its speaker's label when it has one. */
const withSpeaker = (
  segment: FileTranscriptionSegment,
  speakerOf?: SpeakerOf,
): string => {
  const text = segment.text.trim();
  const speaker = speakerOf?.(segment);
  return speaker ? `${speaker}: ${text}` : text;
};

export interface ExportLabels {
  transcript: string;
  postProcessed: string;
  speakerOf?: SpeakerOf;
}

/** Lower-case extension of a path, without the dot. */
export const extensionOf = (path: string): string => {
  const name = path.split(/[\\/]/).pop() ?? "";
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(dot + 1).toLowerCase() : "";
};

/** Splits dropped paths into the ones the import accepts and the others. */
export const partitionPaths = (paths: string[], extensions: string[]) => {
  const accepted = new Set(extensions);
  const supported: string[] = [];
  const unsupported: string[] = [];
  for (const path of paths) {
    (accepted.has(extensionOf(path)) ? supported : unsupported).push(path);
  }
  return { supported, unsupported };
};

/** File name shown for a path (Windows or POSIX). */
export const baseName = (path: string): string =>
  path.split(/[\\/]/).pop() || path;

/** The transcript as plain paragraphs. */
export const toPlainText = (
  segments: FileTranscriptionSegment[],
  speakerOf?: SpeakerOf,
): string => segments.map((s) => withSpeaker(s, speakerOf)).join("\n") + "\n";

/** `hh:mm:ss,mmm`, as SubRip wants it. */
export const srtTimestamp = (ms: number): string => {
  const total = Math.max(0, Math.round(ms));
  const pad = (n: number, width = 2) => n.toString().padStart(width, "0");
  const h = Math.floor(total / 3_600_000);
  const m = Math.floor((total % 3_600_000) / 60_000);
  const s = Math.floor((total % 60_000) / 1000);
  return `${pad(h)}:${pad(m)}:${pad(s)},${pad(total % 1000, 3)}`;
};

/** SubRip subtitles, one cue per segment. */
export const toSrt = (
  segments: FileTranscriptionSegment[],
  speakerOf?: SpeakerOf,
): string =>
  segments
    .map((s, i) => {
      // A cue must last; untimed segments get at least one second.
      const end = Math.max(s.end_ms, s.start_ms + 1000);
      return `${i + 1}\n${srtTimestamp(s.start_ms)} --> ${srtTimestamp(end)}\n${withSpeaker(s, speakerOf)}\n`;
    })
    .join("\n");

/** Markdown with timestamps, and the post-processed text when there is one. */
export const toMarkdown = (
  transcription: FileTranscription,
  segments: FileTranscriptionSegment[],
  labels: ExportLabels,
): string => {
  const lines = [`# ${transcription.file_name}`, ""];
  if (transcription.post_processed_text) {
    lines.push(
      `## ${labels.postProcessed}`,
      "",
      transcription.post_processed_text.trim(),
      "",
    );
  }
  lines.push(`## ${labels.transcript}`, "");
  for (const s of segments) {
    const time = formatTimestamp(s.start_ms);
    const speaker = labels.speakerOf?.(s);
    lines.push(
      speaker
        ? `**[${time}] ${speaker}:** ${s.text.trim()}`
        : `**[${time}]** ${s.text.trim()}`,
      "",
    );
  }
  return lines.join("\n");
};

export const renderExport = (
  format: ExportFormat,
  transcription: FileTranscription,
  segments: FileTranscriptionSegment[],
  labels: ExportLabels,
): string => {
  switch (format) {
    case "txt":
      return toPlainText(segments, labels.speakerOf);
    case "srt":
      return toSrt(segments, labels.speakerOf);
    case "md":
      return toMarkdown(transcription, segments, labels);
  }
};

/** Export file name: the imported file's name with the export extension. */
export const exportFileName = (
  fileName: string,
  format: ExportFormat,
): string => {
  const dot = fileName.lastIndexOf(".");
  const stem = (dot > 0 ? fileName.slice(0, dot) : fileName)
    .replace(/[\\/:*?"<>|]+/g, " ")
    .replace(/\s+/g, " ")
    .trim();
  return `${stem || "transcription"}.${format}`;
};

/** Above this, an import warns that it will take a while. */
export const LONG_FILE_MS = 60 * 60 * 1000;
