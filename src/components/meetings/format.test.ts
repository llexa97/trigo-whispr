import assert from "node:assert/strict";
import i18next from "i18next";
import fr from "../../i18n/locales/fr/translation.json";
import {
  formatDuration,
  formatTimestamp,
  levelToMeter,
  markdownFileName,
  speakerLabel,
} from "./format";

assert.equal(formatTimestamp(72_000), "00:01:12");
assert.equal(formatTimestamp(3_723_999), "01:02:03");
assert.equal(formatDuration(65_000), "01:05");
assert.equal(formatDuration(3_723_000), "1:02:03");

assert.equal(levelToMeter(0), 0);
assert.equal(levelToMeter(1), 1);
assert.equal(levelToMeter(0.001), 0);

assert.equal(markdownFileName('Réunion: "Q3"/budget'), "Réunion Q3 budget.md");
assert.equal(markdownFileName(" / "), "meeting.md");

const i18n = i18next.createInstance();
await i18n.init({ lng: "fr", resources: { fr: { translation: fr } } });
assert.equal(speakerLabel(i18n.t, "me"), "Moi");
assert.equal(speakerLabel(i18n.t, "others"), "Autres");
assert.equal(speakerLabel(i18n.t, "mic_only"), "Micro");
