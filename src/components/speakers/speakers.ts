import type { TFunction } from "i18next";
import type { SpeakerName } from "@/bindings";

/**
 * Chip colours for the people found by speaker detection, by `speaker_id`.
 * Blue comes last: the accent (and so the "Me" chip) is blue.
 */
const PERSON_CLASSES = [
  "bg-amber-500/20 text-amber-600",
  "bg-emerald-500/20 text-emerald-600",
  "bg-fuchsia-500/20 text-fuchsia-500",
  "bg-sky-500/20 text-sky-500",
];

export const personClasses = (speakerId: number): string =>
  PERSON_CLASSES[(Math.max(1, speakerId) - 1) % PERSON_CLASSES.length];

/** The name the user gave a person, if any. */
export const personName = (
  speakerId: number,
  names: SpeakerName[],
): string | undefined => names.find((n) => n.speaker_id === speakerId)?.name;

/** A person's label: the name the user gave, or "Speaker N". */
export const personLabel = (
  t: TFunction,
  speakerId: number,
  names: SpeakerName[],
): string =>
  personName(speakerId, names) ?? t("speakers.person", { n: speakerId });
