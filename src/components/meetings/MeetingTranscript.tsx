import React from "react";
import { useTranslation } from "react-i18next";
import type { MeetingSegment } from "@/bindings";
import {
  formatTimestamp,
  speakerKey,
  speakerLabel,
  type SpeakerKey,
} from "./format";

const SPEAKER_CLASSES: Record<SpeakerKey, string> = {
  me: "bg-logo-primary/25 text-text",
  others: "bg-sky-500/20 text-sky-500",
  mic_only: "bg-mid-gray/20 text-text/70",
};

interface MeetingTranscriptProps {
  segments: MeetingSegment[];
  /** Playback position, to highlight the segment being played. */
  currentMs: number | null;
  /** Called with a segment start when its timestamp is clicked. */
  onSeek?: (ms: number) => void;
}

export const MeetingTranscript: React.FC<MeetingTranscriptProps> = ({
  segments,
  currentMs,
  onSeek,
}) => {
  const { t } = useTranslation();

  return (
    <ol className="space-y-1">
      {segments.map((segment) => {
        const key = speakerKey(segment.speaker);
        const active =
          currentMs !== null &&
          currentMs >= segment.start_ms &&
          currentMs < segment.end_ms;
        const time = formatTimestamp(segment.start_ms);
        return (
          <li
            key={segment.idx}
            aria-current={active ? "true" : undefined}
            className={`flex items-start gap-2 rounded-md px-2 py-1.5 transition-colors ${
              active ? "bg-logo-primary/15" : ""
            }`}
          >
            {onSeek ? (
              <button
                type="button"
                onClick={() => onSeek(segment.start_ms)}
                className="text-xs font-mono text-text/60 hover:text-logo-primary cursor-pointer pt-0.5 shrink-0 focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary rounded"
                aria-label={t("meetings.transcript.playFrom", { time })}
              >
                {`[${time}]`}
              </button>
            ) : (
              <span className="text-xs font-mono text-text/60 pt-0.5 shrink-0">
                {`[${time}]`}
              </span>
            )}
            <span
              className={`text-xs font-medium px-1.5 py-0.5 rounded shrink-0 ${SPEAKER_CLASSES[key]}`}
            >
              {speakerLabel(t, segment.speaker)}
            </span>
            <p className="text-sm text-text/90 select-text whitespace-pre-wrap break-words">
              {segment.text}
            </p>
          </li>
        );
      })}
    </ol>
  );
};
