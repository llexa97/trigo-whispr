import React from "react";
import { useTranslation } from "react-i18next";
import { FileAudio } from "lucide-react";
import { useTranscriptionsStore } from "@/stores/transcriptionsStore";

/** Shown over the other pages while files are dragged over the window:
 * dropping them opens the Transcriptions page. */
export const DropOverlay: React.FC = () => {
  const { t } = useTranslation();
  const drag = useTranscriptionsStore((s) => s.drag);
  if (!drag.active) return null;
  const refused = drag.supported === 0;
  return (
    <div
      className={`fixed inset-3 z-50 pointer-events-none rounded-xl border-2 border-dashed flex flex-col items-center justify-center gap-2 backdrop-blur-[2px] ${refused ? "border-red-400 bg-red-500/10" : "border-logo-primary bg-logo-primary/15"}`}
      aria-live="polite"
    >
      <FileAudio
        className={`w-10 h-10 ${refused ? "text-red-400" : "text-logo-primary"}`}
        aria-hidden="true"
      />
      <p className="text-sm font-medium">
        {refused
          ? t("transcriptions.drop.refused")
          : t("transcriptions.drop.overlay", { count: drag.supported })}
      </p>
    </div>
  );
};
