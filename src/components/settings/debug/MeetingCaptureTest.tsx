import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { commands, type MeetingCaptureTestReport } from "@/bindings";
import { useOsType } from "../../../hooks/useOsType";

const TEST_SECONDS = 10;

/**
 * Records a short meeting capture (microphone + system audio, two aligned
 * 16 kHz WAV tracks) into a temporary folder and shows where it went.
 * Meeting capture is not available on Linux.
 */
export const MeetingCaptureTest: React.FC = () => {
  const { t } = useTranslation();
  const osType = useOsType();
  const [running, setRunning] = useState(false);
  const [report, setReport] = useState<MeetingCaptureTestReport | null>(null);
  const [error, setError] = useState<string | null>(null);

  if (osType !== "windows" && osType !== "macos") {
    return null;
  }

  const runTest = async () => {
    setRunning(true);
    setReport(null);
    setError(null);
    try {
      const result = await commands.testMeetingCapture(TEST_SECONDS);
      if (result.status === "ok") {
        setReport(result.data);
      } else {
        setError(result.error);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setRunning(false);
    }
  };

  const none = t("settings.debug.meetingCaptureTest.none");

  return (
    <div className="p-4 space-y-2">
      <div className="flex justify-between items-center gap-2">
        <div>
          <p className="text-sm font-medium">
            {t("settings.debug.meetingCaptureTest.title")}
          </p>
          <p className="text-xs text-mid-gray">
            {t("settings.debug.meetingCaptureTest.description")}
          </p>
        </div>
        <button
          onClick={runTest}
          disabled={running}
          className="px-2 py-1 text-sm font-semibold bg-mid-gray/10 border border-mid-gray/80 hover:bg-logo-primary/10 rounded cursor-pointer hover:border-logo-primary disabled:opacity-50 disabled:cursor-default whitespace-nowrap"
        >
          {t("settings.debug.meetingCaptureTest.run")}
        </button>
      </div>
      {running && (
        <p className="text-sm animate-pulse">
          {t("settings.debug.meetingCaptureTest.running")}
        </p>
      )}
      {error !== null && (
        <p className="text-sm text-red-500">
          {t("settings.debug.meetingCaptureTest.failed", { error })}
        </p>
      )}
      {report !== null && (
        <div className="text-sm font-mono space-y-1 break-all select-text">
          <p>
            {t("settings.debug.meetingCaptureTest.outDir")}: {report.out_dir}
          </p>
          <p>
            {t("settings.debug.meetingCaptureTest.mic")}:{" "}
            {report.output.mic_path ?? none}
          </p>
          <p>
            {t("settings.debug.meetingCaptureTest.system")}:{" "}
            {report.output.system_path ?? none}
          </p>
          <p>
            {t("settings.debug.meetingCaptureTest.duration")}:{" "}
            {report.output.duration_ms} ·{" "}
            {t("settings.debug.meetingCaptureTest.mode")}:{" "}
            {report.output.system_mode ?? none}
          </p>
          <p>
            {t("settings.debug.meetingCaptureTest.warnings")}:{" "}
            {report.output.warnings.length > 0
              ? report.output.warnings.join(", ")
              : none}
          </p>
        </div>
      )}
    </div>
  );
};
