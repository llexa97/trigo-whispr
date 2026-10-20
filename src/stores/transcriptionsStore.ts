import { create } from "zustand";
import {
  commands,
  events,
  type FileImportOptions,
  type FileImportOutcome,
  type FileTranscription,
  type FileTranscriptionProgress,
  type FileTranscriptionsUpdated,
} from "@/bindings";

const PAGE_SIZE = 30;
const SEARCH_DEBOUNCE_MS = 250;

/** Files being dragged over the window. */
export interface DragState {
  active: boolean;
  /** Dragged files the import accepts, and the ones it would refuse. */
  supported: number;
  unsupported: number;
}

const NO_DRAG: DragState = { active: false, supported: 0, unsupported: 0 };

interface TranscriptionsStore {
  transcriptions: FileTranscription[];
  hasMore: boolean;
  loading: boolean;
  query: string;
  selectedId: string | null;
  progress: Record<string, FileTranscriptionProgress>;
  /** Lower-case extensions the backend decodes. */
  extensions: string[];
  /** Applied to the next imports. */
  options: FileImportOptions;
  drag: DragState;
  /** Opens a sidebar section (set by the app shell). */
  navigate: (section: "transcriptions" | "models") => void;

  initialize: () => Promise<void>;
  reload: () => Promise<void>;
  loadMore: () => Promise<void>;
  setQuery: (query: string) => void;
  select: (id: string | null) => void;
  setOptions: (options: Partial<FileImportOptions>) => void;
  setDrag: (drag: DragState | null) => void;
  importPaths: (paths: string[]) => Promise<FileImportOutcome>;
  setNavigate: (navigate: TranscriptionsStore["navigate"]) => void;
}

let initialized = false;
// Incremented on every first-page load so a slow, outdated response (e.g.
// for a previous search query) never overwrites a newer one.
let generation = 0;
let loadingMore = false;
let searchTimer: ReturnType<typeof setTimeout> | undefined;
// Live updates received while a page is loading. The page may have been read
// before them, so they are applied again on top of it.
let loadsInFlight = 0;
let liveUpdates: FileTranscriptionsUpdated[] = [];

const trackLoad = async (
  load: () => Promise<((replay: FileTranscriptionsUpdated[]) => void) | null>,
) => {
  loadsInFlight++;
  const mark = liveUpdates.length;
  try {
    const apply = await load();
    apply?.(liveUpdates.slice(mark));
  } finally {
    loadsInFlight--;
    if (loadsInFlight === 0) liveUpdates = [];
  }
};

const fetchPage = (query: string, offset: number) =>
  query.trim()
    ? commands.searchFileTranscriptions(query.trim(), offset, PAGE_SIZE)
    : commands.listFileTranscriptions(offset, PAGE_SIZE);

const isRunning = (t: FileTranscription) =>
  t.status === "decoding" || t.status === "transcribing";

export const useTranscriptionsStore = create<TranscriptionsStore>()((
  set,
  get,
) => {
  const applyUpdate = (payload: FileTranscriptionsUpdated) => {
    set((state) => {
      if (payload.action === "deleted") {
        const progress = { ...state.progress };
        delete progress[payload.id];
        return {
          transcriptions: state.transcriptions.filter(
            (t) => t.id !== payload.id,
          ),
          selectedId: state.selectedId === payload.id ? null : state.selectedId,
          progress,
        };
      }

      const transcription = payload.transcription;
      let progress = state.progress;
      if (!isRunning(transcription) && progress[transcription.id]) {
        progress = { ...progress };
        delete progress[transcription.id];
      }

      if (state.transcriptions.some((t) => t.id === transcription.id)) {
        return {
          transcriptions: state.transcriptions.map((t) =>
            t.id === transcription.id ? transcription : t,
          ),
          progress,
        };
      }
      // New transcriptions only show up in the unfiltered list; a search
      // is refreshed when the query changes.
      if (payload.action === "added" && !state.query.trim()) {
        return {
          transcriptions: [transcription, ...state.transcriptions],
          progress,
        };
      }
      return { progress };
    });
  };

  return {
    transcriptions: [],
    hasMore: false,
    loading: true,
    query: "",
    selectedId: null,
    progress: {},
    extensions: [],
    options: { language: null, post_process: false },
    drag: NO_DRAG,
    navigate: () => {},

    initialize: async () => {
      if (initialized) return;
      initialized = true;

      await Promise.all([
        events.fileTranscriptionsUpdated.listen((event) => {
          if (loadsInFlight > 0) liveUpdates.push(event.payload);
          applyUpdate(event.payload);
        }),
        events.fileTranscriptionProgress.listen((event) =>
          set((state) => ({
            progress: {
              ...state.progress,
              [event.payload.id]: event.payload,
            },
          })),
        ),
        commands
          .getSupportedTranscriptionExtensions()
          .then((extensions) => set({ extensions })),
      ]);
      await get().reload();
    },

    reload: async () => {
      const current = ++generation;
      const query = get().query;
      set({ loading: true });
      try {
        await trackLoad(async () => {
          const result = await fetchPage(query, 0);
          if (current !== generation) return null;
          if (result.status !== "ok") {
            console.error("Failed to load transcriptions:", result.error);
            return null;
          }
          return (replay) => {
            set({
              transcriptions: result.data.transcriptions,
              hasMore: result.data.has_more,
            });
            replay.forEach(applyUpdate);
          };
        });
      } catch (error) {
        console.error("Failed to load transcriptions:", error);
      } finally {
        if (current === generation) set({ loading: false });
      }
    },

    loadMore: async () => {
      const { hasMore, loading, query, transcriptions } = get();
      if (!hasMore || loading || loadingMore) return;
      loadingMore = true;
      const current = generation;
      try {
        await trackLoad(async () => {
          const result = await fetchPage(query, transcriptions.length);
          if (current !== generation || result.status !== "ok") return null;
          return (replay) => {
            set((state) => {
              const known = new Set(state.transcriptions.map((t) => t.id));
              return {
                transcriptions: [
                  ...state.transcriptions,
                  ...result.data.transcriptions.filter((t) => !known.has(t.id)),
                ],
                hasMore: result.data.has_more,
              };
            });
            replay.forEach(applyUpdate);
          };
        });
      } catch (error) {
        console.error("Failed to load more transcriptions:", error);
      } finally {
        loadingMore = false;
      }
    },

    setQuery: (query) => {
      set({ query });
      clearTimeout(searchTimer);
      searchTimer = setTimeout(() => get().reload(), SEARCH_DEBOUNCE_MS);
    },

    select: (id) => set({ selectedId: id }),

    setOptions: (options) =>
      set((state) => ({ options: { ...state.options, ...options } })),

    setDrag: (drag) => set({ drag: drag ?? NO_DRAG }),

    importPaths: async (paths) => {
      const result = await commands.importFilesForTranscription(
        paths,
        get().options,
      );
      if (result.status !== "ok") throw new Error(result.error);
      return result.data;
    },

    setNavigate: (navigate) => set({ navigate }),
  };
});
