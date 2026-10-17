import React, { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { FileAudio } from "lucide-react";
import type { FileTranscription, FileTranscriptionProgress } from "@/bindings";
import { useTranscriptionsStore } from "@/stores/transcriptionsStore";
import { formatDuration, formatMeetingDate } from "../meetings/format";
import { TranscriptionStatusBadge } from "./TranscriptionStatusBadge";

const TranscriptionRow: React.FC<{
  transcription: FileTranscription;
  progress?: FileTranscriptionProgress;
  onOpen: () => void;
}> = ({ transcription, progress, onOpen }) => {
  const { i18n } = useTranslation();
  return (
    <li>
      <button
        type="button"
        onClick={onOpen}
        data-transcription-row
        className="w-full text-start px-4 py-3 flex items-center gap-3 cursor-pointer hover:bg-mid-gray/10 focus:outline-none focus-visible:bg-mid-gray/10 focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-logo-primary"
      >
        <FileAudio
          className="w-4 h-4 shrink-0 text-text/50"
          aria-hidden="true"
        />
        <div className="flex-1 min-w-0 space-y-1">
          <p
            className="text-sm font-medium truncate"
            title={transcription.file_name}
          >
            {transcription.file_name}
          </p>
          <p className="text-xs text-text/60 flex items-center gap-1.5 flex-wrap">
            <span>
              {formatMeetingDate(transcription.created_at, i18n.language)}
            </span>
            {transcription.duration_ms != null && (
              <>
                <span aria-hidden="true">·</span>
                <span className="tabular-nums">
                  {formatDuration(transcription.duration_ms)}
                </span>
              </>
            )}
          </p>
        </div>
        <TranscriptionStatusBadge
          status={transcription.status}
          progress={progress}
        />
      </button>
    </li>
  );
};

/** Transcriptions, newest first, with infinite scroll and arrow keys. */
export const TranscriptionList: React.FC<{ onOpen: (id: string) => void }> = ({
  onOpen,
}) => {
  const { t } = useTranslation();
  const transcriptions = useTranscriptionsStore((s) => s.transcriptions);
  const hasMore = useTranscriptionsStore((s) => s.hasMore);
  const loading = useTranscriptionsStore((s) => s.loading);
  const progress = useTranscriptionsStore((s) => s.progress);
  const loadMore = useTranscriptionsStore((s) => s.loadMore);
  const sentinelRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const sentinel = sentinelRef.current;
    if (!sentinel || !hasMore || loading) return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries[0]?.isIntersecting) loadMore();
      },
      { threshold: 0 },
    );
    observer.observe(sentinel);
    return () => observer.disconnect();
  }, [hasMore, loading, loadMore, transcriptions.length]);

  const handleKeyDown = (event: React.KeyboardEvent<HTMLUListElement>) => {
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
    const rows = Array.from(
      event.currentTarget.querySelectorAll<HTMLButtonElement>(
        "[data-transcription-row]",
      ),
    );
    if (rows.length === 0) return;
    event.preventDefault();
    const index = rows.indexOf(document.activeElement as HTMLButtonElement);
    const next =
      event.key === "Home"
        ? 0
        : event.key === "End"
          ? rows.length - 1
          : event.key === "ArrowDown"
            ? Math.min(rows.length - 1, index + 1)
            : Math.max(0, index - 1);
    rows[next]?.focus();
  };

  return (
    <>
      <ul
        className="divide-y divide-mid-gray/20"
        onKeyDown={handleKeyDown}
        aria-label={t("transcriptions.title")}
      >
        {transcriptions.map((transcription) => (
          <TranscriptionRow
            key={transcription.id}
            transcription={transcription}
            progress={progress[transcription.id]}
            onOpen={() => onOpen(transcription.id)}
          />
        ))}
      </ul>
      <div ref={sentinelRef} className="h-1" />
    </>
  );
};
