import React, {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";
import { Pause, Play } from "lucide-react";
import { formatDuration } from "./format";

/** Above this drift (seconds), the follower track is re-aligned. */
const MAX_DRIFT_S = 0.15;

export interface MeetingAudioPlayerHandle {
  /** Moves both tracks to `ms` and starts playback. */
  seekAndPlay: (ms: number) => void;
}

interface MeetingAudioPlayerProps {
  /** Asset URLs of the microphone and system tracks (either may be absent). */
  micSrc: string | null;
  systemSrc: string | null;
  /** Called with the playback position while it changes. */
  onTimeChange?: (ms: number) => void;
}

/**
 * Plays the microphone and system tracks together through two `<audio>`
 * elements kept in sync: the first available track leads, the other follows
 * and is re-aligned when it drifts. The capture pads both WAVs to the same
 * length, so they share one timeline.
 */
export const MeetingAudioPlayer = forwardRef<
  MeetingAudioPlayerHandle,
  MeetingAudioPlayerProps
>(({ micSrc, systemSrc, onTimeChange }, ref) => {
  const { t } = useTranslation();
  const micRef = useRef<HTMLAudioElement>(null);
  const systemRef = useRef<HTMLAudioElement>(null);
  const frameRef = useRef<number>();
  const [playing, setPlaying] = useState(false);
  const [currentTime, setCurrentTime] = useState(0);
  const [duration, setDuration] = useState(0);

  const tracks = useCallback(
    () =>
      [micRef.current, systemRef.current].filter(
        (a): a is HTMLAudioElement => a !== null && Boolean(a.src),
      ),
    [],
  );

  const reportTime = useCallback(
    (seconds: number) => {
      setCurrentTime(seconds);
      onTimeChange?.(seconds * 1000);
    },
    [onTimeChange],
  );

  const tick = useCallback(() => {
    const [leader, ...followers] = tracks();
    if (!leader) return;
    for (const follower of followers) {
      if (
        !follower.ended &&
        Math.abs(follower.currentTime - leader.currentTime) > MAX_DRIFT_S
      ) {
        follower.currentTime = leader.currentTime;
      }
    }
    reportTime(leader.currentTime);
    if (!leader.paused) frameRef.current = requestAnimationFrame(tick);
  }, [reportTime, tracks]);

  const pause = useCallback(() => {
    tracks().forEach((a) => a.pause());
    setPlaying(false);
    if (frameRef.current) cancelAnimationFrame(frameRef.current);
    frameRef.current = undefined;
  }, [tracks]);

  const play = useCallback(async () => {
    const all = tracks();
    if (all.length === 0) return;
    // Once the leader has ended, "play" starts over.
    const time = all[0].ended ? 0 : all[0].currentTime;
    all.forEach((a) => (a.currentTime = time));
    try {
      await Promise.all(all.map((a) => a.play()));
      setPlaying(true);
      if (!frameRef.current) frameRef.current = requestAnimationFrame(tick);
    } catch (error) {
      console.error("Meeting playback failed:", error);
      pause();
    }
  }, [pause, tick, tracks]);

  const seek = useCallback(
    (seconds: number) => {
      tracks().forEach((a) => (a.currentTime = seconds));
      reportTime(seconds);
    },
    [reportTime, tracks],
  );

  useImperativeHandle(
    ref,
    () => ({
      seekAndPlay: (ms) => {
        seek(ms / 1000);
        void play();
      },
    }),
    [play, seek],
  );

  // Track metadata and end of playback.
  useEffect(() => {
    const all = tracks();
    const onMetadata = () =>
      setDuration(Math.max(0, ...all.map((a) => a.duration || 0)));
    const onEnded = () => {
      if (all.every((a) => a.ended || a.paused)) pause();
    };
    all.forEach((a) => {
      a.addEventListener("loadedmetadata", onMetadata);
      a.addEventListener("ended", onEnded);
    });
    return () => {
      all.forEach((a) => {
        a.removeEventListener("loadedmetadata", onMetadata);
        a.removeEventListener("ended", onEnded);
      });
    };
  }, [micSrc, systemSrc, pause, tracks]);

  // Stop everything when the sources change or the player unmounts.
  useEffect(() => {
    setPlaying(false);
    setCurrentTime(0);
    return () => {
      if (frameRef.current) cancelAnimationFrame(frameRef.current);
      frameRef.current = undefined;
    };
  }, [micSrc, systemSrc]);

  const percent =
    duration > 0 ? Math.min(100, (currentTime / duration) * 100) : 0;

  return (
    <div className="flex items-center gap-3">
      <audio ref={micRef} src={micSrc ?? undefined} preload="metadata" />
      <audio ref={systemRef} src={systemSrc ?? undefined} preload="metadata" />
      <button
        type="button"
        onClick={() => (playing ? pause() : play())}
        className="cursor-pointer text-text hover:text-logo-primary transition-colors focus:outline-none focus-visible:ring-1 focus-visible:ring-logo-primary rounded"
        aria-label={
          playing ? t("meetings.player.pause") : t("meetings.player.play")
        }
      >
        {playing ? (
          <Pause width={20} height={20} fill="currentColor" />
        ) : (
          <Play width={20} height={20} fill="currentColor" />
        )}
      </button>
      <span className="text-xs text-text/60 tabular-nums min-w-[40px]">
        {formatDuration(currentTime * 1000)}
      </span>
      <input
        type="range"
        min={0}
        max={duration || 0}
        step={0.1}
        value={currentTime}
        onChange={(e) => seek(parseFloat(e.target.value))}
        aria-label={t("meetings.player.seek")}
        className="flex-1 h-1 rounded-lg appearance-none cursor-pointer focus:outline-none focus:ring-1 focus:ring-logo-primary"
        style={{
          background: `linear-gradient(to right, #FAA2CA 0%, #FAA2CA ${percent}%, rgba(128, 128, 128, 0.2) ${percent}%, rgba(128, 128, 128, 0.2) 100%)`,
        }}
      />
      <span className="text-xs text-text/60 tabular-nums min-w-[40px]">
        {formatDuration(duration * 1000)}
      </span>
    </div>
  );
});

MeetingAudioPlayer.displayName = "MeetingAudioPlayer";
