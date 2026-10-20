import { useEffect, useRef, useState } from "react";
import { events } from "@/bindings";

export interface DiarizationDownload {
  downloaded: number;
  total: number;
  percentage: number;
  /** MB/s, smoothed; 0 until measured. */
  speed: number;
}

/**
 * Live download of the speaker detection model. It has its own progress
 * event, apart from the transcription models' `model-download-progress`.
 */
export const useDiarizationDownload = (): DiarizationDownload | null => {
  const [download, setDownload] = useState<DiarizationDownload | null>(null);
  const last = useRef<{ at: number; downloaded: number } | null>(null);

  useEffect(() => {
    const unlisten = events.diarizationModelProgress.listen((event) => {
      const { downloaded, total } = event.payload;
      const now = Date.now();
      setDownload((current) => {
        let speed = current?.speed ?? 0;
        const previous = last.current;
        if (!previous) {
          last.current = { at: now, downloaded };
        } else if (now - previous.at > 500) {
          const measured = Math.max(
            0,
            (downloaded - previous.downloaded) /
              (1024 * 1024) /
              ((now - previous.at) / 1000),
          );
          speed = speed > 0 ? speed * 0.8 + measured * 0.2 : measured;
          last.current = { at: now, downloaded };
        }
        return {
          downloaded,
          total,
          percentage: total > 0 ? (downloaded / total) * 100 : 0,
          speed,
        };
      });
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  return download;
};
