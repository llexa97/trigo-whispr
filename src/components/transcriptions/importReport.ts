import type { TFunction } from "i18next";
import { toast } from "sonner";
import type { FileImportOutcome } from "@/bindings";
import { formatDuration } from "../meetings/format";
import { baseName, LONG_FILE_MS } from "./format";

/**
 * Tells the user how an import went: refused files with the reason, a
 * warning for very long files, and a way to the models page when no model
 * is downloaded.
 */
export const reportImport = (
  t: TFunction,
  outcome: FileImportOutcome,
  openModels: () => void,
) => {
  if (outcome.no_model) {
    toast.error(t("transcriptions.errors.noModelTitle"), {
      description: t("transcriptions.errors.no_model"),
      action: {
        label: t("transcriptions.errors.openModels"),
        onClick: openModels,
      },
    });
    return;
  }
  for (const rejected of outcome.rejected) {
    toast.error(
      t("transcriptions.import.rejected", { name: rejected.file_name }),
      { description: t(`transcriptions.errors.${rejected.code}`) },
    );
  }
  for (const added of outcome.added) {
    if ((added.duration_ms ?? 0) > LONG_FILE_MS) {
      toast.info(t("transcriptions.import.longFileTitle"), {
        description: t("transcriptions.import.longFile", {
          name: added.file_name,
          duration: formatDuration(added.duration_ms ?? 0),
        }),
      });
    }
  }
};

/** Refuses dropped files the import does not accept, before any work. */
export const reportUnsupported = (t: TFunction, paths: string[]) => {
  if (paths.length === 0) return;
  toast.error(t("transcriptions.import.unsupportedTitle"), {
    description: paths.map(baseName).join(", "),
  });
};
