import React from "react";
import { useTranslation } from "react-i18next";
import type { MeetingSegment, Speaker, SpeakerName } from "@/bindings";
import { SpeakerChip } from "../speakers/SpeakerChip";
import { personClasses, personLabel, personName } from "../speakers/speakers";
import { formatTimestamp, speakerLabel } from "./format";

const SPEAKER_CLASSES: Record<Speaker, string> = {
  me: "bg-logo-primary/25 text-text",
  others: "bg-sky-500/20 text-sky-500",
  mic_only: "bg-mid-gray/20 text-text/70",
};

interface MeetingTranscriptProps {
  segments: MeetingSegment[];
  /** Names the user gave the people speaker detection found. */
  speakerNames?: SpeakerName[];
  /** Renames a person speaker detection found (empty: default label). */
  onRenameSpeaker?: (speakerId: number, name: string) => void;
  /** Playback position, to highlight the segment being played. */
  currentMs: number | null;
  /** Called with a segment start when its timestamp is clicked. */
  onSeek?: (ms: number) => void;
}

export const MeetingTranscript: React.FC<MeetingTranscriptProps> = ({
  segments,
  speakerNames = [],
  onRenameSpeaker,
  currentMs,
  onSeek,
}) => {
  const { t } = useTranslation();

  return (
    <ol className="space-y-1">
      {segments.map((segment) => {
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
            {segment.speaker_id != null ? (
              <SpeakerChip
                label={personLabel(t, segment.speaker_id, speakerNames)}
                name={personName(segment.speaker_id, speakerNames)}
                className={personClasses(segment.speaker_id)}
                onRename={
                  onRenameSpeaker
                    ? (name) => onRenameSpeaker(segment.speaker_id!, name)
                    : undefined
                }
              />
            ) : (
              <SpeakerChip
                label={speakerLabel(t, segment.speaker)}
                className={SPEAKER_CLASSES[segment.speaker]}
              />
            )}
            <p className="text-sm text-text/90 select-text whitespace-pre-wrap break-words">
              {segment.text}
            </p>
          </li>
        );
      })}
    </ol>
  );
};
