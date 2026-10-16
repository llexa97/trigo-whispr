//! A meeting's transcript: each track's chunks put back on the track's
//! timeline, then the "Me / Others" merge and the files written next to the
//! recording.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{Local, TimeZone};
use serde::{Deserialize, Serialize};
use specta::Type;
use transcribe_cpp::Transcript;

use super::crosstalk::{drop_echoed_segments, split_crosstalk, CrosstalkOptions};
use crate::audio_toolkit::chunking::join_transcripts;

/// A pause longer than this between two pieces of one speaker starts a new
/// segment.
pub const MAX_PAUSE_MS: u64 = 2_000;

pub use crate::managers::meeting::folder::{
    TRANSCRIPT_JSON_FILE as TRANSCRIPT_JSON, TRANSCRIPT_MD_FILE as TRANSCRIPT_MD,
};

/// One word, in milliseconds from the start of its track.
#[derive(Debug, Clone, PartialEq)]
pub struct TimedWord {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

/// One segment as the model returned it, in milliseconds from the start of
/// its track.
#[derive(Debug, Clone, PartialEq)]
pub struct TimedSegment {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

/// Everything decoded from one track, chunk by chunk, on the track's timeline.
#[derive(Debug, Clone, Default)]
pub struct TrackTranscript {
    pub words: Vec<TimedWord>,
    pub segments: Vec<TimedSegment>,
    /// Chunks that had text but no word timestamps: their words are missing,
    /// so the track can only be de-duplicated segment by segment.
    chunks_without_words: usize,
    /// The first language the model detected, if it reported one.
    pub language: Option<String>,
}

impl TrackTranscript {
    /// Adds one chunk's transcript. The model's times are relative to the
    /// chunk, which starts `offset_ms` into the track and lasts `chunk_ms`.
    pub fn push_chunk(&mut self, transcript: &Transcript, offset_ms: u64, chunk_ms: u64) {
        let shift = |t0: i64, t1: i64| {
            let start = offset_ms + t0.max(0) as u64;
            let end = (offset_ms + t1.max(0) as u64).max(start);
            (start, end)
        };
        let words_before = self.words.len();
        for word in &transcript.words {
            if word.text.trim().is_empty() {
                continue;
            }
            let (start_ms, end_ms) = shift(word.t0_ms, word.t1_ms);
            self.words.push(TimedWord {
                text: word.text.clone(),
                start_ms,
                end_ms,
            });
        }
        let segments_before = self.segments.len();
        for segment in &transcript.segments {
            if segment.text.trim().is_empty() {
                continue;
            }
            let (start_ms, end_ms) = shift(segment.t0_ms, segment.t1_ms);
            self.segments.push(TimedSegment {
                text: segment.text.clone(),
                start_ms,
                end_ms,
            });
        }
        // An untimed transcript still belongs to its chunk.
        if self.segments.len() == segments_before && !transcript.text.trim().is_empty() {
            self.segments.push(TimedSegment {
                text: transcript.text.clone(),
                start_ms: offset_ms,
                end_ms: offset_ms + chunk_ms,
            });
        }
        if self.words.len() == words_before && self.segments.len() > segments_before {
            self.chunks_without_words += 1;
        }
        if self.language.is_none() {
            self.language = transcript.language.clone();
        }
    }

    /// Every word of the track has a timestamp (vacuously, for a silent one).
    pub fn has_word_timestamps(&self) -> bool {
        self.chunks_without_words == 0
    }
}

pub use crate::managers::meeting::store::{MeetingSegment, Speaker};

/// A speaker's label in the Markdown export.
pub fn speaker_label(speaker: Speaker) -> &'static str {
    match speaker {
        Speaker::Me => "Moi",
        Speaker::Others => "Autres",
        Speaker::MicOnly => "Micro",
    }
}

/// Merges the tracks into one chronological transcript. The mic's echo of
/// the system track is removed first, word by word when both tracks have
/// word timestamps, else segment by segment. Without a system track the mic
/// is labelled [`Speaker::MicOnly`].
pub fn merge_tracks(
    mic: Option<&TrackTranscript>,
    system: Option<&TrackTranscript>,
    opts: &CrosstalkOptions,
) -> Vec<MeetingSegment> {
    let mut pieces: Vec<Piece> = Vec::new();
    match (mic, system) {
        (Some(mic), Some(system)) => {
            if mic.has_word_timestamps() && system.has_word_timestamps() {
                let split = split_crosstalk(&mic.words, &system.words, opts);
                pieces.extend(words_as(Speaker::Me, &split.you_words));
                pieces.extend(words_as(Speaker::Others, &split.far_words));
            } else {
                let kept = drop_echoed_segments(&mic.segments, &system.segments, opts);
                pieces.extend(segments_as(Speaker::Me, &kept));
                pieces.extend(segments_as(Speaker::Others, &system.segments));
            }
        }
        (Some(mic), None) => pieces.extend(track_as(Speaker::MicOnly, mic)),
        (None, Some(system)) => pieces.extend(track_as(Speaker::Others, system)),
        (None, None) => {}
    }
    group(pieces)
}

/// A word or a model segment, attributed.
struct Piece {
    speaker: Speaker,
    start_ms: u64,
    end_ms: u64,
    text: String,
}

fn words_as(speaker: Speaker, words: &[TimedWord]) -> impl Iterator<Item = Piece> + '_ {
    words.iter().map(move |w| Piece {
        speaker,
        start_ms: w.start_ms,
        end_ms: w.end_ms,
        text: w.text.clone(),
    })
}

fn segments_as(speaker: Speaker, segments: &[TimedSegment]) -> impl Iterator<Item = Piece> + '_ {
    segments.iter().map(move |s| Piece {
        speaker,
        start_ms: s.start_ms,
        end_ms: s.end_ms,
        text: s.text.clone(),
    })
}

fn track_as(speaker: Speaker, track: &TrackTranscript) -> Vec<Piece> {
    if track.has_word_timestamps() && !track.words.is_empty() {
        words_as(speaker, &track.words).collect()
    } else {
        segments_as(speaker, &track.segments).collect()
    }
}

/// Consecutive pieces of one speaker, in time order, joined into segments; a
/// change of speaker or a pause over [`MAX_PAUSE_MS`] starts a new one.
fn group(mut pieces: Vec<Piece>) -> Vec<MeetingSegment> {
    struct Group {
        speaker: Speaker,
        start_ms: u64,
        end_ms: u64,
        texts: Vec<String>,
    }
    // Stable, so pieces starting together keep their track order.
    pieces.sort_by_key(|p| (p.start_ms, p.end_ms));
    let mut groups: Vec<Group> = Vec::new();
    for piece in pieces {
        match groups.last_mut() {
            Some(last)
                if last.speaker == piece.speaker
                    && piece.start_ms.saturating_sub(last.end_ms) <= MAX_PAUSE_MS =>
            {
                last.end_ms = last.end_ms.max(piece.end_ms);
                last.texts.push(piece.text);
            }
            _ => groups.push(Group {
                speaker: piece.speaker,
                start_ms: piece.start_ms,
                end_ms: piece.end_ms,
                texts: vec![piece.text],
            }),
        }
    }
    let segments = groups
        .into_iter()
        .map(|g| MeetingSegment {
            idx: 0,
            speaker: g.speaker,
            start_ms: g.start_ms as i64,
            end_ms: g.end_ms as i64,
            text: join_transcripts(&g.texts),
        })
        .collect();
    renumber(segments)
}

/// Drops empty segments and numbers the rest in order.
pub fn renumber(segments: Vec<MeetingSegment>) -> Vec<MeetingSegment> {
    segments
        .into_iter()
        .filter(|s| !s.text.is_empty())
        .enumerate()
        .map(|(idx, s)| MeetingSegment {
            idx: idx as i64,
            ..s
        })
        .collect()
}

/// What the transcript files say about the meeting itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct MeetingInfo {
    pub id: String,
    pub title: String,
    /// Epoch milliseconds.
    pub started_at: i64,
    pub duration_ms: u64,
    pub source_app: Option<String>,
}

/// `transcript.json`: the segments and what produced them. A superset of
/// what `managers::meeting::folder::read_transcript` re-imports when the
/// database is rebuilt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptFile {
    pub version: u32,
    #[serde(flatten)]
    pub meeting: MeetingInfo,
    pub model_id: String,
    pub language: Option<String>,
    pub segments: Vec<MeetingSegment>,
}

impl TranscriptFile {
    pub const VERSION: u32 = 1;
}

/// `[hh:mm:ss]` of a meeting offset.
pub fn format_timestamp(ms: u64) -> String {
    let secs = ms / 1_000;
    format!("{:02}:{:02}:{:02}", secs / 3_600, secs / 60 % 60, secs % 60)
}

fn format_duration(ms: u64) -> String {
    let secs = ms / 1_000;
    let (h, m, s) = (secs / 3_600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h} h {m:02} min")
    } else if m > 0 {
        format!("{m} min")
    } else {
        format!("{s} s")
    }
}

fn format_date(epoch_ms: i64) -> String {
    Local
        .timestamp_millis_opt(epoch_ms)
        .single()
        .map(|date| date.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// `transcript.md`.
pub fn render_markdown(meeting: &MeetingInfo, segments: &[MeetingSegment]) -> String {
    let mut header = vec![
        format_date(meeting.started_at),
        format_duration(meeting.duration_ms),
    ];
    if let Some(app) = meeting.source_app.as_deref().filter(|a| !a.is_empty()) {
        header.push(app.to_string());
    }
    let mut md = format!("# {}\n{}\n", meeting.title, header.join(" · "));
    for segment in segments {
        md.push_str(&format!(
            "\n**[{}] {} :** {}\n",
            format_timestamp(segment.start_ms.max(0) as u64),
            speaker_label(segment.speaker),
            segment.text
        ));
    }
    md
}

/// Writes `transcript.json` and `transcript.md` into the meeting's folder,
/// each replaced whole so a crash never leaves half a file.
pub fn write_transcript_files(dir: &Path, file: &TranscriptFile) -> Result<()> {
    let json = serde_json::to_string_pretty(file)?;
    write_replacing(&dir.join(TRANSCRIPT_JSON), json.as_bytes())?;
    let md = render_markdown(&file.meeting, &file.segments);
    write_replacing(&dir.join(TRANSCRIPT_MD), md.as_bytes())
}

fn write_replacing(path: &Path, contents: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, contents).with_context(|| format!("Failed to write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("Failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use transcribe_cpp::{Segment, Word};

    fn word(text: &str, t0: i64, t1: i64) -> Word {
        Word {
            text: text.into(),
            t0_ms: t0,
            t1_ms: t1,
            ..Default::default()
        }
    }

    fn seg(text: &str, t0: i64, t1: i64) -> Segment {
        Segment {
            text: text.into(),
            t0_ms: t0,
            t1_ms: t1,
            ..Default::default()
        }
    }

    fn transcript(segments: Vec<Segment>, words: Vec<Word>) -> Transcript {
        Transcript {
            text: segments
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            segments,
            words,
            ..Default::default()
        }
    }

    fn tw(text: &str, start: u64, end: u64) -> TimedWord {
        TimedWord {
            text: text.into(),
            start_ms: start,
            end_ms: end,
        }
    }

    fn track_of_words(words: Vec<TimedWord>) -> TrackTranscript {
        TrackTranscript {
            words,
            ..Default::default()
        }
    }

    fn summary(segments: &[MeetingSegment]) -> Vec<(Speaker, i64, &str)> {
        segments
            .iter()
            .map(|s| (s.speaker, s.start_ms, s.text.as_str()))
            .collect()
    }

    #[test]
    fn chunk_times_are_shifted_by_the_chunk_offset() {
        let mut track = TrackTranscript::default();
        track.push_chunk(
            &transcript(
                vec![seg(" Hello world", 100, 900)],
                vec![word(" Hello", 100, 400), word(" world", 450, 900)],
            ),
            30_000,
            30_000,
        );
        assert_eq!(
            track.words,
            vec![tw(" Hello", 30_100, 30_400), tw(" world", 30_450, 30_900)]
        );
        assert_eq!(track.segments[0].start_ms, 30_100);
        assert_eq!(track.segments[0].end_ms, 30_900);
        assert!(track.has_word_timestamps());
    }

    #[test]
    fn negative_or_inverted_times_are_clamped() {
        let mut track = TrackTranscript::default();
        track.push_chunk(
            &transcript(vec![seg("a", -20, -10)], vec![word("a", -20, -30)]),
            1_000,
            5_000,
        );
        assert_eq!(track.words, vec![tw("a", 1_000, 1_000)]);
    }

    #[test]
    fn a_chunk_without_words_falls_back_to_segments() {
        let mut track = TrackTranscript::default();
        track.push_chunk(&transcript(vec![seg("Bonjour", 0, 800)], vec![]), 0, 30_000);
        assert!(!track.has_word_timestamps());
        assert_eq!(track.segments.len(), 1);
    }

    #[test]
    fn untimed_text_spans_its_chunk() {
        let mut track = TrackTranscript::default();
        let untimed = Transcript {
            text: "Bonjour".into(),
            ..Default::default()
        };
        track.push_chunk(&untimed, 60_000, 25_000);
        assert_eq!(
            track.segments,
            vec![TimedSegment {
                text: "Bonjour".into(),
                start_ms: 60_000,
                end_ms: 85_000
            }]
        );
    }

    #[test]
    fn a_silent_chunk_keeps_word_timestamps_usable() {
        let mut track = TrackTranscript::default();
        track.push_chunk(&Transcript::default(), 0, 30_000);
        assert!(track.has_word_timestamps());
        assert!(track.segments.is_empty());
    }

    #[test]
    fn tracks_merge_chronologically() {
        let mic = track_of_words(vec![
            tw("Bonjour", 0, 400),
            tw("tout", 450, 700),
            tw("le", 750, 800),
            tw("monde.", 850, 1_200),
            tw("Oui,", 6_000, 6_300),
            tw("d'accord.", 6_350, 6_800),
        ]);
        let system = track_of_words(vec![
            tw("Salut,", 2_000, 2_400),
            tw("on", 2_500, 2_600),
            tw("commence ?", 2_650, 3_100),
        ]);
        let merged = merge_tracks(Some(&mic), Some(&system), &CrosstalkOptions::default());
        assert_eq!(
            summary(&merged),
            vec![
                (Speaker::Me, 0, "Bonjour tout le monde."),
                (Speaker::Others, 2_000, "Salut, on commence ?"),
                (Speaker::Me, 6_000, "Oui, d'accord."),
            ]
        );
        assert_eq!(merged[0].end_ms, 1_200);
    }

    #[test]
    fn a_long_pause_splits_one_speaker() {
        let mic = track_of_words(vec![tw("Un.", 0, 500), tw("Deux.", 2_600, 3_000)]);
        let merged = merge_tracks(Some(&mic), None, &CrosstalkOptions::default());
        assert_eq!(merged.len(), 2);
        // Exactly 2 s does not split.
        let mic = track_of_words(vec![tw("Un.", 0, 500), tw("Deux.", 2_500, 3_000)]);
        let merged = merge_tracks(Some(&mic), None, &CrosstalkOptions::default());
        assert_eq!(merged.len(), 1);
    }

    #[test]
    fn echo_is_removed_from_me_at_word_level() {
        let mic = track_of_words(vec![
            tw("Hi", 0, 200),
            tw("the", 1_050, 1_200),
            tw("quick", 1_350, 1_500),
            tw("fox", 1_650, 1_800),
        ]);
        let system = track_of_words(vec![
            tw("the", 1_000, 1_150),
            tw("quick", 1_300, 1_450),
            tw("fox", 1_600, 1_750),
        ]);
        let merged = merge_tracks(Some(&mic), Some(&system), &CrosstalkOptions::default());
        assert_eq!(
            summary(&merged),
            vec![
                (Speaker::Me, 0, "Hi"),
                (Speaker::Others, 1_000, "the quick fox"),
            ]
        );
    }

    #[test]
    fn echo_is_removed_at_segment_level_without_words() {
        let mut mic = TrackTranscript::default();
        mic.push_chunk(
            &transcript(
                vec![
                    seg("Bonjour à tous.", 0, 1_000),
                    seg("On commence par le budget", 5_100, 7_900),
                ],
                vec![],
            ),
            0,
            30_000,
        );
        let mut system = TrackTranscript::default();
        system.push_chunk(
            &transcript(
                vec![seg("On commence par le budget.", 5_000, 7_800)],
                vec![],
            ),
            0,
            30_000,
        );
        let merged = merge_tracks(Some(&mic), Some(&system), &CrosstalkOptions::default());
        assert_eq!(
            summary(&merged),
            vec![
                (Speaker::Me, 0, "Bonjour à tous."),
                (Speaker::Others, 5_000, "On commence par le budget."),
            ]
        );
    }

    #[test]
    fn mic_only_is_not_labelled_me() {
        let mic = track_of_words(vec![tw("Bonjour", 0, 400)]);
        let merged = merge_tracks(Some(&mic), None, &CrosstalkOptions::default());
        assert_eq!(summary(&merged), vec![(Speaker::MicOnly, 0, "Bonjour")]);
    }

    #[test]
    fn system_only_is_others() {
        let system = track_of_words(vec![tw("Bonjour", 0, 400)]);
        let merged = merge_tracks(None, Some(&system), &CrosstalkOptions::default());
        assert_eq!(summary(&merged), vec![(Speaker::Others, 0, "Bonjour")]);
    }

    #[test]
    fn markdown_lists_segments_with_timestamps() {
        let meeting = MeetingInfo {
            id: "id".into(),
            title: "Point hebdo".into(),
            started_at: 0,
            duration_ms: 3_725_000,
            source_app: Some("Microsoft Teams".into()),
        };
        let segments = vec![
            MeetingSegment {
                idx: 0,
                speaker: Speaker::Me,
                start_ms: 72_000,
                end_ms: 75_000,
                text: "Bonjour.".into(),
            },
            MeetingSegment {
                idx: 1,
                speaker: Speaker::Others,
                start_ms: 80_500,
                end_ms: 82_000,
                text: "Salut.".into(),
            },
        ];
        let md = render_markdown(&meeting, &segments);
        let lines: Vec<&str> = md.lines().collect();
        assert_eq!(lines[0], "# Point hebdo");
        assert!(lines[1].ends_with(" · 1 h 02 min · Microsoft Teams"));
        assert!(md.contains("\n**[00:01:12] Moi :** Bonjour.\n"));
        assert!(md.contains("\n**[00:01:20] Autres :** Salut.\n"));
    }

    #[test]
    fn transcript_files_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let file = TranscriptFile {
            version: TranscriptFile::VERSION,
            meeting: MeetingInfo {
                id: "id".into(),
                title: "Réunion".into(),
                started_at: 1_700_000_000_000,
                duration_ms: 60_000,
                source_app: None,
            },
            model_id: "whisper-small".into(),
            language: Some("fr".into()),
            segments: vec![MeetingSegment {
                idx: 0,
                speaker: Speaker::MicOnly,
                start_ms: 0,
                end_ms: 1_000,
                text: "Bonjour.".into(),
            }],
        };
        write_transcript_files(dir.path(), &file).unwrap();
        let json = fs::read_to_string(dir.path().join(TRANSCRIPT_JSON)).unwrap();
        assert!(json.contains("\"speaker\": \"mic_only\""));
        let read: TranscriptFile = serde_json::from_str(&json).unwrap();
        assert_eq!(read, file);
        let md = fs::read_to_string(dir.path().join(TRANSCRIPT_MD)).unwrap();
        assert!(md.contains("**[00:00:00] Micro :** Bonjour."));

        // A rebuilt database re-imports it.
        let reimported = crate::managers::meeting::folder::read_transcript(dir.path())
            .unwrap()
            .unwrap();
        assert_eq!(reimported.model_id.as_deref(), Some("whisper-small"));
        assert_eq!(reimported.segments, file.segments);
    }

    #[test]
    fn timestamps_format_as_hh_mm_ss() {
        assert_eq!(format_timestamp(0), "00:00:00");
        assert_eq!(format_timestamp(3_723_999), "01:02:03");
    }
}
