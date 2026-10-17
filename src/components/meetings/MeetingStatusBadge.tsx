import React from "react";
import { useTranslation } from "react-i18next";
import type { MeetingStatus } from "@/bindings";

const STATUS_CLASSES: Record<MeetingStatus, string> = {
  recording: "bg-red-500/15 text-red-400",
  pending: "bg-mid-gray/20 text-text/70",
  transcribing: "bg-logo-primary/20 text-text",
  done: "bg-green-500/15 text-green-500",
  failed: "bg-red-500/15 text-red-400",
  recovered: "bg-warning/15 text-warning",
};

interface MeetingStatusBadgeProps {
  status: MeetingStatus;
  /** Transcription progress (0–1), when known. */
  progress?: number;
}

export const MeetingStatusBadge: React.FC<MeetingStatusBadgeProps> = ({
  status,
  progress,
}) => {
  const { t } = useTranslation();
  const label =
    status === "transcribing" && progress !== undefined
      ? t("meetings.status.transcribingProgress", {
          percent: Math.round(progress * 100),
        })
      : t(`meetings.status.${status}`);

  return (
    <span
      className={`inline-flex items-center px-2 py-0.5 rounded-full text-xs font-medium whitespace-nowrap ${STATUS_CLASSES[status]}`}
    >
      {label}
    </span>
  );
};
