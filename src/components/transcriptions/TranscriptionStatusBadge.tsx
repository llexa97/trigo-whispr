import React from "react";
import { useTranslation } from "react-i18next";
import type {
  FileTranscriptionProgress,
  FileTranscriptionStatus,
} from "@/bindings";

const STATUS_CLASSES: Record<FileTranscriptionStatus, string> = {
  pending: "bg-mid-gray/20 text-text/70",
  decoding: "bg-logo-primary/20 text-text",
  transcribing: "bg-logo-primary/20 text-text",
  done: "bg-green-500/15 text-green-500",
  failed: "bg-red-500/15 text-red-400",
  cancelled: "bg-warning/15 text-warning",
};

interface TranscriptionStatusBadgeProps {
  status: FileTranscriptionStatus;
  progress?: FileTranscriptionProgress;
}

export const TranscriptionStatusBadge: React.FC<
  TranscriptionStatusBadgeProps
> = ({ status, progress }) => {
  const { t } = useTranslation();
  const running = status === "decoding" || status === "transcribing";
  const percent =
    running && progress?.progress != null
      ? Math.round(progress.progress * 100)
      : null;
  const label =
    percent !== null
      ? t(`transcriptions.status.${status}Progress`, { percent })
      : t(`transcriptions.status.${status}`);

  return (
    <span
      className={`inline-flex items-center px-2 py-0.5 rounded-full text-xs font-medium whitespace-nowrap ${STATUS_CLASSES[status]}`}
    >
      {label}
    </span>
  );
};
