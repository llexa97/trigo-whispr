import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  commands,
  events,
  type Meeting,
  type MeetingStateChanged,
  type MeetingsUpdated,
} from "@/bindings";

const PAGE_SIZE = 30;

/**
 * Progress of a meeting transcription, emitted by the transcription pipeline
 * after each chunk. Not in `bindings.ts` yet: the pipeline that emits it
 * lands separately, until then nothing is received.
 */
export interface MeetingTranscriptionProgress {
  meeting_id: string;
  /** 0–1. */
  progress: number;
  eta_ms: number | null;
}

const PROGRESS_EVENT = "meeting-transcription-progress";
const CANCEL_TRANSCRIPTION_COMMAND = "cancel_meeting_transcription";

const IDLE_STATE: MeetingStateChanged = {
  state: "idle",
  meeting_id: null,
  elapsed_ms: 0,
  warnings: [],
  mic_level: 0,
  system_level: 0,
};

interface MeetingsStore {
  recorder: MeetingStateChanged;
  meetings: Meeting[];
  hasMore: boolean;
  loading: boolean;
  query: string;
  selectedId: string | null;
  progress: Record<string, MeetingTranscriptionProgress>;

  initialize: () => Promise<void>;
  reload: () => Promise<void>;
  loadMore: () => Promise<void>;
  setQuery: (query: string) => void;
  select: (id: string | null) => void;
  startMeeting: () => Promise<string | null>;
  stopMeeting: () => Promise<string | null>;
  cancelTranscription: (id: string) => Promise<string | null>;
}

let initialized = false;
// Incremented on every first-page load so a slow, outdated response (e.g.
// for a previous search query) never overwrites a newer one.
let generation = 0;
let loadingMore = false;

const fetchPage = (query: string, offset: number) =>
  query.trim()
    ? commands.searchMeetings(query.trim(), offset, PAGE_SIZE)
    : commands.listMeetings(offset, PAGE_SIZE);

const errorMessage = (error: unknown) =>
  error instanceof Error ? error.message : String(error);

export const useMeetingsStore = create<MeetingsStore>()((set, get) => {
  const applyUpdate = (payload: MeetingsUpdated) => {
    set((state) => {
      if (payload.action === "deleted") {
        const progress = { ...state.progress };
        delete progress[payload.id];
        return {
          meetings: state.meetings.filter((m) => m.id !== payload.id),
          selectedId: state.selectedId === payload.id ? null : state.selectedId,
          progress,
        };
      }

      const meeting = payload.meeting;
      let progress = state.progress;
      if (meeting.status !== "transcribing" && progress[meeting.id]) {
        progress = { ...progress };
        delete progress[meeting.id];
      }

      const exists = state.meetings.some((m) => m.id === meeting.id);
      if (exists) {
        return {
          meetings: state.meetings.map((m) =>
            m.id === meeting.id ? meeting : m,
          ),
          progress,
        };
      }
      // New meetings only show up in the unfiltered list; a search is
      // refreshed when the query changes.
      if (payload.action === "added" && !state.query.trim()) {
        return { meetings: [meeting, ...state.meetings], progress };
      }
      return { progress };
    });
  };

  return {
    recorder: IDLE_STATE,
    meetings: [],
    hasMore: false,
    loading: true,
    query: "",
    selectedId: null,
    progress: {},

    initialize: async () => {
      if (initialized) return;
      initialized = true;

      await Promise.all([
        events.meetingStateChanged.listen((event) =>
          set({ recorder: event.payload }),
        ),
        events.meetingsUpdated.listen((event) => applyUpdate(event.payload)),
        listen<MeetingTranscriptionProgress>(PROGRESS_EVENT, (event) =>
          set((state) => ({
            progress: {
              ...state.progress,
              [event.payload.meeting_id]: event.payload,
            },
          })),
        ),
      ]);

      try {
        set({ recorder: await commands.getMeetingState() });
      } catch (error) {
        console.error("Failed to get meeting state:", error);
      }
      await get().reload();
    },

    reload: async () => {
      const current = ++generation;
      const query = get().query;
      set({ loading: true });
      try {
        const result = await fetchPage(query, 0);
        if (current !== generation) return;
        if (result.status === "ok") {
          set({
            meetings: result.data.meetings,
            hasMore: result.data.has_more,
          });
        } else {
          console.error("Failed to load meetings:", result.error);
        }
      } catch (error) {
        console.error("Failed to load meetings:", error);
      } finally {
        if (current === generation) set({ loading: false });
      }
    },

    loadMore: async () => {
      const { hasMore, loading, query, meetings } = get();
      if (!hasMore || loading || loadingMore) return;
      loadingMore = true;
      const current = generation;
      try {
        const result = await fetchPage(query, meetings.length);
        if (current !== generation || result.status !== "ok") return;
        set((state) => {
          const known = new Set(state.meetings.map((m) => m.id));
          return {
            meetings: [
              ...state.meetings,
              ...result.data.meetings.filter((m) => !known.has(m.id)),
            ],
            hasMore: result.data.has_more,
          };
        });
      } catch (error) {
        console.error("Failed to load more meetings:", error);
      } finally {
        loadingMore = false;
      }
    },

    setQuery: (query) => {
      set({ query });
    },

    select: (id) => set({ selectedId: id }),

    startMeeting: async () => {
      try {
        const result = await commands.startMeeting({ kind: "manual" });
        return result.status === "ok" ? null : result.error;
      } catch (error) {
        return errorMessage(error);
      }
    },

    stopMeeting: async () => {
      try {
        const result = await commands.stopMeeting();
        return result.status === "ok" ? null : result.error;
      } catch (error) {
        return errorMessage(error);
      }
    },

    cancelTranscription: async (id) => {
      try {
        await invoke(CANCEL_TRANSCRIPTION_COMMAND, { id });
        return null;
      } catch (error) {
        return errorMessage(error);
      }
    },
  };
});
