//! Speaker diarization (who spoke when) for meetings and imported files.
//!
//! The diarizer (Sortformer, up to 4 speakers) runs in its own worker
//! process, on CPU, beside the transcription engine: see
//! `managers::transcription::diarize`. It returns speaker turns; this module
//! puts the transcript's words and segments on them. Speakers are numbered
//! by first appearance in the final transcript, from 1.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use specta::Type;
use transcribe_cpp::SpeakerSegment;

/// A piece of text with no speaker turn over it takes the nearest turn
/// within this distance: the diarizer's own speech detection trims the
/// edges of turns that the transcriber still puts words in.
const NEAREST_TURN_MS: u64 = 1_500;

/// The name the user gave a person of a transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SpeakerName {
    pub speaker_id: u32,
    pub name: String,
}

/// One speaker turn, in milliseconds from the start of the audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Turn {
    pub start_ms: u64,
    pub end_ms: u64,
    /// As the diarizer numbers them (arrival order, from 1).
    pub speaker: u32,
}

/// The diarizer's segments as turns, empty or invalid ones dropped, in time
/// order.
pub fn turns_from(segments: &[SpeakerSegment]) -> Vec<Turn> {
    let mut turns: Vec<Turn> = segments
        .iter()
        .filter(|s| s.speaker_id > 0 && s.t1_ms > s.t0_ms && s.t1_ms > 0)
        .map(|s| Turn {
            start_ms: s.t0_ms.max(0) as u64,
            end_ms: s.t1_ms as u64,
            speaker: s.speaker_id as u32,
        })
        .collect();
    turns.sort_by_key(|t| (t.start_ms, t.end_ms));
    turns
}

/// The speaker of `[start_ms, end_ms]`: the one whose turns cover most of
/// it, else the one with the nearest turn within [`NEAREST_TURN_MS`].
pub fn speaker_at(turns: &[Turn], start_ms: u64, end_ms: u64) -> Option<u32> {
    let end_ms = end_ms.max(start_ms);
    let mut overlap: HashMap<u32, u64> = HashMap::new();
    let mut nearest: Option<(u64, u32)> = None;
    for turn in turns {
        let from = start_ms.max(turn.start_ms);
        let to = end_ms.min(turn.end_ms);
        if to > from {
            *overlap.entry(turn.speaker).or_default() += to - from;
        }
        let distance = if turn.end_ms < start_ms {
            start_ms - turn.end_ms
        } else {
            turn.start_ms.saturating_sub(end_ms)
        };
        if nearest.is_none_or(|(best, _)| distance < best) {
            nearest = Some((distance, turn.speaker));
        }
    }
    // Ties go to the lowest id, so the result never depends on map order.
    if let Some((speaker, _)) = overlap
        .into_iter()
        .max_by_key(|&(speaker, covered)| (covered, std::cmp::Reverse(speaker)))
    {
        return Some(speaker);
    }
    nearest
        .filter(|&(distance, _)| distance <= NEAREST_TURN_MS)
        .map(|(_, speaker)| speaker)
}

/// The speakers of consecutive pieces of one audio track. A piece no turn
/// is near keeps the speaker before it (the first ones take the first known
/// speaker), so a stray word never splits a speaker's segment.
pub fn speakers_of(turns: &[Turn], spans: &[(u64, u64)]) -> Vec<Option<u32>> {
    if turns.is_empty() {
        return vec![None; spans.len()];
    }
    let mut speakers: Vec<Option<u32>> = spans
        .iter()
        .map(|&(start, end)| speaker_at(turns, start, end))
        .collect();
    let mut last = speakers.iter().flatten().next().copied();
    for speaker in &mut speakers {
        match speaker {
            Some(known) => last = Some(*known),
            None => *speaker = last,
        }
    }
    speakers
}

/// Renumbers speakers 1, 2, 3… in order of first appearance, so the
/// transcript never shows "Speaker 1" then "Speaker 3".
pub fn renumber_by_appearance(speakers: &mut [Option<u32>]) {
    let mut order: HashMap<u32, u32> = HashMap::new();
    for speaker in speakers.iter_mut().flatten() {
        let next = order.len() as u32 + 1;
        *speaker = *order.entry(*speaker).or_insert(next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(start_ms: u64, end_ms: u64, speaker: u32) -> Turn {
        Turn {
            start_ms,
            end_ms,
            speaker,
        }
    }

    fn segment(t0_ms: i64, t1_ms: i64, speaker_id: i32) -> SpeakerSegment {
        SpeakerSegment {
            t0_ms,
            t1_ms,
            speaker_id,
            p: f32::NAN,
        }
    }

    #[test]
    fn turns_are_sorted_and_invalid_ones_dropped() {
        let turns = turns_from(&[
            segment(5_000, 6_000, 2),
            segment(0, 1_000, 1),
            segment(2_000, 2_000, 1),
            segment(3_000, 4_000, 0),
            segment(-500, 200, 3),
        ]);
        assert_eq!(
            turns,
            vec![turn(0, 200, 3), turn(0, 1_000, 1), turn(5_000, 6_000, 2)]
        );
    }

    #[test]
    fn the_speaker_covering_most_of_a_span_wins() {
        let turns = [turn(0, 1_000, 1), turn(800, 3_000, 2)];
        assert_eq!(speaker_at(&turns, 100, 900), Some(1));
        assert_eq!(speaker_at(&turns, 900, 2_000), Some(2));
    }

    #[test]
    fn overlapping_speakers_tie_to_the_lowest_id() {
        let turns = [turn(0, 1_000, 2), turn(0, 1_000, 1)];
        assert_eq!(speaker_at(&turns, 0, 1_000), Some(1));
    }

    #[test]
    fn a_span_between_turns_takes_the_nearest_close_one() {
        let turns = [turn(0, 1_000, 1), turn(5_000, 6_000, 2)];
        assert_eq!(speaker_at(&turns, 1_200, 1_300), Some(1));
        assert_eq!(speaker_at(&turns, 4_600, 4_800), Some(2));
        assert_eq!(speaker_at(&turns, 2_800, 2_900), None);
    }

    #[test]
    fn a_zero_length_span_inside_a_turn_is_attributed() {
        let turns = [turn(0, 1_000, 1)];
        assert_eq!(speaker_at(&turns, 500, 500), Some(1));
    }

    #[test]
    fn unattributed_pieces_keep_the_speaker_before_them() {
        let turns = [turn(5_000, 6_000, 1), turn(20_000, 21_000, 2)];
        let spans = [(0, 100), (5_100, 5_500), (10_000, 10_500), (20_100, 20_500)];
        assert_eq!(
            speakers_of(&turns, &spans),
            vec![Some(1), Some(1), Some(1), Some(2)]
        );
    }

    #[test]
    fn without_turns_nothing_is_attributed() {
        assert_eq!(speakers_of(&[], &[(0, 100)]), vec![None]);
    }

    #[test]
    fn speakers_are_numbered_by_first_appearance() {
        let mut speakers = vec![Some(3), None, Some(1), Some(3), Some(4), Some(1)];
        renumber_by_appearance(&mut speakers);
        assert_eq!(
            speakers,
            vec![Some(1), None, Some(2), Some(1), Some(3), Some(2)]
        );
    }
}
