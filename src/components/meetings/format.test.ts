import assert from "node:assert/strict";
import i18next from "i18next";
import en from "../../i18n/locales/en/translation.json";
import fr from "../../i18n/locales/fr/translation.json";
import type { Meeting } from "@/bindings";
import {
  formatDuration,
  formatTimestamp,
  levelToMeter,
  markdownFileName,
  speakerKey,
  transcriptMarkdown,
} from "./format";

assert.equal(formatTimestamp(72_000), "00:01:12");
assert.equal(formatTimestamp(3_723_999), "01:02:03");
assert.equal(formatDuration(65_000), "01:05");
assert.equal(formatDuration(3_723_000), "1:02:03");

assert.equal(speakerKey("me"), "me");
assert.equal(speakerKey("mic_only"), "mic_only");
assert.equal(speakerKey("others"), "others");

assert.equal(levelToMeter(0), 0);
assert.equal(levelToMeter(1), 1);
assert.equal(levelToMeter(0.001), 0);

assert.equal(markdownFileName('Réunion: "Q3"/budget'), "Réunion Q3 budget.md");

const i18n = i18next.createInstance();
await i18n.init({
  lng: "fr",
  resources: { en: { translation: en }, fr: { translation: fr } },
});

const meeting: Meeting = {
  id: "a",
  title: "Point budget",
  folder: "f",
  started_at: Date.UTC(2026, 9, 16, 9, 30),
  ended_at: null,
  duration_ms: 125_000,
  source: "detected",
  source_app: "Microsoft Teams",
  has_system_audio: true,
  has_audio: true,
  status: "done",
  error: null,
  model_id: null,
  created_at: 0,
};
const segments = [
  {
    idx: 0,
    speaker: "me" as const,
    start_ms: 72_000,
    end_ms: 75_000,
    text: " Bonjour <à> tous ",
  },
  {
    idx: 1,
    speaker: "others" as const,
    start_ms: 80_000,
    end_ms: 82_000,
    text: "Salut",
  },
];

const markdown = transcriptMarkdown(i18n.t, "fr", meeting, segments);
const lines = markdown.split("\n");
assert.equal(lines[0], "# Point budget");
assert.match(lines[1], /^.+ · 00:02:05 · Microsoft Teams$/);
assert.equal(lines[2], "");
assert.equal(lines[3], "**[00:01:12] Moi :** Bonjour <à> tous");
assert.equal(lines[5], "**[00:01:20] Autres :** Salut");
assert.ok(markdown.endsWith("Salut\n"));

const manual = transcriptMarkdown(
  i18n.t,
  "fr",
  { ...meeting, source: "manual", source_app: null, duration_ms: null },
  [],
);
assert.match(manual, /^# Point budget\n.+ · Manuel\n$/);
