//! Meeting recording and transcription. `capture` records the microphone
//! and the system audio as two aligned tracks on disk; storage and lifecycle
//! live in `managers::meeting`, which builds on it. `control` and `indicator`
//! are the manual controls (tray, shortcut, CLI) and the floating recording
//! pill.
//!
//! Transcription is engine-independent here: each track is decoded on its
//! own ([`track`]), the mic's echo of the system track is removed
//! ([`crosstalk`]), and the two are merged into one "Me / Others" transcript
//! ([`transcript`]). The queue that runs it on the engine is
//! `managers::transcription::meeting`.

pub mod capture;
pub mod control;
pub mod crosstalk;
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
    use super::transcript::{merge_tracks, render_markdown, MeetingInfo, Speaker, TrackTranscript};
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
        println!("{}", render_markdown(&meeting, &segments));
        assert!(segments.iter().any(|s| s.speaker == Speaker::Me));
        assert!(segments.iter().any(|s| s.speaker == Speaker::Others));
        assert!(segments.windows(2).all(|w| w[0].start_ms <= w[1].start_ms));
    }
}
