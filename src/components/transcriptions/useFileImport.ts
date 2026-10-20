import { useCallback, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { toast } from "sonner";
import { events } from "@/bindings";
import { useTranscriptionsStore } from "@/stores/transcriptionsStore";
import { partitionPaths } from "./format";
import { reportImport, reportUnsupported } from "./importReport";

type Section = "transcriptions" | "models";

/**
 * Imports files dropped anywhere on the window (and the ones picked from the
 * tray): the Transcriptions page opens and they are queued. While files are
 * dragged over the window, the store tells the drop zone what they are.
 */
export const useFileImport = (
  enabled: boolean,
  navigate: (section: Section) => void,
) => {
  const { t } = useTranslation();
  const initialize = useTranscriptionsStore((s) => s.initialize);
  const setDrag = useTranscriptionsStore((s) => s.setDrag);
  const importPaths = useTranscriptionsStore((s) => s.importPaths);
  const setNavigate = useTranscriptionsStore((s) => s.setNavigate);
  const openModels = useCallback(() => navigate("models"), [navigate]);

  useEffect(() => setNavigate(navigate), [navigate, setNavigate]);

  useEffect(() => {
    if (enabled) initialize();
  }, [enabled, initialize]);

  useEffect(() => {
    if (!enabled) return;
    const unlisten = getCurrentWebview().onDragDropEvent((event) => {
      const payload = event.payload;
      const extensions = useTranscriptionsStore.getState().extensions;
      if (payload.type === "enter") {
        const { supported, unsupported } = partitionPaths(
          payload.paths,
          extensions,
        );
        setDrag({
          active: true,
          supported: supported.length,
          unsupported: unsupported.length,
        });
      } else if (payload.type === "leave") {
        setDrag(null);
      } else if (payload.type === "drop") {
        setDrag(null);
        const { supported, unsupported } = partitionPaths(
          payload.paths,
          extensions,
        );
        reportUnsupported(t, unsupported);
        if (supported.length === 0) return;
        navigate("transcriptions");
        importPaths(supported)
          .then((outcome) => reportImport(t, outcome, openModels))
          .catch((error) =>
            toast.error(t("transcriptions.errors.importFailed"), {
              description: String(error),
            }),
          );
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [enabled, importPaths, navigate, openModels, setDrag, t]);

  useEffect(() => {
    if (!enabled) return;
    const unlisten = events.filesImported.listen((event) => {
      navigate("transcriptions");
      reportImport(t, event.payload.outcome, openModels);
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [enabled, navigate, openModels, t]);
};
