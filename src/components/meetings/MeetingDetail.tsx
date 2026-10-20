import React, { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { convertFileSrc } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import { writeTextFile } from "@tauri-apps/plugin-fs";
import {
  ArrowLeft,
  Check,
  Copy,
  FileDown,
  FolderOpen,
  Pencil,
  RotateCcw,
  Trash2,
  XCircle,
} from "lucide-react";
import { toast } from "sonner";
import { commands, type MeetingSegment } from "@/bindings";
import { useModelName } from "@/hooks/useModelName";
import { useMeetingsStore } from "@/stores/meetingsStore";
import { Button } from "../ui/Button";
import { Dialog } from "../ui/Dialog";
import { copyToClipboard } from "../settings/history/clipboard";
import {
  formatDuration,
  formatMeetingDate,
  markdownFileName,
  sourceLabel,
} from "./format";
import {
  MeetingAudioPlayer,
  type MeetingAudioPlayerHandle,
} from "./MeetingAudioPlayer";
import { MeetingStatusBadge } from "./MeetingStatusBadge";
import { MeetingTranscript } from "./MeetingTranscript";

interface AudioSources {
  mic: string | null;
  system: string | null;
}

const EditableTitle: React.FC<{ id: string; title: string }> = ({
  id,
  title,
}) => {
  const { t } = useTranslation();
  const [editing, setEditing] = useState(false);
  const [value, setValue] = useState(title);

  useEffect(() => {
    if (!editing) setValue(title);
  }, [editing, title]);

  const commit = async () => {
    setEditing(false);
    const next = value.trim();
    if (!next || next === title) return;
    const result = await commands.renameMeeting(id, next);
    if (result.status !== "ok") {
      toast.error(t("meetings.errors.rename"), { description: result.error });
    }
  };

  if (editing) {
    return (
      <input
        autoFocus
        value={value}
        onChange={(e) => setValue(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter") e.currentTarget.blur();
          if (e.key === "Escape") {
            setValue(title);
            setEditing(false);
          }
        }}
        aria-label={t("meetings.detail.titleLabel")}
        className="w-full text-base font-semibold bg-transparent border-b border-logo-primary focus:outline-none"
      />
    );
  }

  return (
    <button
      type="button"
      onClick={() => setEditing(true)}
      className="group flex items-center gap-2 min-w-0 text-start cursor-text focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary rounded"
      aria-label={t("meetings.detail.rename")}
      title={t("meetings.detail.rename")}
    >
      <h2 className="text-base font-semibold truncate">{title}</h2>
      <Pencil
        className="w-3.5 h-3.5 shrink-0 text-text/40 group-hover:text-logo-primary"
        aria-hidden="true"
      />
    </button>
  );
};

const MetaItem: React.FC<{ label: string; value: React.ReactNode }> = ({
  label,
  value,
}) => (
  <div className="min-w-0">
    <dt className="text-xs text-text/50">{label}</dt>
    <dd className="text-sm truncate">{value}</dd>
  </div>
);

const ActionButton: React.FC<{
  onClick: () => void;
  label: string;
  icon: React.ReactNode;
  disabled?: boolean;
  variant?: "secondary" | "danger-ghost";
}> = ({ onClick, label, icon, disabled, variant = "secondary" }) => (
  <Button
    variant={variant}
    size="sm"
    onClick={onClick}
    disabled={disabled}
    className="flex items-center gap-1.5"
  >
    {icon}
    <span>{label}</span>
  </Button>
);

interface MeetingDetailProps {
  id: string;
  onBack: () => void;
}

export const MeetingDetail: React.FC<MeetingDetailProps> = ({ id, onBack }) => {
  const { t, i18n } = useTranslation();
  const modelName = useModelName();
  const meeting = useMeetingsStore((s) => s.meetings.find((m) => m.id === id));
  const progress = useMeetingsStore((s) => s.progress[id]);
  const cancelTranscription = useMeetingsStore((s) => s.cancelTranscription);
  const [segments, setSegments] = useState<MeetingSegment[] | null>(null);
  const [audio, setAudio] = useState<AudioSources | null>(null);
  const [currentMs, setCurrentMs] = useState<number | null>(null);
  const [copied, setCopied] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [busy, setBusy] = useState(false);
  const playerRef = useRef<MeetingAudioPlayerHandle>(null);

  const status = meeting?.status;
  const hasAudio = meeting?.has_audio ?? false;
  const stopped = status !== undefined && status !== "recording";

  // Reload the transcript whenever the meeting row changes (status, title,
  // new transcription…).
  useEffect(() => {
    if (!meeting) return;
    let cancelled = false;
    commands.getMeeting(meeting.id).then((result) => {
      if (cancelled) return;
      if (result.status === "ok") {
        setSegments(result.data?.segments ?? []);
      } else {
        console.error("Failed to load meeting:", result.error);
        setSegments([]);
      }
    });
    return () => {
      cancelled = true;
    };
  }, [meeting]);

  // The WAVs are complete once the recording has stopped.
  useEffect(() => {
    if (!hasAudio || !stopped) {
      setAudio(null);
      return;
    }
    let cancelled = false;
    commands.getMeetingAudioPaths(id).then((result) => {
      if (cancelled) return;
      if (result.status === "ok") {
        const src = (path: string | null) =>
          path ? convertFileSrc(path, "asset") : null;
        setAudio({
          mic: src(result.data.mic),
          system: src(result.data.system),
        });
      } else {
        setAudio(null);
      }
    });
    return () => {
      cancelled = true;
    };
  }, [id, hasAudio, stopped]);

  const handleTimeChange = useCallback((ms: number) => setCurrentMs(ms), []);

  if (!meeting) {
    return null;
  }

  // Same rendering as the `transcript.md` written by the transcription.
  const markdown = async () => {
    const result = await commands.getMeetingMarkdown(meeting.id);
    if (result.status !== "ok") throw new Error(result.error);
    return result.data;
  };
  const hasTranscript = (segments?.length ?? 0) > 0;
  const transcribing = meeting.status === "transcribing";
  const canRetry =
    meeting.has_audio &&
    (meeting.status === "done" ||
      meeting.status === "failed" ||
      meeting.status === "recovered" ||
      // A cancelled transcription is left pending until it is retried; the
      // backend never queues the same meeting twice.
      meeting.status === "pending");

  const handleCopy = async () => {
    let ok = false;
    try {
      ok = await copyToClipboard(await markdown());
    } catch (error) {
      console.error("Failed to render the transcript:", error);
    }
    if (!ok) {
      toast.error(t("meetings.errors.copy"));
      return;
    }
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  const handleExport = async () => {
    try {
      const path = await save({
        defaultPath: markdownFileName(meeting.title),
        filters: [{ name: "Markdown", extensions: ["md"] }],
      });
      if (!path) return;
      await writeTextFile(path, await markdown());
      toast.success(t("meetings.detail.exported"));
    } catch (error) {
      toast.error(t("meetings.errors.export"), { description: String(error) });
    }
  };

  const handleOpenFolder = async () => {
    const result = await commands.openMeetingFolder(meeting.id);
    if (result.status !== "ok") {
      toast.error(t("meetings.errors.openFolder"), {
        description: result.error,
      });
    }
  };

  const handleRetry = async () => {
    setBusy(true);
    const result = await commands.retryMeetingTranscription(meeting.id);
    setBusy(false);
    if (result.status !== "ok") {
      toast.error(t("meetings.errors.retry"), { description: result.error });
    }
  };

  const handleCancelTranscription = async () => {
    setBusy(true);
    const error = await cancelTranscription(meeting.id);
    setBusy(false);
    if (error) {
      toast.error(t("meetings.errors.cancelTranscription"), {
        description: error,
      });
    }
  };

  const handleDelete = async () => {
    setConfirmDelete(false);
    const result = await commands.deleteMeeting(meeting.id);
    if (result.status === "ok") {
      onBack();
    } else {
      toast.error(t("meetings.errors.delete"), { description: result.error });
    }
  };

  const showPlayer =
    audio !== null && (audio.mic !== null || audio.system !== null);

  let transcriptContent: React.ReactNode;
  if (segments === null) {
    transcriptContent = t("meetings.detail.loading");
  } else if (hasTranscript) {
    transcriptContent = (
      <MeetingTranscript
        segments={segments}
        currentMs={showPlayer ? currentMs : null}
        onSeek={
          showPlayer ? (ms) => playerRef.current?.seekAndPlay(ms) : undefined
        }
      />
    );
  } else {
    transcriptContent = t(`meetings.detail.emptyTranscript.${meeting.status}`);
  }

  return (
    <div className="space-y-4">
      <div className="flex items-start gap-2">
        <button
          type="button"
          onClick={onBack}
          className="p-1 rounded-md cursor-pointer text-text/60 hover:text-logo-primary focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary"
          aria-label={t("meetings.detail.back")}
          title={t("meetings.detail.back")}
        >
          <ArrowLeft className="w-5 h-5" />
        </button>
        <div className="flex-1 min-w-0 space-y-1">
          <EditableTitle id={meeting.id} title={meeting.title} />
          <MeetingStatusBadge
            status={meeting.status}
            progress={progress?.progress}
          />
        </div>
      </div>

      <dl className="grid grid-cols-2 sm:grid-cols-3 gap-3 px-1">
        <MetaItem
          label={t("meetings.detail.date")}
          value={formatMeetingDate(meeting.started_at, i18n.language)}
        />
        <MetaItem
          label={t("meetings.detail.duration")}
          value={
            meeting.duration_ms != null
              ? formatDuration(meeting.duration_ms)
              : "—"
          }
        />
        <MetaItem
          label={t("meetings.detail.source")}
          value={sourceLabel(t, meeting)}
        />
        <MetaItem
          label={t("meetings.detail.model")}
          value={modelName(meeting.model_id)}
        />
        <MetaItem
          label={t("meetings.detail.systemTrack")}
          value={
            meeting.has_system_audio
              ? t("meetings.detail.systemTrackYes")
              : t("meetings.detail.systemTrackNo")
          }
        />
        {!meeting.has_audio && (
          <MetaItem
            label={t("meetings.detail.audio")}
            value={t("meetings.detail.audioDeleted")}
          />
        )}
      </dl>

      {transcribing && (
        <div className="space-y-1 px-1">
          <div
            className="h-1.5 rounded-full bg-mid-gray/20 overflow-hidden"
            role="progressbar"
            aria-label={t("meetings.detail.progress")}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={
              progress ? Math.round(progress.progress * 100) : undefined
            }
          >
            <div
              className={`h-full bg-logo-primary transition-[width] duration-500 ${progress ? "" : "w-1/3 animate-pulse"}`}
              style={
                progress ? { width: `${progress.progress * 100}%` } : undefined
              }
            />
          </div>
          {progress?.eta_ms != null && (
            <p className="text-xs text-text/60">
              {t("meetings.detail.eta", {
                time: formatDuration(progress.eta_ms),
              })}
            </p>
          )}
        </div>
      )}

      {meeting.status === "failed" && meeting.error && (
        <p className="text-xs text-red-400 px-1 break-words">{meeting.error}</p>
      )}

      <div className="flex flex-wrap gap-2">
        <ActionButton
          onClick={handleCopy}
          disabled={!hasTranscript}
          label={t("meetings.actions.copy")}
          icon={
            copied ? (
              <Check className="w-4 h-4" aria-hidden="true" />
            ) : (
              <Copy className="w-4 h-4" aria-hidden="true" />
            )
          }
        />
        <ActionButton
          onClick={handleExport}
          disabled={!hasTranscript}
          label={t("meetings.actions.export")}
          icon={<FileDown className="w-4 h-4" aria-hidden="true" />}
        />
        <ActionButton
          onClick={handleOpenFolder}
          label={t("meetings.actions.openFolder")}
          icon={<FolderOpen className="w-4 h-4" aria-hidden="true" />}
        />
        {canRetry && (
          <ActionButton
            onClick={handleRetry}
            disabled={busy}
            label={t("meetings.actions.retry")}
            icon={<RotateCcw className="w-4 h-4" aria-hidden="true" />}
          />
        )}
        {transcribing && (
          <ActionButton
            onClick={handleCancelTranscription}
            disabled={busy}
            label={t("meetings.actions.cancelTranscription")}
            icon={<XCircle className="w-4 h-4" aria-hidden="true" />}
          />
        )}
        <ActionButton
          onClick={() => setConfirmDelete(true)}
          disabled={meeting.status === "recording"}
          variant="danger-ghost"
          label={t("meetings.actions.delete")}
          icon={<Trash2 className="w-4 h-4" aria-hidden="true" />}
        />
      </div>

      {showPlayer && (
        <div className="bg-background border border-mid-gray/20 rounded-lg px-4 py-3">
          <MeetingAudioPlayer
            ref={playerRef}
            micSrc={audio.mic}
            systemSrc={audio.system}
            onTimeChange={handleTimeChange}
          />
        </div>
      )}

      <section className="bg-background border border-mid-gray/20 rounded-lg p-2">
        <h3 className="text-xs font-medium text-mid-gray uppercase tracking-wide px-2 py-1">
          {t("meetings.detail.transcript")}
        </h3>
        {hasTranscript ? (
          transcriptContent
        ) : (
          <p className="px-2 py-3 text-sm text-text/60">{transcriptContent}</p>
        )}
      </section>

      <Dialog
        open={confirmDelete}
        onOpenChange={setConfirmDelete}
        title={t("meetings.delete.title")}
        description={t("meetings.delete.description", {
          title: meeting.title,
        })}
        closeLabel={t("meetings.delete.cancel")}
        footer={
          <>
            <Button variant="secondary" onClick={() => setConfirmDelete(false)}>
              {t("meetings.delete.cancel")}
            </Button>
            <Button variant="danger" onClick={handleDelete}>
              {t("meetings.delete.confirm")}
            </Button>
          </>
        }
      >
        {null}
      </Dialog>
    </div>
  );
};
