import assert from "node:assert/strict";
import type {
  FileTranscription,
  FileTranscriptionSegment,
} from "../../bindings";
import {
  baseName,
  exportFileName,
  extensionOf,
  partitionPaths,
  srtTimestamp,
  toMarkdown,
  toPlainText,
  toSrt,
} from "./format";

assert.equal(extensionOf("C:\\Users\\Zoë\\Mémo vocal.M4A"), "m4a");
assert.equal(extensionOf("/home/a/archive.tar.gz"), "gz");
assert.equal(extensionOf("/home/a/.hidden"), "");
assert.equal(extensionOf("noext"), "");
assert.equal(baseName("C:\\dossier\\réunion été.mp3"), "réunion été.mp3");
assert.equal(baseName("/tmp/clip.webm"), "clip.webm");

assert.deepEqual(
  partitionPaths(["/a/x.MP3", "/a/y.txt", "C:\\b\\z.mkv"], ["mp3", "mkv"]),
  { supported: ["/a/x.MP3", "C:\\b\\z.mkv"], unsupported: ["/a/y.txt"] },
);

assert.equal(srtTimestamp(0), "00:00:00,000");
assert.equal(srtTimestamp(3_723_045), "01:02:03,045");

const segments: FileTranscriptionSegment[] = [
  { idx: 0, start_ms: 0, end_ms: 1_500, text: " Bonjour à tous. " },
  { idx: 1, start_ms: 61_000, end_ms: 61_000, text: "Deuxième phrase." },
];

assert.equal(toPlainText(segments), "Bonjour à tous.\nDeuxième phrase.\n");
assert.equal(
  toSrt(segments),
  "1\n00:00:00,000 --> 00:00:01,500\nBonjour à tous.\n\n" +
    // An untimed segment still lasts a second.
    "2\n00:01:01,000 --> 00:01:02,000\nDeuxième phrase.\n",
);

const transcription = {
  file_name: "Mémo.m4a",
  post_processed_text: "Résumé.",
} as FileTranscription;
const md = toMarkdown(transcription, segments, {
  transcript: "Transcript",
  postProcessed: "Post-processed",
});
assert.ok(md.startsWith("# Mémo.m4a\n\n## Post-processed\n\nRésumé.\n"));
assert.ok(md.includes("**[00:01:01]** Deuxième phrase."));

assert.equal(exportFileName("Réunion: Q3.m4a", "srt"), "Réunion Q3.srt");
assert.equal(exportFileName("noext", "txt"), "noext.txt");
assert.equal(exportFileName(" / .mp3", "md"), "transcription.md");
