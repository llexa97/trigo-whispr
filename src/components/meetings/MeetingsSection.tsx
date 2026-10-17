import React, { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { Search, Users } from "lucide-react";
import { toast } from "sonner";
import { useMeetingsStore } from "@/stores/meetingsStore";
import { Button } from "../ui/Button";
import { MeetingDetail } from "./MeetingDetail";
import { MeetingList } from "./MeetingList";
import { MeetingRecorder } from "./MeetingRecorder";

const EmptyState: React.FC = () => {
  const { t } = useTranslation();
  const recorder = useMeetingsStore((s) => s.recorder);
  const startMeeting = useMeetingsStore((s) => s.startMeeting);

  const handleStart = async () => {
    const error = await startMeeting();
    if (error) {
      toast.error(t("meetings.errors.start"), { description: error });
    }
  };

  return (
    <div className="px-6 py-8 flex flex-col items-center text-center gap-3">
      <Users className="w-8 h-8 text-text/30" aria-hidden="true" />
      <p className="text-sm font-medium">{t("meetings.empty.title")}</p>
      <p className="text-xs text-text/60 max-w-sm">
        {t("meetings.empty.description")}
      </p>
      {recorder.state === "idle" && (
        <Button variant="primary-soft" onClick={handleStart}>
          {t("meetings.recorder.start")}
        </Button>
      )}
    </div>
  );
};

/** « Meetings » section: recorder, searchable list and meeting detail. */
export const MeetingsSection: React.FC = () => {
  const { t } = useTranslation();
  const initialize = useMeetingsStore((s) => s.initialize);
  const query = useMeetingsStore((s) => s.query);
  const setQuery = useMeetingsStore((s) => s.setQuery);
  const loading = useMeetingsStore((s) => s.loading);
  const count = useMeetingsStore((s) => s.meetings.length);
  const selectedId = useMeetingsStore((s) => s.selectedId);
  const select = useMeetingsStore((s) => s.select);

  useEffect(() => {
    initialize();
  }, [initialize]);

  const searching = query.trim().length > 0;

  let content: React.ReactNode;
  if (loading && count === 0) {
    content = (
      <p className="px-4 py-3 text-center text-sm text-text/60">
        {t("meetings.loading")}
      </p>
    );
  } else if (count === 0 && searching) {
    content = (
      <p className="px-4 py-3 text-center text-sm text-text/60">
        {t("meetings.search.noResults")}
      </p>
    );
  } else if (count === 0) {
    content = <EmptyState />;
  } else {
    content = <MeetingList onOpen={select} />;
  }

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <div className="px-4">
        <MeetingRecorder />
      </div>

      {selectedId ? (
        <div className="px-4">
          <MeetingDetail id={selectedId} onBack={() => select(null)} />
        </div>
      ) : (
        <div className="space-y-2">
          <div className="px-4 flex items-center justify-between gap-4">
            <h2 className="text-xs font-medium text-mid-gray uppercase tracking-wide">
              {t("meetings.title")}
            </h2>
            <label className="relative flex items-center w-56">
              <Search
                className="absolute start-2 w-3.5 h-3.5 text-text/40 pointer-events-none"
                aria-hidden="true"
              />
              <input
                type="search"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder={t("meetings.search.placeholder")}
                aria-label={t("meetings.search.placeholder")}
                className="w-full ps-7 pe-2 py-1 text-sm bg-mid-gray/10 border border-mid-gray/20 rounded-md focus:outline-none focus:border-logo-primary"
              />
            </label>
          </div>
          <div className="bg-background border border-mid-gray/20 rounded-lg overflow-visible">
            {content}
          </div>
        </div>
      )}
    </div>
  );
};
