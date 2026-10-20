import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { ShieldCheck } from "lucide-react";
import {
  commands,
  type MeetingAudioRetention,
  type MeetingAutoStop,
  type MeetingDetectionApp,
} from "@/bindings";
import { useSettings } from "@/hooks/useSettings";
import { useOsType } from "@/hooks/useOsType";
import { Dropdown } from "../../ui/Dropdown";
import { SettingContainer } from "../../ui/SettingContainer";
import { SettingsGroup } from "../../ui/SettingsGroup";
import { ToggleSwitch } from "../../ui/ToggleSwitch";
import { ShortcutInput } from "../ShortcutInput";
import { MeetingCaptureTest } from "../debug/MeetingCaptureTest";
import { SpeakerDetectionSettings } from "./SpeakerDetectionSettings";

const AUTO_STOP_MODES: MeetingAutoStop[] = ["ask", "auto", "never"];
const RETENTIONS: MeetingAudioRetention[] = [
  "forever",
  "days_7",
  "days_30",
  "days_90",
  "delete_after_transcription",
];

/** Detection, per-app toggles, recording options and privacy reminder. */
export const MeetingsSettings: React.FC = () => {
  const { t } = useTranslation();
  const osType = useOsType();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  const [apps, setApps] = useState<MeetingDetectionApp[]>([]);
  const [detectionSupported, setDetectionSupported] = useState(true);

  useEffect(() => {
    commands.getMeetingDetectionApps().then(setApps);
    commands.isMeetingDetectionSupported().then(setDetectionSupported);
  }, []);

  const toggle = (
    key:
      | "meeting_detection_enabled"
      | "meeting_auto_start"
      | "meeting_prompt_sound"
      | "meeting_capture_system_audio"
      | "show_meeting_indicator"
      | "meeting_consent_reminder",
    fallback: boolean,
  ) => ({
    checked: getSetting(key) ?? fallback,
    onChange: (enabled: boolean) => updateSetting(key, enabled),
    isUpdating: isUpdating(key),
    label: t(`settings.meetings.${key}.label`),
    description: t(`settings.meetings.${key}.description`),
    grouped: true,
  });

  const detectionEnabled = getSetting("meeting_detection_enabled") ?? true;
  const overrides = getSetting("meeting_detection_apps") ?? {};
  const setAppEnabled = (id: string, enabled: boolean) =>
    updateSetting("meeting_detection_apps", { ...overrides, [id]: enabled });

  return (
    <div className="space-y-6">
      <SettingsGroup
        title={t("settings.meetings.detection.title")}
        description={
          detectionSupported
            ? t("settings.meetings.detection.description")
            : t("settings.meetings.detection.unsupported")
        }
      >
        <ToggleSwitch {...toggle("meeting_detection_enabled", true)} />
        <ToggleSwitch
          {...toggle("meeting_auto_start", false)}
          disabled={!detectionEnabled}
        />
        <SettingContainer
          title={t("settings.meetings.meeting_auto_stop.label")}
          description={t("settings.meetings.meeting_auto_stop.description")}
          grouped={true}
          disabled={!detectionEnabled}
        >
          <Dropdown
            options={AUTO_STOP_MODES.map((mode) => ({
              value: mode,
              label: t(`settings.meetings.meeting_auto_stop.options.${mode}`),
            }))}
            selectedValue={getSetting("meeting_auto_stop") ?? "ask"}
            onSelect={(value) =>
              updateSetting("meeting_auto_stop", value as MeetingAutoStop)
            }
            disabled={!detectionEnabled || isUpdating("meeting_auto_stop")}
          />
        </SettingContainer>
        <ToggleSwitch
          {...toggle("meeting_prompt_sound", true)}
          disabled={!detectionEnabled}
        />
      </SettingsGroup>

      <SettingsGroup
        title={t("settings.meetings.apps.title")}
        description={t("settings.meetings.apps.description")}
      >
        {apps.map((app) => (
          <ToggleSwitch
            key={app.id}
            checked={overrides[app.id] ?? app.default_enabled}
            onChange={(enabled) => setAppEnabled(app.id, enabled)}
            isUpdating={isUpdating("meeting_detection_apps")}
            disabled={!detectionEnabled}
            label={app.display_name}
            description={
              app.kind === "browser"
                ? t("settings.meetings.apps.browser")
                : t("settings.meetings.apps.native", {
                    app: app.display_name,
                  })
            }
            grouped={true}
          />
        ))}
      </SettingsGroup>

      <SettingsGroup title={t("settings.meetings.recording.title")}>
        <ToggleSwitch {...toggle("meeting_capture_system_audio", true)} />
        <ToggleSwitch {...toggle("show_meeting_indicator", true)} />
        <ToggleSwitch {...toggle("meeting_consent_reminder", true)} />
        <SettingContainer
          title={t("settings.meetings.meeting_audio_retention.label")}
          description={t(
            "settings.meetings.meeting_audio_retention.description",
          )}
          grouped={true}
        >
          <Dropdown
            options={RETENTIONS.map((retention) => ({
              value: retention,
              label: t(
                `settings.meetings.meeting_audio_retention.options.${retention}`,
              ),
            }))}
            selectedValue={getSetting("meeting_audio_retention") ?? "days_30"}
            onSelect={(value) =>
              updateSetting(
                "meeting_audio_retention",
                value as MeetingAudioRetention,
              )
            }
            disabled={isUpdating("meeting_audio_retention")}
          />
        </SettingContainer>
        <ShortcutInput shortcutId="toggle_meeting_recording" grouped={true} />
        {osType === "macos" && <MeetingCaptureTest />}
      </SettingsGroup>

      <SpeakerDetectionSettings />

      <div className="px-4 flex items-start gap-2 text-xs text-mid-gray">
        <ShieldCheck className="w-4 h-4 shrink-0" aria-hidden="true" />
        <p>{t("settings.meetings.privacy")}</p>
      </div>
    </div>
  );
};
