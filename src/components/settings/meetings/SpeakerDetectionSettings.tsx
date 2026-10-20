import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { commands, events, type DiarizationModelStatus } from "@/bindings";
import { useSettings } from "@/hooks/useSettings";
import { Button } from "../../ui/Button";
import { SettingContainer } from "../../ui/SettingContainer";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { ToggleSwitch } from "../../ui/ToggleSwitch";

const toMegabytes = (bytes: number) => Math.round(bytes / (1024 * 1024));

/**
 * Speaker detection for meetings and imported files: the toggle, and its
 * model, downloaded on request.
 */
export const SpeakerDetectionSettings: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const [status, setStatus] = useState<DiarizationModelStatus | null>(null);
  const [percent, setPercent] = useState<number | null>(null);

  useEffect(() => {
    commands.getDiarizationModelStatus().then(setStatus);
    const changed = events.diarizationModelChanged.listen((event) => {
      setStatus(event.payload.status);
      if (!event.payload.status.downloading) setPercent(null);
      if (event.payload.error) {
        toast.error(t("settings.meetings.speakerDetection.downloadFailed"), {
          description: event.payload.error,
        });
      }
    });
    const progress = events.diarizationModelProgress.listen((event) => {
      const { downloaded, total } = event.payload;
      if (total > 0) setPercent(Math.floor((downloaded / total) * 100));
    });
    return () => {
      changed.then((unlisten) => unlisten());
      progress.then((unlisten) => unlisten());
    };
  }, [t]);

  const enabled = getSetting("speaker_detection_enabled") ?? true;
  const size = toMegabytes(status?.size_bytes ?? 0);

  const download = async () => {
    // Failures arrive as a `diarizationModelChanged` event.
    await commands.downloadDiarizationModel();
  };

  const remove = async () => {
    const result = await commands.deleteDiarizationModel();
    if (result.status !== "ok") {
      toast.error(t("settings.meetings.speakerDetection.deleteFailed"), {
        description: result.error,
      });
    }
  };

  let modelAction: React.ReactNode = null;
  if (status?.downloading) {
    modelAction = (
      <div className="flex items-center gap-2">
        <span className="text-xs text-text/60 tabular-nums">
          {t("settings.meetings.speakerDetection.downloading", {
            percent: percent ?? 0,
          })}
        </span>
        <Button
          variant="secondary"
          size="sm"
          onClick={() => commands.cancelDiarizationModelDownload()}
        >
          {t("settings.meetings.speakerDetection.cancel")}
        </Button>
      </div>
    );
  } else if (status?.downloaded) {
    modelAction = (
      <Button variant="danger-ghost" size="sm" onClick={remove}>
        {t("settings.meetings.speakerDetection.delete")}
      </Button>
    );
  } else if (status) {
    modelAction = (
      <Button variant="secondary" size="sm" onClick={download}>
        {t("settings.meetings.speakerDetection.download", { size })}
      </Button>
    );
  }

  return (
    <SettingsGroup
      title={t("settings.meetings.speakerDetection.title")}
      description={t("settings.meetings.speakerDetection.description")}
    >
      <ToggleSwitch
        checked={enabled}
        onChange={(value) => updateSetting("speaker_detection_enabled", value)}
        isUpdating={isUpdating("speaker_detection_enabled")}
        label={t("settings.meetings.speaker_detection_enabled.label")}
        description={t(
          "settings.meetings.speaker_detection_enabled.description",
        )}
        grouped={true}
      />
      <SettingContainer
        title={t("settings.meetings.speakerDetection.model")}
        description={
          status?.downloaded
            ? t("settings.meetings.speakerDetection.modelReady", { size })
            : t("settings.meetings.speakerDetection.modelMissing")
        }
        grouped={true}
        disabled={!enabled}
      >
        {modelAction}
      </SettingContainer>
    </SettingsGroup>
  );
};
