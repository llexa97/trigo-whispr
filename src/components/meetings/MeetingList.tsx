import React, { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { AppWindow, Hand } from "lucide-react";
import type { Meeting } from "@/bindings";
import { useMeetingsStore } from "@/stores/meetingsStore";
import { formatDuration, formatMeetingDate, sourceLabel } from "./format";
import { MeetingStatusBadge } from "./MeetingStatusBadge";

const MeetingRow: React.FC<{
  meeting: Meeting;
  progress?: number;
  onOpen: () => void;
}> = ({ meeting, progress, onOpen }) => {
  const { t, i18n } = useTranslation();
  const SourceIcon = meeting.source_app ? AppWindow : Hand;

  return (
    <li>
      <button
        type="button"
        onClick={onOpen}
        data-meeting-row
        className="w-full text-start px-4 py-3 flex items-center gap-3 cursor-pointer hover:bg-mid-gray/10 focus:outline-none focus-visible:bg-mid-gray/10 focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-logo-primary"
      >
        <div className="flex-1 min-w-0 space-y-1">
          <p className="text-sm font-medium truncate">{meeting.title}</p>
          <p className="text-xs text-text/60 flex items-center gap-1.5 flex-wrap">
            <span>{formatMeetingDate(meeting.started_at, i18n.language)}</span>
            {meeting.duration_ms != null && (
              <>
                <span aria-hidden="true">·</span>
                <span className="tabular-nums">
                  {formatDuration(meeting.duration_ms)}
                </span>
              </>
            )}
            <span aria-hidden="true">·</span>
            <span className="inline-flex items-center gap-1">
              <SourceIcon className="w-3 h-3" aria-hidden="true" />
              {sourceLabel(t, meeting)}
            </span>
          </p>
        </div>
        <MeetingStatusBadge status={meeting.status} progress={progress} />
      </button>
    </li>
  );
};

interface MeetingListProps {
  onOpen: (id: string) => void;
}

/** Meetings, newest first, with infinite scroll and arrow-key navigation. */
export const MeetingList: React.FC<MeetingListProps> = ({ onOpen }) => {
  const { t } = useTranslation();
  const meetings = useMeetingsStore((s) => s.meetings);
  const hasMore = useMeetingsStore((s) => s.hasMore);
  const loading = useMeetingsStore((s) => s.loading);
  const progress = useMeetingsStore((s) => s.progress);
  const loadMore = useMeetingsStore((s) => s.loadMore);
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
  }, [hasMore, loading, loadMore, meetings.length]);

  const handleKeyDown = (event: React.KeyboardEvent<HTMLUListElement>) => {
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
    const rows = Array.from(
      event.currentTarget.querySelectorAll<HTMLButtonElement>(
        "[data-meeting-row]",
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
        aria-label={t("meetings.title")}
      >
        {meetings.map((meeting) => (
          <MeetingRow
            key={meeting.id}
            meeting={meeting}
            progress={progress[meeting.id]?.progress}
            onOpen={() => onOpen(meeting.id)}
          />
        ))}
      </ul>
      <div ref={sentinelRef} className="h-1" />
    </>
  );
};
