import React, { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { convertFileSrc } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import { writeTextFile } from "@tauri-apps/plugin-fs";
import {
  ArrowLeft,
  Check,
  Copy,
  Cpu,
  FileDown,
  FolderOpen,
  RotateCcw,
  Trash2,
  XCircle,
} from "lucide-react";
import { toast } from "sonner";
import { commands, type FileTranscriptionSegment } from "@/bindings";
import { useSettings } from "@/hooks/useSettings";
import { useModelName } from "@/hooks/useModelName";
import { getLanguageLabel } from "@/lib/constants/languages";
import { useTranscriptionsStore } from "@/stores/transcriptionsStore";
import { Button } from "../ui/Button";
import { Dialog } from "../ui/Dialog";
import { copyToClipboard } from "../settings/history/clipboard";
import {
  formatDuration,
  formatMeetingDate,
  formatTimestamp,
} from "../meetings/format";
import {
  MeetingAudioPlayer,
  type MeetingAudioPlayerHandle,
} from "../meetings/MeetingAudioPlayer";
import { exportFileName, renderExport, type ExportFormat } from "./format";
import { LanguageOverride } from "./LanguageOverride";
import { TranscriptionStatusBadge } from "./TranscriptionStatusBadge";

const EXPORT_FORMATS: { format: ExportFormat; name: string }[] = [
  { format: "txt", name: "Text" },
  { format: "md", name: "Markdown" },
  { format: "srt", name: "SubRip" },
];

const MetaItem: React.FC<{ label: string; value: React.ReactNode }> = ({
  label,
  value,
}) => (
  <div className="min-w-0">
    <dt className="text-xs text-text/50">{label}</dt>
    <dd
      className="text-sm truncate"
      title={typeof value === "string" ? value : undefined}
    >
      {value}
    </dd>
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

const Transcript: React.FC<{
  segments: FileTranscriptionSegment[];
  currentMs: number | null;
  onSeek?: (ms: number) => void;
}> = ({ segments, currentMs, onSeek }) => {
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
            className={`flex items-start gap-2 rounded-md px-2 py-1.5 transition-colors ${active ? "bg-logo-primary/15" : ""}`}
          >
            {onSeek ? (
              <button
                type="button"
                onClick={() => onSeek(segment.start_ms)}
                className="text-xs font-mono text-text/60 hover:text-logo-primary cursor-pointer pt-0.5 shrink-0 focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary rounded"
                aria-label={t("transcriptions.detail.playFrom", { time })}
              >
                {`[${time}]`}
              </button>
            ) : (
              <span className="text-xs font-mono text-text/60 pt-0.5 shrink-0">
                {`[${time}]`}
              </span>
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

interface TranscriptionDetailProps {
  id: string;
  onBack: () => void;
}

export const TranscriptionDetail: React.FC<TranscriptionDetailProps> = ({
  id,
  onBack,
}) => {
  const { t, i18n } = useTranslation();
  const modelName = useModelName();
  const { getSetting } = useSettings();
  const transcription = useTranscriptionsStore((s) =>
    s.transcriptions.find((x) => x.id === id),
  );
  const progress = useTranscriptionsStore((s) => s.progress[id]);
  const navigate = useTranscriptionsStore((s) => s.navigate);
  const [segments, setSegments] = useState<FileTranscriptionSegment[] | null>(
    null,
  );
  const [audioSrc, setAudioSrc] = useState<string | null>(null);
  const [currentMs, setCurrentMs] = useState<number | null>(null);
  const [copied, setCopied] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [retryOpen, setRetryOpen] = useState(false);
  const [retryLanguage, setRetryLanguage] = useState<string | null>(null);
  const [retryPostProcess, setRetryPostProcess] = useState(false);
  const [busy, setBusy] = useState(false);
  const playerRef = useRef<MeetingAudioPlayerHandle>(null);
  const postProcessAvailable = getSetting("post_process_enabled") ?? false;

  const hasAudio = transcription?.has_audio ?? false;
  const status = transcription?.status;

  // Reload the transcript whenever the row changes (status, re-run…).
  useEffect(() => {
    if (!transcription) return;
    let cancelled = false;
    commands.getFileTranscription(transcription.id).then((result) => {
      if (cancelled) return;
      if (result.status === "ok") {
        setSegments(result.data?.segments ?? []);
      } else {
        console.error("Failed to load the transcription:", result.error);
        setSegments([]);
      }
    });
    return () => {
      cancelled = true;
    };
  }, [transcription]);

  // The WAV is complete once decoding is over.
  useEffect(() => {
    if (!hasAudio || status === "decoding") {
      setAudioSrc(null);
      return;
    }
    let cancelled = false;
    commands.getFileTranscriptionAudioPath(id).then((result) => {
      if (cancelled) return;
      setAudioSrc(
        result.status === "ok" && result.data
          ? convertFileSrc(result.data, "asset")
          : null,
      );
    });
    return () => {
      cancelled = true;
    };
  }, [id, hasAudio, status]);

  const handleTimeChange = useCallback((ms: number) => setCurrentMs(ms), []);

  if (!transcription) return null;

  const running =
    transcription.status === "decoding" ||
    transcription.status === "transcribing";
  const queued = transcription.status === "pending";
  const hasTranscript = (segments?.length ?? 0) > 0;
  const labels = {
    transcript: t("transcriptions.detail.transcript"),
    postProcessed: t("transcriptions.detail.postProcessed"),
  };

  const handleCopy = async () => {
    const text =
      transcription.post_processed_text ??
      renderExport("txt", transcription, segments ?? [], labels);
    if (!(await copyToClipboard(text))) {
      toast.error(t("transcriptions.errors.copy"));
      return;
    }
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  const handleExport = async (format: ExportFormat, name: string) => {
    try {
      const path = await save({
        defaultPath: exportFileName(transcription.file_name, format),
        filters: [{ name, extensions: [format] }],
      });
      if (!path) return;
      await writeTextFile(
        path,
        renderExport(format, transcription, segments ?? [], labels),
      );
      toast.success(t("transcriptions.detail.exported"));
    } catch (error) {
      toast.error(t("transcriptions.errors.export"), {
        description: String(error),
      });
    }
  };

  const handleOpenFolder = async () => {
    const result = await commands.openFileTranscriptionFolder(id);
    if (result.status !== "ok") {
      toast.error(t("transcriptions.errors.openFolder"), {
        description: result.error,
      });
    }
  };

  const openRetry = () => {
    setRetryLanguage(transcription.language);
    setRetryPostProcess(transcription.post_process);
    setRetryOpen(true);
  };

  const handleRetry = async () => {
    setRetryOpen(false);
    setBusy(true);
    const result = await commands.retryFileTranscription(id, {
      language: retryLanguage,
      post_process: retryPostProcess && postProcessAvailable,
    });
    setBusy(false);
    if (result.status !== "ok") {
      toast.error(t("transcriptions.errors.retry"), {
        description: result.error,
      });
    }
  };

  const handleCancel = async () => {
    setBusy(true);
    const result = await commands.cancelFileTranscription(id);
    setBusy(false);
    if (result.status !== "ok") {
      toast.error(t("transcriptions.errors.cancel"), {
        description: result.error,
      });
    }
  };

  const handleDelete = async () => {
    setConfirmDelete(false);
    setBusy(true);
    const result = await commands.deleteFileTranscription(id);
    setBusy(false);
    if (result.status === "ok") {
      onBack();
    } else {
      toast.error(t("transcriptions.errors.delete"), {
        description: result.error,
      });
    }
  };

  // Once transcribed, the language the model actually used: a forced one it
  // ignored (Parakeet) is not shown as applied.
  const transcribed = transcription.model_id != null;
  const language = transcribed
    ? transcription.detected_language
    : transcription.language;

  let emptyText: string;
  if (segments === null) {
    emptyText = t("transcriptions.detail.loading");
  } else {
    emptyText = t(`transcriptions.detail.empty.${transcription.status}`);
  }

  const stage = progress?.stage ?? transcription.status;
  const percent =
    progress?.progress != null ? progress.progress * 100 : undefined;

  return (
    <div className="space-y-4">
      <div className="flex items-start gap-2">
        <button
          type="button"
          onClick={onBack}
          className="p-1 rounded-md cursor-pointer text-text/60 hover:text-logo-primary focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary"
          aria-label={t("transcriptions.detail.back")}
          title={t("transcriptions.detail.back")}
        >
          <ArrowLeft className="w-5 h-5" />
        </button>
        <div className="flex-1 min-w-0 space-y-1">
          <h2
            className="text-base font-semibold truncate"
            title={transcription.file_name}
          >
            {transcription.file_name}
          </h2>
          <TranscriptionStatusBadge
            status={transcription.status}
            progress={progress}
          />
        </div>
      </div>

      <dl className="grid grid-cols-2 sm:grid-cols-3 gap-3 px-1">
        <MetaItem
          label={t("transcriptions.detail.date")}
          value={formatMeetingDate(transcription.created_at, i18n.language)}
        />
        <MetaItem
          label={t("transcriptions.detail.duration")}
          value={
            transcription.duration_ms != null
              ? formatDuration(transcription.duration_ms)
              : "—"
          }
        />
        <MetaItem
          label={t("transcriptions.detail.model")}
          value={modelName(transcription.model_id)}
        />
        <MetaItem
          label={t("transcriptions.detail.language")}
          value={
            language
              ? (getLanguageLabel(language) ?? language)
              : transcribed
                ? t("transcriptions.detail.languageAuto")
                : t("transcriptions.options.languageFromSettings")
          }
        />
        <MetaItem
          label={t("transcriptions.detail.audio")}
          value={
            hasAudio
              ? t("transcriptions.detail.audioKept")
              : t("transcriptions.detail.audioNotKept")
          }
        />
        <MetaItem
          label={t("transcriptions.detail.source")}
          value={transcription.source_path}
        />
      </dl>

      {running && (
        <div className="space-y-1 px-1">
          <div
            className="h-1.5 rounded-full bg-mid-gray/20 overflow-hidden"
            role="progressbar"
            aria-label={t(`transcriptions.status.${stage}`)}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={
              percent !== undefined ? Math.round(percent) : undefined
            }
          >
            <div
              className={`h-full bg-logo-primary transition-[width] duration-500 ${percent !== undefined ? "" : "w-1/3 animate-pulse"}`}
              style={
                percent !== undefined ? { width: `${percent}%` } : undefined
              }
            />
          </div>
          <p className="text-xs text-text/60">
            {t(`transcriptions.status.${stage}`)}
            {progress?.eta_ms != null &&
              ` · ${t("transcriptions.detail.eta", {
                time: formatDuration(progress.eta_ms),
              })}`}
          </p>
        </div>
      )}

      {transcription.status === "failed" && transcription.error_code && (
        <div className="px-1 space-y-1">
          <p className="text-sm text-red-400 break-words">
            {t(`transcriptions.errors.${transcription.error_code}`)}
          </p>
          {transcription.error && (
            <p className="text-xs text-text/50 break-words">
              {transcription.error}
            </p>
          )}
          {transcription.error_code === "no_model" && (
            <Button
              variant="secondary"
              size="sm"
              onClick={() => navigate("models")}
              className="flex items-center gap-1.5"
            >
              <Cpu className="w-4 h-4" aria-hidden="true" />
              {t("transcriptions.errors.openModels")}
            </Button>
          )}
        </div>
      )}

      <div className="flex flex-wrap gap-2">
        <ActionButton
          onClick={handleCopy}
          disabled={!hasTranscript}
          label={t("transcriptions.actions.copy")}
          icon={
            copied ? (
              <Check className="w-4 h-4" aria-hidden="true" />
            ) : (
              <Copy className="w-4 h-4" aria-hidden="true" />
            )
          }
        />
        {EXPORT_FORMATS.map(({ format, name }) => (
          <ActionButton
            key={format}
            onClick={() => handleExport(format, name)}
            disabled={!hasTranscript}
            label={t("transcriptions.actions.export", {
              format: format.toUpperCase(),
            })}
            icon={<FileDown className="w-4 h-4" aria-hidden="true" />}
          />
        ))}
        {!running && !queued && (
          <ActionButton
            onClick={openRetry}
            disabled={busy}
            label={t("transcriptions.actions.retry")}
            icon={<RotateCcw className="w-4 h-4" aria-hidden="true" />}
          />
        )}
        {(running || queued) && (
          <ActionButton
            onClick={handleCancel}
            disabled={busy}
            label={t("transcriptions.actions.cancel")}
            icon={<XCircle className="w-4 h-4" aria-hidden="true" />}
          />
        )}
        <ActionButton
          onClick={handleOpenFolder}
          label={t("transcriptions.actions.openFolder")}
          icon={<FolderOpen className="w-4 h-4" aria-hidden="true" />}
        />
        <ActionButton
          onClick={() => setConfirmDelete(true)}
          disabled={busy}
          variant="danger-ghost"
          label={t("transcriptions.actions.delete")}
          icon={<Trash2 className="w-4 h-4" aria-hidden="true" />}
        />
      </div>

      {audioSrc && (
        <div className="bg-background border border-mid-gray/20 rounded-lg px-4 py-3">
          <MeetingAudioPlayer
            ref={playerRef}
            micSrc={audioSrc}
            systemSrc={null}
            onTimeChange={handleTimeChange}
          />
        </div>
      )}

      {transcription.post_processed_text && (
        <section className="bg-background border border-mid-gray/20 rounded-lg p-2">
          <h3 className="text-xs font-medium text-mid-gray uppercase tracking-wide px-2 py-1">
            {labels.postProcessed}
          </h3>
          <p className="px-2 py-1.5 text-sm text-text/90 select-text whitespace-pre-wrap break-words">
            {transcription.post_processed_text}
          </p>
        </section>
      )}

      <section className="bg-background border border-mid-gray/20 rounded-lg p-2">
        <h3 className="text-xs font-medium text-mid-gray uppercase tracking-wide px-2 py-1">
          {labels.transcript}
        </h3>
        {hasTranscript && segments ? (
          <Transcript
            segments={segments}
            currentMs={audioSrc ? currentMs : null}
            onSeek={
              audioSrc ? (ms) => playerRef.current?.seekAndPlay(ms) : undefined
            }
          />
        ) : (
          <p className="px-2 py-3 text-sm text-text/60">{emptyText}</p>
        )}
      </section>

      <Dialog
        open={retryOpen}
        onOpenChange={setRetryOpen}
        title={t("transcriptions.retry.title")}
        description={t("transcriptions.retry.description")}
        closeLabel={t("transcriptions.retry.cancel")}
        footer={
          <>
            <Button variant="secondary" onClick={() => setRetryOpen(false)}>
              {t("transcriptions.retry.cancel")}
            </Button>
            <Button variant="primary" onClick={handleRetry}>
              {t("transcriptions.retry.confirm")}
            </Button>
          </>
        }
      >
        <div className="space-y-3">
          <label className="flex items-center justify-between gap-3 text-sm">
            <span>{t("transcriptions.options.language")}</span>
            <LanguageOverride
              value={retryLanguage}
              onChange={setRetryLanguage}
            />
          </label>
          {postProcessAvailable && (
            <label className="flex items-center gap-2 text-sm cursor-pointer">
              <input
                type="checkbox"
                checked={retryPostProcess}
                onChange={(e) => setRetryPostProcess(e.target.checked)}
                className="accent-logo-primary"
              />
              <span>{t("transcriptions.options.postProcess")}</span>
            </label>
          )}
          <p className="text-xs text-text/60">
            {t("transcriptions.retry.model")}
          </p>
        </div>
      </Dialog>

      <Dialog
        open={confirmDelete}
        onOpenChange={setConfirmDelete}
        title={t("transcriptions.delete.title")}
        description={t("transcriptions.delete.description", {
          name: transcription.file_name,
        })}
        closeLabel={t("transcriptions.delete.cancel")}
        footer={
          <>
            <Button variant="secondary" onClick={() => setConfirmDelete(false)}>
              {t("transcriptions.delete.cancel")}
            </Button>
            <Button variant="danger" onClick={handleDelete}>
              {t("transcriptions.delete.confirm")}
            </Button>
          </>
        }
      >
        {null}
      </Dialog>
    </div>
  );
};
