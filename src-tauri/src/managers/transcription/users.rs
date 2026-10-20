//! Who is using the engine. "Unload immediately" means once the last of them
//! is done: a meeting ending (or cancelled) must not unload the model under a
//! dictation, nor a dictation under a meeting's (or an imported file's)
//! transcription.

/// The engine's users. Kept behind one lock with every unload decision, so a
/// user registering and an unload can't interleave: either the unload came
/// first (and the new user loads the model again) or it sees the user.
#[derive(Debug, Default)]
pub(super) struct EngineUsers {
    /// A dictation, from its start (before the model load it triggers) until
    /// its pipeline finished, as the transcription coordinator tracks it.
    dictation: bool,
    /// Transcriptions outside a dictation's pipeline (a re-transcribed
    /// history entry, a file from the CLI) and the dictation's own
    /// transcription run.
    foreground: usize,
    /// Background transcriptions under way: a meeting's, an imported
    /// file's.
    background: usize,
}

impl EngineUsers {
    pub(super) fn in_use(&self) -> bool {
        self.dictation || self.foreground > 0 || self.background > 0
    }

    /// A dictation, or a transcription outside the meeting queue: what a
    /// meeting gives way to.
    pub(super) fn dictating(&self) -> bool {
        self.dictation || self.foreground > 0
    }

    /// A meeting's or an imported file's transcription is under way.
    pub(super) fn background_active(&self) -> bool {
        self.background > 0
    }

    pub(super) fn set_dictation(&mut self, active: bool) {
        self.dictation = active;
    }

    pub(super) fn add_foreground(&mut self) {
        self.foreground += 1;
    }

    pub(super) fn remove_foreground(&mut self) {
        self.foreground = self.foreground.saturating_sub(1);
    }

    pub(super) fn add_background(&mut self) {
        self.background += 1;
    }

    pub(super) fn remove_background(&mut self) {
        self.background = self.background.saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_meeting_ending_during_a_dictation_leaves_the_engine_in_use() {
        let mut users = EngineUsers::default();
        users.add_background();
        users.set_dictation(true);
        users.remove_background();
        assert!(users.in_use(), "the dictation still needs the model");
        users.set_dictation(false);
        assert!(!users.in_use(), "the last user out unloads");
    }

    #[test]
    fn a_dictation_ending_during_a_meeting_leaves_the_engine_in_use() {
        let mut users = EngineUsers::default();
        users.add_background();
        users.set_dictation(true);
        users.add_foreground();
        users.remove_foreground();
        users.set_dictation(false);
        assert!(users.in_use());
        assert!(!users.dictating());
        users.remove_background();
        assert!(!users.in_use());
    }

    #[test]
    fn a_meeting_and_a_file_share_the_engine() {
        let mut users = EngineUsers::default();
        users.add_background();
        users.add_background();
        users.remove_background();
        assert!(users.in_use(), "the file is still being transcribed");
        assert!(!users.dictating());
        users.remove_background();
        assert!(!users.in_use());
    }

    #[test]
    fn foreground_runs_nest() {
        let mut users = EngineUsers::default();
        users.add_foreground();
        users.add_foreground();
        users.remove_foreground();
        assert!(users.dictating());
        users.remove_foreground();
        users.remove_foreground();
        assert!(!users.in_use());
    }
}
