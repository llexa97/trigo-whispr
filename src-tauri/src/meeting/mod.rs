//! Meeting recording and transcription. `capture` records the microphone
//! and the system audio as two aligned tracks on disk; storage and lifecycle
//! live in `managers::meeting`, which builds on it. `control` and `indicator`
//! are the manual controls (tray, shortcut, CLI) and the floating recording
//! pill; `detection` offers to record when a meeting app takes the
//! microphone.
//!
//! Transcription is engine-independent here: each track is decoded on its
//! own ([`track`]), the mic's echo of the system track is removed
//! ([`crosstalk`]), and the two are merged into one "Me / Others" transcript
//! ([`transcript`]). The queue that runs it on the engine is
//! `managers::transcription::meeting`.

pub mod capture;
pub mod control;
pub mod crosstalk;
pub mod detection;
pub mod indicator;
pub mod power;
pub mod track;
pub mod transcript;

#[cfg(test)]
mod integration_tests {
    use std::path::{Path, PathBuf};

    use anyhow::Result;
    use transcribe_cpp::{Model, RunOptions, Session, Transcript};

    use super::crosstalk::CrosstalkOptions;
    use super::track::{decode_track, SpeechGate, TrackHooks};
    use super::transcript::{
        merge_tracks, merge_tracks_with_speakers, render_markdown, MeetingInfo, Speaker,
        TrackTranscript, TrackTurns,
    };
    use crate::audio_toolkit::chunking::Chunker;
    use crate::audio_toolkit::{SileroVad, SpeechScorer};

    struct SessionHooks<'a>(&'a mut Session);

    impl TrackHooks for SessionHooks<'_> {
        fn check_cancelled(&self) -> Result<()> {
            Ok(())
        }

        fn before_chunk(&mut self) -> Result<()> {
            Ok(())
        }

        fn decode(&mut self, pcm: Vec<f32>) -> Result<Transcript> {
            Ok(self.0.run(&pcm, &RunOptions::default())?)
        }

        fn after_chunk(&mut self, _samples: usize) {}
    }

    fn vad() -> Result<Box<dyn SpeechScorer>> {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/models/silero-vad-v6.2-F32.gguf");
        Ok(Box::new(SileroVad::new(path, 0.3)?))
    }

    fn decode(session: &mut Session, path: &Path) -> TrackTranscript {
        let chunker = Chunker::new(Box::new(vad));
        decode_track(
            path,
            chunker,
            SpeechGate::new(vad()),
            &mut SessionHooks(session),
        )
        .unwrap()
    }

    /// Decodes two short fixture tracks with a real model and VAD, end to end
    /// but for the engine's worker process:
    ///
    /// ```sh
    /// TRIGO_WHISPR_TEST_MODEL=/path/to/model.gguf \
    /// TRIGO_WHISPR_MEETING_FIXTURES=/folder/with/mic.wav/and/system.wav \
    ///   cargo test --lib meeting_fixture -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a model and two fixture WAVs, see the doc comment"]
    fn meeting_fixture_transcribes_me_and_others() {
        let model = std::env::var("TRIGO_WHISPR_TEST_MODEL").expect("TRIGO_WHISPR_TEST_MODEL");
        let fixtures = PathBuf::from(
            std::env::var("TRIGO_WHISPR_MEETING_FIXTURES").expect("TRIGO_WHISPR_MEETING_FIXTURES"),
        );
        transcribe_cpp::init_backends_default().unwrap();
        let model = Model::load(model).unwrap();
        let mut session = model.session().unwrap();

        let mic = decode(&mut session, &fixtures.join("mic.wav"));
        let system = decode(&mut session, &fixtures.join("system.wav"));
        let segments = merge_tracks(Some(&mic), Some(&system), &CrosstalkOptions::default());

        let meeting = MeetingInfo {
            id: "fixture".into(),
            title: "Fixture".into(),
            started_at: 0,
            duration_ms: segments.last().map_or(0, |s| s.end_ms.max(0) as u64),
            source_app: None,
        };
        println!("{}", render_markdown(&meeting, &segments, &[]));
        assert!(segments.iter().any(|s| s.speaker == Speaker::Me));
        assert!(segments.iter().any(|s| s.speaker == Speaker::Others));
        assert!(segments.windows(2).all(|w| w[0].start_ms <= w[1].start_ms));
    }

    /// Transcribes and diarizes a recording of several people, as the
    /// system track of a meeting and as an imported file:
    ///
    /// ```sh
    /// TRIGO_WHISPR_TEST_MODEL=/path/to/whisper.gguf \
    /// TRIGO_WHISPR_TEST_DIARIZE_MODEL=/path/to/sortformer.gguf \
    /// TRIGO_WHISPR_TEST_DIARIZE_WAV=/path/to/several-voices.wav \
    ///   cargo test --lib diarized_fixture -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs two models and a WAV, see the doc comment"]
    fn diarized_fixture_tells_people_apart() {
        use crate::diarization::{renumber_by_appearance, speakers_of, turns_from};
        use transcribe_cpp::{
            DiarizeExtension, DiarizeOptions, SortformerDiarizeOptions, SortformerPreset,
        };

        let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("set {name}"));
        let wav = PathBuf::from(var("TRIGO_WHISPR_TEST_DIARIZE_WAV"));
        transcribe_cpp::init_backends_default().unwrap();

        let asr = Model::load(var("TRIGO_WHISPR_TEST_MODEL")).unwrap();
        let track = decode(&mut asr.session().unwrap(), &wav);

        let diarizer = Model::load(var("TRIGO_WHISPR_TEST_DIARIZE_MODEL")).unwrap();
        let pcm = super::track::read_track(&wav).unwrap();
        let options = DiarizeOptions {
            family: Some(DiarizeExtension::Sortformer(SortformerDiarizeOptions {
                preset: Some(SortformerPreset::VeryHighLatency),
            })),
        };
        let turns = turns_from(
            &diarizer
                .diarize_session()
                .unwrap()
                .run(&pcm, &options)
                .unwrap(),
        );

        // As a meeting's system track: word by word.
        let segments = merge_tracks_with_speakers(
            None,
            Some(&track),
            &CrosstalkOptions::default(),
            TrackTurns {
                mic: None,
                system: Some(&turns),
            },
        );
        let meeting = MeetingInfo {
            id: "fixture".into(),
            title: "Diarized fixture".into(),
            started_at: 0,
            duration_ms: segments.last().map_or(0, |s| s.end_ms.max(0) as u64),
            source_app: None,
        };
        println!("{}", render_markdown(&meeting, &segments, &[]));

        // As an imported file: segment by segment.
        let spans: Vec<(u64, u64)> = track
            .segments
            .iter()
            .map(|s| (s.start_ms, s.end_ms))
            .collect();
        let mut speakers = speakers_of(&turns, &spans);
        renumber_by_appearance(&mut speakers);
        for (segment, speaker) in track.segments.iter().zip(&speakers) {
            println!("[{speaker:?}] {}", segment.text.trim());
        }

        let mut people: Vec<u32> = segments.iter().filter_map(|s| s.speaker_id).collect();
        people.sort_unstable();
        people.dedup();
        assert!(people.len() >= 2, "people found: {people:?}");
        assert!(segments.iter().all(|s| s.speaker == Speaker::Others));
    }
}
