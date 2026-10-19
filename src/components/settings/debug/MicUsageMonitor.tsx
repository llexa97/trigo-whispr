import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { commands, type MicUsageReport } from "@/bindings";
import { useOsType } from "../../../hooks/useOsType";

/** Same pace as the meeting detection. */
const POLL_MS = 2000;

/**
 * Lists, live, the apps the meeting detection probe sees using the
 * microphone, and the catalog app each one maps to. Only Windows has a
 * probe so far; macOS shows "not supported".
 */
export const MicUsageMonitor: React.FC = () => {
  const { t } = useTranslation();
  const osType = useOsType();
  const [watching, setWatching] = useState(false);
  const [report, setReport] = useState<MicUsageReport | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!watching) {
      return;
    }
    let cancelled = false;
    const poll = async () => {
      try {
        const next = await commands.getMicUsage();
        if (!cancelled) {
          setReport(next);
          setError(null);
        }
      } catch (e) {
        if (!cancelled) {
          setError(String(e));
        }
      }
    };
    poll();
    const timer = window.setInterval(poll, POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [watching]);

  if (osType !== "windows" && osType !== "macos") {
    return null;
  }

  const probeError = error ?? report?.error ?? null;

  return (
    <div className="p-4 space-y-2">
      <div className="flex justify-between items-center gap-2">
        <div>
          <p className="text-sm font-medium">
            {t("settings.debug.micUsage.title")}
          </p>
          <p className="text-xs text-mid-gray">
            {t("settings.debug.micUsage.description")}
          </p>
        </div>
        <button
          onClick={() => setWatching((w) => !w)}
          className="px-2 py-1 text-sm font-semibold bg-mid-gray/10 border border-mid-gray/80 hover:bg-logo-primary/10 rounded cursor-pointer hover:border-logo-primary whitespace-nowrap"
        >
          {watching
            ? t("settings.debug.micUsage.stop")
            : t("settings.debug.micUsage.watch")}
        </button>
      </div>
      {watching && report !== null && !report.supported && (
        <p className="text-sm text-mid-gray">
          {t("settings.debug.micUsage.unsupported")}
        </p>
      )}
      {watching && probeError !== null && (
        <p className="text-sm text-red-500">
          {t("settings.debug.micUsage.failed", { error: probeError })}
        </p>
      )}
      {watching && report !== null && report.supported && (
        <div className="text-sm font-mono space-y-1 break-all select-text">
          {report.users.length === 0 ? (
            <p className="text-mid-gray">
              {t("settings.debug.micUsage.nobody")}
            </p>
          ) : (
            report.users.map((user) => (
              <p key={user.app_id}>
                {user.app_id}
                {user.pid !== null && ` · pid ${user.pid}`}
                {" → "}
                {user.own
                  ? t("settings.debug.micUsage.own")
                  : (user.catalog_app ?? t("settings.debug.micUsage.unknown"))}
                {user.exe_path !== null && (
                  <span className="block text-xs text-mid-gray">
                    {user.exe_path}
                  </span>
                )}
              </p>
            ))
          )}
        </div>
      )}
    </div>
  );
};
