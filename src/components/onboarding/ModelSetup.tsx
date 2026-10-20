import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { AlertCircle, Check, Loader2 } from "lucide-react";
import {
  commands,
  type DefaultModelError,
  type DefaultModelStatus,
} from "@/bindings";
import TrigoWhisprTextLogo from "../icons/TrigoWhisprTextLogo";
import { Button } from "../ui/Button";
import { useModelStore } from "../../stores/modelStore";
import { useDefaultModels } from "../../hooks/useDefaultModels";
import { formatModelSize } from "../../lib/utils/format";

interface ModelSetupProps {
  onComplete: () => void;
  preview?: boolean;
}

/** User-facing explanation of why a default model couldn't be installed. */
export const defaultModelErrorMessage = (
  t: (key: string, options?: Record<string, unknown>) => string,
  error: DefaultModelError,
): string => {
  switch (error.kind) {
    case "disk_space":
      return t("onboarding.setup.errors.diskSpace", {
        required: formatModelSize(error.required_mb),
        available: formatModelSize(error.available_mb),
      });
    case "cancelled":
      return t("onboarding.setup.errors.cancelled");
    case "download":
      return t("onboarding.setup.errors.download");
  }
};

// First-run step: shows the two models Trigo installs on its own (the backend
// starts them at launch) and lets the user in as soon as dictation's is ready.
const ModelSetup: React.FC<ModelSetupProps> = ({
  onComplete,
  preview = false,
}) => {
  const { t, i18n } = useTranslation();
  const status = useDefaultModels();
  const { downloadProgress, downloadStats, selectModel } = useModelStore();
  const [starting, setStarting] = useState(false);

  if (!status) {
    return (
      <div className="h-screen w-full flex items-center justify-center">
        <Loader2 className="w-8 h-8 animate-spin text-text/50" />
      </div>
    );
  }

  const dictation = status.models.find((m) => m.role === "dictation");
  const failed = status.models.filter((m) => m.state === "failed");
  const othersPending = status.models.some(
    (m) => m.role !== "dictation" && m.state !== "ready",
  );
  const dictationReady = dictation?.state === "ready";

  const formatTimeLeft = (seconds: number) =>
    // Unit formatting is localized by the browser, so only the "remaining"
    // wrapper needs translating.
    new Intl.NumberFormat(i18n.language, {
      style: "unit",
      unit: seconds < 60 ? "second" : "minute",
      unitDisplay: "short",
    }).format(seconds < 60 ? Math.ceil(seconds) : Math.ceil(seconds / 60));

  const handleRetry = () => {
    if (preview) return;
    commands.retryDefaultModels().catch((e) => {
      console.error("Failed to retry default models:", e);
    });
  };

  const handleStart = async () => {
    if (preview || !dictation) return;
    setStarting(true);
    // Loads the model and marks onboarding as completed.
    if (await selectModel(dictation.model_id)) {
      onComplete();
    } else {
      toast.error(t("onboarding.errors.selectModel"));
      setStarting(false);
    }
  };

  return (
    <div className="h-screen w-full flex flex-col p-6 gap-5 items-center justify-center">
      <TrigoWhisprTextLogo width={160} className="shrink-0" />

      <div className="text-center max-w-md shrink-0">
        <h2 className="text-xl font-semibold text-text mb-1.5">
          {t("onboarding.setup.title")}
        </h2>
        <p className="text-sm text-text/70">
          {t("onboarding.setup.description")}
        </p>
      </div>

      <ul className="w-full max-w-[560px] min-h-0 overflow-y-auto rounded-xl border-2 border-mid-gray/20 divide-y divide-mid-gray/20">
        {status.models.map((model) => {
          const progress = downloadProgress[model.model_id];
          const speed = downloadStats[model.model_id]?.speed ?? 0;
          const remainingMb =
            progress && progress.total > 0
              ? (progress.total - progress.downloaded) / (1024 * 1024)
              : 0;
          return (
            <SetupRow
              key={model.model_id}
              model={model}
              percentage={progress?.percentage}
              downloadedMb={
                progress ? progress.downloaded / (1024 * 1024) : undefined
              }
              speed={speed}
              timeLeft={
                speed > 0 && remainingMb > 0
                  ? formatTimeLeft(remainingMb / speed)
                  : null
              }
            />
          );
        })}
      </ul>

      {failed.length > 0 && (
        <div
          role="alert"
          className="w-full max-w-[560px] flex items-start gap-3 rounded-lg border border-error/30 bg-error/5 px-4 py-3"
        >
          <AlertCircle className="w-4 h-4 mt-0.5 shrink-0 text-error" />
          <div className="flex-1 min-w-0 space-y-1 text-sm">
            {failed.map(
              (model) =>
                model.error && (
                  <p key={model.model_id} className="text-text/80">
                    <span className="font-medium">{model.name}</span>
                    {" — "}
                    {defaultModelErrorMessage(t, model.error)}
                  </p>
                ),
            )}
          </div>
          <Button
            variant="secondary"
            size="sm"
            onClick={handleRetry}
            disabled={status.running}
            className="shrink-0"
          >
            {t("onboarding.setup.retry")}
          </Button>
        </div>
      )}

      <div className="w-full max-w-[560px] flex flex-col items-center gap-2 shrink-0">
        <Button
          variant="primary"
          size="lg"
          className="w-full"
          onClick={handleStart}
          disabled={!dictationReady || starting}
        >
          {starting ? (
            <span className="flex items-center justify-center gap-2">
              <Loader2 className="w-4 h-4 animate-spin" />
              {t("modelSelector.loading", { modelName: dictation?.name })}
            </span>
          ) : (
            t("onboarding.setup.start")
          )}
        </Button>
        <p className="text-xs text-text/50 text-center" aria-live="polite">
          {!dictationReady
            ? t("onboarding.setup.waitingForDictation", {
                name: dictation?.name,
              })
            : othersPending
              ? t("onboarding.setup.continuesInBackground")
              : null}
        </p>
      </div>
    </div>
  );
};

interface SetupRowProps {
  model: DefaultModelStatus;
  percentage?: number;
  downloadedMb?: number;
  speed: number;
  timeLeft: string | null;
}

const SetupRow: React.FC<SetupRowProps> = ({
  model,
  percentage,
  downloadedMb,
  speed,
  timeLeft,
}) => {
  const { t } = useTranslation();
  const size = formatModelSize(Number(model.size_mb));
  const downloading = model.state === "downloading";

  const status = () => {
    switch (model.state) {
      case "ready":
        return (
          <span
            role="img"
            aria-label={t("modelSelector.modelReady")}
            className="grid place-items-center w-[22px] h-[22px] rounded-full bg-emerald-500/20 text-emerald-500"
          >
            <Check className="w-3 h-3" strokeWidth={3} />
          </span>
        );
      case "downloading":
        return (
          <span className="tabular-nums text-text/50">
            {Math.round(percentage ?? 0)}%
          </span>
        );
      case "failed":
        return (
          <span className="text-error">{t("onboarding.setup.failed")}</span>
        );
      case "pending":
        return (
          <span className="text-text/50">{t("onboarding.setup.pending")}</span>
        );
    }
  };

  return (
    <li className="px-4 py-3 space-y-2">
      <div className="flex items-center gap-3">
        <div className="flex-1 min-w-0">
          <p className="text-sm font-semibold text-text truncate">
            {model.name}
          </p>
          <p className="text-xs text-text/40 truncate">
            {t(`onboarding.setup.roles.${model.role}`)}
            {" · "}
            {downloading && downloadedMb !== undefined
              ? t("onboarding.setup.downloadedOf", {
                  downloaded: formatModelSize(downloadedMb),
                  total: size,
                })
              : size}
          </p>
        </div>
        <div className="flex items-center gap-2 shrink-0 text-xs">
          {status()}
        </div>
      </div>
      {downloading && (
        <>
          <div className="h-1.5 w-full rounded-full bg-mid-gray/20 overflow-hidden">
            <div
              className="h-full rounded-full bg-logo-primary transition-[width] duration-150 ease-linear motion-reduce:transition-none"
              style={{ width: `${percentage ?? 0}%` }}
            />
          </div>
          <p className="flex items-center gap-2 text-xs text-text/50 tabular-nums">
            {speed > 0 && (
              <span>
                {t("modelSelector.downloadSpeed", { speed: speed.toFixed(1) })}
              </span>
            )}
            {speed > 0 && timeLeft && (
              <span className="text-text/30" aria-hidden="true">
                ·
              </span>
            )}
            {timeLeft && (
              <span>
                {t("onboarding.migration.timeRemaining", { time: timeLeft })}
              </span>
            )}
          </p>
        </>
      )}
    </li>
  );
};

export default ModelSetup;
