import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { open } from "@tauri-apps/plugin-dialog";
import { FileAudio, FolderOpen } from "lucide-react";
import { toast } from "sonner";
import { useSettings } from "@/hooks/useSettings";
import { useTranscriptionsStore } from "@/stores/transcriptionsStore";
import { Button } from "../ui/Button";
import { reportImport } from "./importReport";
import { LanguageOverride } from "./LanguageOverride";

/**
 * The top of the Transcriptions page: where files are dropped (the whole
 * window accepts them; this zone lights up while they are dragged), a native
 * picker as the alternative, and the options the next imports get.
 */
export const DropZone: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting } = useSettings();
  const drag = useTranscriptionsStore((s) => s.drag);
  const extensions = useTranscriptionsStore((s) => s.extensions);
  const options = useTranscriptionsStore((s) => s.options);
  const setOptions = useTranscriptionsStore((s) => s.setOptions);
  const importPaths = useTranscriptionsStore((s) => s.importPaths);
  const navigate = useTranscriptionsStore((s) => s.navigate);
  const [picking, setPicking] = useState(false);
  const postProcessAvailable = getSetting("post_process_enabled") ?? false;

  const refused = drag.active && drag.supported === 0;
  const zoneClasses = refused
    ? "border-red-400 bg-red-500/10"
    : drag.active
      ? "border-logo-primary bg-logo-primary/15"
      : "border-mid-gray/30 bg-mid-gray/5";

  let hint = t("transcriptions.drop.hint");
  if (refused) {
    hint = t("transcriptions.drop.refused");
  } else if (drag.active) {
    hint = t("transcriptions.drop.release", { count: drag.supported });
  }

  const handlePick = async () => {
    setPicking(true);
    try {
      const selected = await open({
        multiple: true,
        directory: false,
        filters: [{ name: t("transcriptions.drop.filterName"), extensions }],
      });
      const paths = selected === null ? [] : [selected].flat();
      if (paths.length > 0) {
        reportImport(t, await importPaths(paths), () => navigate("models"));
      }
    } catch (error) {
      toast.error(t("transcriptions.errors.importFailed"), {
        description: String(error),
      });
    } finally {
      setPicking(false);
    }
  };

  return (
    <div className="space-y-3">
      <div
        className={`rounded-lg border-2 border-dashed px-6 py-8 flex flex-col items-center text-center gap-3 transition-colors ${zoneClasses}`}
        role="region"
        aria-label={t("transcriptions.drop.title")}
      >
        <FileAudio
          className={`w-8 h-8 ${refused ? "text-red-400" : drag.active ? "text-logo-primary" : "text-text/40"}`}
          aria-hidden="true"
        />
        <p className="text-sm font-medium">{t("transcriptions.drop.title")}</p>
        <p
          className={`text-xs max-w-sm ${refused ? "text-red-400" : "text-text/60"}`}
          aria-live="polite"
        >
          {hint}
        </p>
        <Button
          variant="primary-soft"
          onClick={handlePick}
          disabled={picking}
          className="flex items-center gap-1.5"
        >
          <FolderOpen className="w-4 h-4" aria-hidden="true" />
          {t("transcriptions.drop.choose")}
        </Button>
        <p className="text-[11px] text-text/40">
          {t("transcriptions.drop.formats")}
        </p>
      </div>

      <div className="flex flex-wrap items-center gap-x-6 gap-y-2 px-1">
        <label className="flex items-center gap-2 text-xs text-text/70">
          <span>{t("transcriptions.options.language")}</span>
          <LanguageOverride
            value={options.language}
            onChange={(language) => setOptions({ language })}
          />
        </label>
        {postProcessAvailable && (
          <label className="flex items-center gap-2 text-xs text-text/70 cursor-pointer">
            <input
              type="checkbox"
              checked={options.post_process}
              onChange={(e) => setOptions({ post_process: e.target.checked })}
              className="accent-logo-primary"
            />
            <span>{t("transcriptions.options.postProcess")}</span>
          </label>
        )}
      </div>
    </div>
  );
};
