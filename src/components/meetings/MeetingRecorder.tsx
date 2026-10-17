import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { AlertTriangle, Circle, Loader2, Square } from "lucide-react";
import { toast } from "sonner";
import { commands, type CaptureWarning } from "@/bindings";
import { useOsType } from "@/hooks/useOsType";
import { useMeetingsStore } from "@/stores/meetingsStore";
import { Button } from "../ui/Button";
import { formatDuration, levelToMeter } from "./format";

const LevelMeter: React.FC<{ label: string; level: number }> = ({
  label,
  level,
}) => {
  const value = Math.round(levelToMeter(level) * 100);
  return (
    <div className="flex items-center gap-2 min-w-0">
      <span className="text-xs text-text/60 w-16 shrink-0 truncate">
        {label}
      </span>
      <div
        className="h-1.5 flex-1 rounded-full bg-mid-gray/20 overflow-hidden"
        role="meter"
        aria-label={label}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={value}
      >
        <div
          className="h-full bg-logo-primary transition-[width] duration-300"
          style={{ width: `${value}%` }}
        />
      </div>
    </div>
  );
};

const WarningRow: React.FC<{ warning: CaptureWarning }> = ({ warning }) => {
  const { t } = useTranslation();
  const osType = useOsType();
  const showPrivacyAction =
    warning === "mic_unavailable" && osType === "windows";

  return (
    <li className="flex items-start gap-2 text-xs text-warning">
      <AlertTriangle className="w-4 h-4 shrink-0 mt-px" aria-hidden="true" />
      <span className="flex-1">{t(`meetings.warnings.${warning}`)}</span>
      {showPrivacyAction && (
        <Button
          variant="warning"
          size="sm"
          onClick={() => commands.openMicrophonePrivacySettings()}
        >
          {t("meetings.warnings.openPrivacySettings")}
        </Button>
      )}
    </li>
  );
};

/** Start/stop button, timer, level meters and capture warnings. */
export const MeetingRecorder: React.FC = () => {
  const { t } = useTranslation();
  const recorder = useMeetingsStore((s) => s.recorder);
  const startMeeting = useMeetingsStore((s) => s.startMeeting);
  const stopMeeting = useMeetingsStore((s) => s.stopMeeting);
  const [pending, setPending] = useState(false);

  const recording = recorder.state === "recording";
  const stopping = recorder.state === "stopping";

  const handleClick = async () => {
    setPending(true);
    const error = recording ? await stopMeeting() : await startMeeting();
    setPending(false);
    if (error) {
      toast.error(
        recording ? t("meetings.errors.stop") : t("meetings.errors.start"),
        { description: error },
      );
    }
  };

  return (
    <div className="space-y-3">
      <div className="flex items-center gap-3">
        <Button
          variant={recording ? "danger" : "primary"}
          onClick={handleClick}
          disabled={pending || stopping}
          className="flex items-center gap-2"
        >
          {stopping ? (
            <Loader2 className="w-4 h-4 animate-spin" aria-hidden="true" />
          ) : recording ? (
            <Square className="w-4 h-4" fill="currentColor" aria-hidden />
          ) : (
            <Circle className="w-4 h-4" fill="currentColor" aria-hidden />
          )}
          <span className="tabular-nums">
            {stopping
              ? t("meetings.recorder.stopping")
              : recording
                ? t("meetings.recorder.stop", {
                    time: formatDuration(recorder.elapsed_ms),
                  })
                : t("meetings.recorder.start")}
          </span>
        </Button>
        {recording && (
          <span className="flex items-center gap-1.5 text-xs text-red-400">
            <span className="w-2 h-2 rounded-full bg-red-500 animate-pulse" />
            {t("meetings.recorder.recording")}
          </span>
        )}
      </div>

      {recording && (
        <div className="grid gap-1.5 sm:grid-cols-2 sm:gap-4">
          <LevelMeter
            label={t("meetings.recorder.micLevel")}
            level={recorder.mic_level}
          />
          <LevelMeter
            label={t("meetings.recorder.systemLevel")}
            level={recorder.system_level}
          />
        </div>
      )}

      {recording && recorder.warnings.length > 0 && (
        <ul className="space-y-1.5" aria-live="polite">
          {recorder.warnings.map((warning) => (
            <WarningRow key={warning} warning={warning} />
          ))}
        </ul>
      )}
    </div>
  );
};
