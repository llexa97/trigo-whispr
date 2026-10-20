import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { ArrowLeft, Search, Settings } from "lucide-react";
import type { MeetingAudioRetention } from "@/bindings";
import { useSettings } from "@/hooks/useSettings";
import { useTranscriptionsStore } from "@/stores/transcriptionsStore";
import { Button } from "../ui/Button";
import { Dropdown } from "../ui/Dropdown";
import { SettingContainer } from "../ui/SettingContainer";
import { SettingsGroup } from "../ui/SettingsGroup";
import { DropZone } from "./DropZone";
import { TranscriptionDetail } from "./TranscriptionDetail";
import { TranscriptionList } from "./TranscriptionList";

const RETENTIONS: MeetingAudioRetention[] = [
  "forever",
  "days_7",
  "days_30",
  "days_90",
  "delete_after_transcription",
];

const TranscriptionsSettings: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating } = useSettings();
  return (
    <SettingsGroup title={t("transcriptions.settings.title")}>
      <SettingContainer
        title={t("transcriptions.settings.retention.label")}
        description={t("transcriptions.settings.retention.description")}
        grouped={true}
      >
        <Dropdown
          options={RETENTIONS.map((retention) => ({
            value: retention,
            label: t(
              `settings.meetings.meeting_audio_retention.options.${retention}`,
            ),
          }))}
          selectedValue={
            getSetting("file_transcription_audio_retention") ?? "days_30"
          }
          onSelect={(value) =>
            updateSetting(
              "file_transcription_audio_retention",
              value as MeetingAudioRetention,
            )
          }
          disabled={isUpdating("file_transcription_audio_retention")}
        />
      </SettingContainer>
    </SettingsGroup>
  );
};

/** « Transcriptions » section: drop zone, searchable list and detail of
 * the imported files. */
export const TranscriptionsSection: React.FC = () => {
  const { t } = useTranslation();
  const initialize = useTranscriptionsStore((s) => s.initialize);
  const query = useTranscriptionsStore((s) => s.query);
  const setQuery = useTranscriptionsStore((s) => s.setQuery);
  const loading = useTranscriptionsStore((s) => s.loading);
  const count = useTranscriptionsStore((s) => s.transcriptions.length);
  const selectedId = useTranscriptionsStore((s) => s.selectedId);
  const select = useTranscriptionsStore((s) => s.select);
  const [showSettings, setShowSettings] = useState(false);

  useEffect(() => {
    initialize();
  }, [initialize]);

  const searching = query.trim().length > 0;

  let content: React.ReactNode;
  if (loading && count === 0) {
    content = (
      <p className="px-4 py-3 text-center text-sm text-text/60">
        {t("transcriptions.loading")}
      </p>
    );
  } else if (count === 0 && searching) {
    content = (
      <p className="px-4 py-3 text-center text-sm text-text/60">
        {t("transcriptions.search.noResults")}
      </p>
    );
  } else if (count === 0) {
    content = (
      <div className="px-6 py-6 text-center space-y-1">
        <p className="text-sm font-medium">{t("transcriptions.empty.title")}</p>
        <p className="text-xs text-text/60">
          {t("transcriptions.empty.description")}
        </p>
      </div>
    );
  } else {
    content = <TranscriptionList onOpen={select} />;
  }

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <div className="px-4 flex items-start gap-3">
        <div className="flex-1 min-w-0">
          {!showSettings && !selectedId && <DropZone />}
        </div>
        <Button
          variant="secondary"
          size="sm"
          onClick={() => setShowSettings((shown) => !shown)}
          className="flex items-center gap-1.5 shrink-0"
          aria-pressed={showSettings}
        >
          {showSettings ? (
            <ArrowLeft className="w-4 h-4" aria-hidden="true" />
          ) : (
            <Settings className="w-4 h-4" aria-hidden="true" />
          )}
          {showSettings
            ? t("transcriptions.settings.back")
            : t("transcriptions.settings.open")}
        </Button>
      </div>

      {showSettings ? (
        <TranscriptionsSettings />
      ) : selectedId ? (
        <div className="px-4">
          <TranscriptionDetail id={selectedId} onBack={() => select(null)} />
        </div>
      ) : (
        <div className="space-y-2">
          <div className="px-4 flex items-center justify-between gap-4">
            <h2 className="text-xs font-medium text-mid-gray uppercase tracking-wide">
              {t("transcriptions.title")}
            </h2>
            <label className="relative flex items-center w-56">
              <Search
                className="absolute start-2 w-3.5 h-3.5 text-text/40 pointer-events-none"
                aria-hidden="true"
              />
              <input
                type="search"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder={t("transcriptions.search.placeholder")}
                aria-label={t("transcriptions.search.placeholder")}
                className="w-full ps-7 pe-2 py-1 text-sm bg-mid-gray/10 border border-mid-gray/20 rounded-md focus:outline-none focus:border-logo-primary"
              />
            </label>
          </div>
          <div className="bg-background border border-mid-gray/20 rounded-lg overflow-visible">
            {content}
          </div>
        </div>
      )}
    </div>
  );
};
