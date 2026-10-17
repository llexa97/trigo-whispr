//! Detection state machine, free of threads, windows and clocks so it can be
//! tested with a fake probe and a fake clock: the [`service`](super::service)
//! feeds it what the probe saw and runs the [`Action`]s it returns.
//!
//! Per catalog app: `Idle → Candidate(since) → Prompted → Recording |
//! Dismissed`, back to `Idle` when the app releases the microphone. A prompt
//! only follows [`DEBOUNCE`] of continuous use (microphone tests and very
//! short calls stay silent), never while a meeting is already recorded, and
//! "Ignore" holds for the rest of the call.
//!
//! Separately, a recording started from a detection is watched: once its app
//! has released the microphone for [`RELEASE_GRACE`], the engine offers to
//! stop it (ask, or a countdown, per `meeting_auto_stop`). Recordings
//! started manually are never offered to stop. An offer follows the setting:
//! changing `meeting_auto_stop` withdraws it and, unless the new mode is
//! `never`, offers again in the new mode.

use super::catalog::{self, Platform};
use super::MicUser;
use crate::settings::MeetingAutoStop;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// How often the service polls the probe.
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Continuous microphone use before prompting.
pub const DEBOUNCE: Duration = Duration::from_secs(4);
/// Unanswered prompts close (as "Ignore") after this long.
pub const PROMPT_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the source app must have released the microphone before a
/// detected meeting is considered over.
pub const RELEASE_GRACE: Duration = Duration::from_secs(20);

/// The meeting recording as the engine sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recording {
    None,
    /// Started manually, or stopping: blocks prompts, never auto-stopped.
    Other,
    /// Started from a detection of catalog app `app_id`.
    Detected {
        meeting_id: String,
        app_id: String,
    },
}

/// What the engine wants done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Show "<app> meeting detected — Record / Ignore".
    Prompt {
        app_id: &'static str,
        app_name: &'static str,
    },
    /// Close the detection prompt.
    ClosePrompt,
    /// Start recording. `auto`: without asking (`meeting_auto_start`), so a
    /// confirmation is shown once it started.
    StartRecording {
        app_id: &'static str,
        app_name: &'static str,
        auto: bool,
    },
    /// The meeting looks over: ask to stop it, or (`auto`) start the
    /// cancellable countdown.
    OfferStop {
        meeting_id: String,
        app_name: &'static str,
        auto: bool,
    },
    /// The app took the microphone back, the recording ended or
    /// `meeting_auto_stop` changed: withdraw the stop offer (cancel the
    /// countdown).
    WithdrawStopOffer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Record,
    Ignore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppState {
    Candidate(Instant),
    Prompted(Instant),
    /// A start was requested; `seen` once the recording showed up. Until
    /// then no other app is prompted; [`Engine::start_finished`] releases it
    /// if the start did not happen.
    Recording {
        seen: bool,
    },
    Dismissed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Offer {
    /// Offered in this `meeting_auto_stop` mode, not answered yet.
    Pending(MeetingAutoStop),
    /// The user chose to keep recording: not offered again for this release.
    Declined,
}

#[derive(Debug)]
struct StopWatch {
    meeting_id: String,
    released_since: Option<Instant>,
    offer: Option<Offer>,
}

pub struct Settings<'a> {
    pub platform: Platform,
    pub app_overrides: &'a HashMap<String, bool>,
    pub auto_start: bool,
    pub auto_stop: MeetingAutoStop,
}

#[derive(Default)]
pub struct Engine {
    /// Apps not listed are `Idle`.
    apps: HashMap<&'static str, AppState>,
    stop_watch: Option<StopWatch>,
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    /// One poll: what the probe saw at `now`, and the recording state.
    pub fn tick(
        &mut self,
        now: Instant,
        mic_users: &[MicUser],
        recording: &Recording,
        settings: &Settings,
    ) -> Vec<Action> {
        let mut actions = Vec::new();

        // Catalog apps on the microphone (Trigo and unknown apps excluded).
        let on_mic: HashSet<&'static str> = mic_users
            .iter()
            .filter_map(|u| catalog::match_app(u, settings.platform))
            .map(|e| e.id)
            .collect();
        let enabled =
            |id: &str| catalog::entry(id).is_some_and(|e| e.enabled(settings.app_overrides));

        // Released (or disabled) apps go back to Idle.
        let released: Vec<_> = self
            .apps
            .keys()
            .copied()
            .filter(|id| !on_mic.contains(id) || !enabled(id))
            .collect();
        for id in released {
            if let Some(AppState::Prompted(_)) = self.apps.remove(id) {
                actions.push(Action::ClosePrompt);
            }
        }

        // A meeting really being recorded covers every app on the mic for
        // this call. A start still pending (reminder open, slow capture) only
        // defers them: they stay candidates, prompted once it turns out not
        // to have started (`start_finished`).
        let recorded = *recording != Recording::None;
        let mut start_pending = self
            .apps
            .values()
            .any(|s| *s == AppState::Recording { seen: false });
        let mut prompt_open = self
            .apps
            .values()
            .any(|s| matches!(s, AppState::Prompted(_)));

        for entry in catalog::CATALOG {
            let id = entry.id;
            if !on_mic.contains(id) || !enabled(id) {
                continue;
            }
            let recording_from_here =
                matches!(recording, Recording::Detected { app_id, .. } if app_id == id);
            let state = self.apps.get(id).copied();
            let next = match state {
                None if recorded => AppState::Dismissed,
                None => AppState::Candidate(now),
                Some(AppState::Candidate(_)) if recorded => AppState::Dismissed,
                Some(AppState::Candidate(since))
                    if now.duration_since(since) >= DEBOUNCE && !prompt_open && !start_pending =>
                {
                    if settings.auto_start {
                        start_pending = true;
                        actions.push(Action::StartRecording {
                            app_id: id,
                            app_name: entry.display_name,
                            auto: true,
                        });
                        AppState::Recording { seen: false }
                    } else {
                        prompt_open = true;
                        actions.push(Action::Prompt {
                            app_id: id,
                            app_name: entry.display_name,
                        });
                        AppState::Prompted(now)
                    }
                }
                Some(AppState::Prompted(since))
                    if recorded || now.duration_since(since) >= PROMPT_TIMEOUT =>
                {
                    // Recorded some other way meanwhile, or left unanswered.
                    actions.push(Action::ClosePrompt);
                    AppState::Dismissed
                }
                Some(AppState::Recording { .. }) if recording_from_here => {
                    AppState::Recording { seen: true }
                }
                // Its recording was stopped while the call goes on: no new
                // prompt for this call.
                Some(AppState::Recording { seen: true }) => AppState::Dismissed,
                Some(other) => other,
            };
            self.apps.insert(id, next);
        }

        self.watch_for_end(now, &on_mic, recording, settings, &mut actions);
        actions
    }

    fn watch_for_end(
        &mut self,
        now: Instant,
        on_mic: &HashSet<&'static str>,
        recording: &Recording,
        settings: &Settings,
        actions: &mut Vec<Action>,
    ) {
        let Recording::Detected { meeting_id, app_id } = recording else {
            if self
                .stop_watch
                .take()
                .is_some_and(|w| matches!(w.offer, Some(Offer::Pending(_))))
            {
                actions.push(Action::WithdrawStopOffer);
            }
            return;
        };
        if self
            .stop_watch
            .as_ref()
            .is_none_or(|w| &w.meeting_id != meeting_id)
        {
            self.stop_watch = Some(StopWatch {
                meeting_id: meeting_id.clone(),
                released_since: None,
                offer: None,
            });
        }
        let Some(watch) = self.stop_watch.as_mut() else {
            return;
        };

        if on_mic.contains(app_id.as_str()) {
            watch.released_since = None;
            if matches!(watch.offer.take(), Some(Offer::Pending(_))) {
                actions.push(Action::WithdrawStopOffer);
            }
            return;
        }
        let since = *watch.released_since.get_or_insert(now);
        match watch.offer {
            Some(Offer::Declined) => return,
            Some(Offer::Pending(mode)) if mode == settings.auto_stop => return,
            // The mode changed under the offer: withdraw it, then offer
            // again in the new mode below.
            Some(Offer::Pending(_)) => {
                watch.offer = None;
                actions.push(Action::WithdrawStopOffer);
            }
            None => {}
        }
        if now.duration_since(since) < RELEASE_GRACE || settings.auto_stop == MeetingAutoStop::Never
        {
            return;
        }
        watch.offer = Some(Offer::Pending(settings.auto_stop));
        actions.push(Action::OfferStop {
            meeting_id: meeting_id.clone(),
            app_name: catalog::entry(app_id).map_or("", |e| e.display_name),
            auto: settings.auto_stop == MeetingAutoStop::Auto,
        });
    }

    /// The user chose to keep recording meeting `meeting_id` (answered the
    /// stop prompt, or cancelled the countdown). Not offered again until
    /// its app takes the microphone back and releases it again.
    pub fn stop_declined(&mut self, meeting_id: &str) {
        if let Some(watch) = self
            .stop_watch
            .as_mut()
            .filter(|w| w.meeting_id == meeting_id && w.offer.is_some())
        {
            watch.offer = Some(Offer::Declined);
        }
    }

    /// The start requested for `app_id` is over: `started` if the recording
    /// runs, otherwise it was cancelled at the reminder, failed or was
    /// refused. A start that did not happen stops blocking the other apps;
    /// `app_id` itself is not prompted again for this call.
    pub fn start_finished(&mut self, app_id: &str, started: bool) {
        if started {
            return;
        }
        if let Some(state) = self
            .apps
            .iter_mut()
            .find(|(id, _)| **id == app_id)
            .map(|(_, state)| state)
            .filter(|state| **state == AppState::Recording { seen: false })
        {
            *state = AppState::Dismissed;
        }
    }

    /// The user answered the detection prompt for `app_id`.
    pub fn answer(&mut self, app_id: &str, answer: Answer) -> Vec<Action> {
        let Some((&id, state)) = self.apps.iter_mut().find(|(id, _)| **id == app_id) else {
            return vec![Action::ClosePrompt];
        };
        if !matches!(state, AppState::Prompted(_)) {
            return vec![Action::ClosePrompt];
        }
        match answer {
            Answer::Ignore => {
                *state = AppState::Dismissed;
                vec![Action::ClosePrompt]
            }
            Answer::Record => {
                *state = AppState::Recording { seen: false };
                let app_name = catalog::entry(id).map_or("", |e| e.display_name);
                vec![
                    Action::ClosePrompt,
                    Action::StartRecording {
                        app_id: id,
                        app_name,
                        auto: false,
                    },
                ]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fake probe + fake clock: each `poll` advances the clock by the poll
    /// interval and feeds the engine the apps currently "on the mic".
    struct Harness {
        engine: Engine,
        start: Instant,
        now: Instant,
        mic: Vec<MicUser>,
        recording: Recording,
        overrides: HashMap<String, bool>,
        auto_start: bool,
        auto_stop: MeetingAutoStop,
    }

    impl Harness {
        fn new() -> Self {
            let start = Instant::now();
            Self {
                engine: Engine::new(),
                start,
                now: start,
                mic: Vec::new(),
                recording: Recording::None,
                overrides: HashMap::new(),
                auto_start: false,
                auto_stop: MeetingAutoStop::Ask,
            }
        }

        fn take_mic(&mut self, app_id: &str) {
            self.mic.push(MicUser::new(app_id));
        }

        fn release_mic(&mut self, app_id: &str) {
            self.mic.retain(|u| u.app_id != app_id);
        }

        fn poll(&mut self) -> Vec<Action> {
            self.now += POLL_INTERVAL;
            let settings = Settings {
                platform: Platform::Windows,
                app_overrides: &self.overrides,
                auto_start: self.auto_start,
                auto_stop: self.auto_stop,
            };
            self.engine
                .tick(self.now, &self.mic, &self.recording, &settings)
        }

        /// Polls until an action shows up; returns it with the elapsed time.
        fn poll_until(&mut self, limit: Duration) -> Option<(Duration, Vec<Action>)> {
            let from = self.now;
            while self.now.duration_since(from) < limit {
                let actions = self.poll();
                if !actions.is_empty() {
                    return Some((self.now.duration_since(self.start), actions));
                }
            }
            None
        }

        fn poll_for(&mut self, duration: Duration) -> Vec<Action> {
            let from = self.now;
            let mut all = Vec::new();
            while self.now.duration_since(from) < duration {
                all.extend(self.poll());
            }
            all
        }

        fn detected(&mut self, app_id: &str) {
            self.recording = Recording::Detected {
                meeting_id: "m1".into(),
                app_id: app_id.into(),
            };
        }
    }

    fn prompt(app_id: &'static str, app_name: &'static str) -> Action {
        Action::Prompt { app_id, app_name }
    }

    #[test]
    fn prompts_after_four_to_six_seconds_of_microphone_use() {
        let mut h = Harness::new();
        h.take_mic("ms-teams.exe");
        let (at, actions) = h.poll_until(Duration::from_secs(30)).unwrap();
        assert_eq!(actions, vec![prompt("teams", "Microsoft Teams")]);
        assert!(at >= Duration::from_secs(4) && at <= Duration::from_secs(6));
    }

    #[test]
    fn a_short_microphone_use_never_prompts() {
        let mut h = Harness::new();
        h.take_mic("zoom.exe");
        assert!(h.poll().is_empty());
        h.release_mic("zoom.exe");
        assert!(h.poll_for(Duration::from_secs(30)).is_empty());
    }

    #[test]
    fn ignore_holds_until_the_app_releases_the_microphone() {
        let mut h = Harness::new();
        h.take_mic("zoom.exe");
        h.poll_until(Duration::from_secs(10)).unwrap();
        assert_eq!(
            h.engine.answer("zoom", Answer::Ignore),
            vec![Action::ClosePrompt]
        );
        assert!(h.poll_for(Duration::from_secs(120)).is_empty());

        // Next call: prompted again.
        h.release_mic("zoom.exe");
        assert!(h.poll().is_empty());
        h.take_mic("zoom.exe");
        let (_, actions) = h.poll_until(Duration::from_secs(10)).unwrap();
        assert_eq!(actions, vec![prompt("zoom", "Zoom")]);
    }

    #[test]
    fn unanswered_prompts_close_as_ignore() {
        let mut h = Harness::new();
        h.take_mic("zoom.exe");
        h.poll_until(Duration::from_secs(10)).unwrap();
        let (_, actions) = h.poll_until(Duration::from_secs(60)).unwrap();
        assert_eq!(actions, vec![Action::ClosePrompt]);
        assert!(h.poll_for(Duration::from_secs(60)).is_empty());
    }

    #[test]
    fn the_prompt_closes_when_the_app_releases_the_microphone() {
        let mut h = Harness::new();
        h.take_mic("zoom.exe");
        h.poll_until(Duration::from_secs(10)).unwrap();
        h.release_mic("zoom.exe");
        assert_eq!(h.poll(), vec![Action::ClosePrompt]);
    }

    #[test]
    fn no_prompt_while_a_meeting_is_recorded() {
        let mut h = Harness::new();
        h.recording = Recording::Other;
        h.take_mic("zoom.exe");
        assert!(h.poll_for(Duration::from_secs(60)).is_empty());
        // Not after the recording ends either: that call was covered.
        h.recording = Recording::None;
        assert!(h.poll_for(Duration::from_secs(60)).is_empty());
    }

    #[test]
    fn trigo_unknown_and_disabled_apps_never_prompt() {
        let mut h = Harness::new();
        h.take_mic("trigo-whispr.exe");
        h.take_mic("audacity.exe");
        h.take_mic("chrome.exe"); // Off by default.
        h.take_mic("slack.exe");
        h.overrides.insert("slack".into(), false);
        assert!(h.poll_for(Duration::from_secs(60)).is_empty());
    }

    #[test]
    fn record_starts_a_detected_recording() {
        let mut h = Harness::new();
        h.take_mic("zoom.exe");
        h.poll_until(Duration::from_secs(10)).unwrap();
        assert_eq!(
            h.engine.answer("zoom", Answer::Record),
            vec![
                Action::ClosePrompt,
                Action::StartRecording {
                    app_id: "zoom",
                    app_name: "Zoom",
                    auto: false
                }
            ]
        );
        // While it starts, other apps are not prompted.
        h.take_mic("slack.exe");
        assert!(h.poll_for(Duration::from_secs(10)).is_empty());
    }

    #[test]
    fn auto_start_records_without_prompting() {
        let mut h = Harness::new();
        h.auto_start = true;
        h.take_mic("zoom.exe");
        let (_, actions) = h.poll_until(Duration::from_secs(10)).unwrap();
        assert_eq!(
            actions,
            vec![Action::StartRecording {
                app_id: "zoom",
                app_name: "Zoom",
                auto: true
            }]
        );
    }

    #[test]
    fn offers_to_stop_twenty_seconds_after_the_app_released_the_microphone() {
        let mut h = Harness::new();
        h.take_mic("zoom.exe");
        h.poll_until(Duration::from_secs(10)).unwrap();
        h.engine.answer("zoom", Answer::Record);
        h.detected("zoom");
        assert!(h.poll_for(Duration::from_secs(60)).is_empty());

        h.release_mic("zoom.exe");
        let released_at = h.now;
        let (_, actions) = h.poll_until(Duration::from_secs(60)).unwrap();
        let waited = h.now.duration_since(released_at);
        assert!(waited >= RELEASE_GRACE && waited <= RELEASE_GRACE + POLL_INTERVAL);
        assert_eq!(
            actions,
            vec![Action::OfferStop {
                meeting_id: "m1".into(),
                app_name: "Zoom",
                auto: false
            }]
        );
        // Offered once.
        assert!(h.poll_for(Duration::from_secs(60)).is_empty());
    }

    #[test]
    fn auto_stop_mode_asks_for_the_countdown() {
        let mut h = Harness::new();
        h.auto_stop = MeetingAutoStop::Auto;
        h.detected("teams");
        let (_, actions) = h.poll_until(Duration::from_secs(60)).unwrap();
        assert!(matches!(
            actions.as_slice(),
            [Action::OfferStop { auto: true, .. }]
        ));
    }

    #[test]
    fn never_mode_keeps_recording() {
        let mut h = Harness::new();
        h.auto_stop = MeetingAutoStop::Never;
        h.detected("teams");
        assert!(h.poll_for(Duration::from_secs(120)).is_empty());
    }

    #[test]
    fn a_short_release_does_not_end_the_meeting() {
        let mut h = Harness::new();
        h.take_mic("zoom.exe");
        h.detected("zoom");
        h.poll();
        h.release_mic("zoom.exe");
        assert!(h.poll_for(Duration::from_secs(10)).is_empty());
        h.take_mic("zoom.exe");
        assert!(h.poll_for(Duration::from_secs(60)).is_empty());
    }

    #[test]
    fn retaking_the_microphone_withdraws_the_offer() {
        let mut h = Harness::new();
        h.detected("zoom");
        h.poll_until(Duration::from_secs(60)).unwrap();
        h.take_mic("zoom.exe");
        assert_eq!(h.poll(), vec![Action::WithdrawStopOffer]);
    }

    #[test]
    fn a_manual_recording_is_never_stopped() {
        let mut h = Harness::new();
        h.recording = Recording::Other;
        h.take_mic("zoom.exe");
        h.poll_for(Duration::from_secs(10));
        h.release_mic("zoom.exe");
        assert!(h.poll_for(Duration::from_secs(120)).is_empty());
    }

    #[test]
    fn stopping_the_detected_recording_does_not_prompt_again_for_the_call() {
        let mut h = Harness::new();
        h.take_mic("zoom.exe");
        h.poll_until(Duration::from_secs(10)).unwrap();
        h.engine.answer("zoom", Answer::Record);
        h.detected("zoom");
        h.poll();
        h.recording = Recording::None;
        assert!(h.poll_for(Duration::from_secs(60)).is_empty());
    }

    #[test]
    fn one_prompt_at_a_time() {
        let mut h = Harness::new();
        h.take_mic("zoom.exe");
        h.take_mic("slack.exe");
        let (_, actions) = h.poll_until(Duration::from_secs(10)).unwrap();
        assert_eq!(actions.len(), 1);
        assert!(h.poll_for(Duration::from_secs(10)).is_empty());
        // Once answered, the other app gets its turn.
        h.engine.answer("zoom", Answer::Ignore);
        let (_, actions) = h.poll_until(Duration::from_secs(10)).unwrap();
        assert_eq!(actions, vec![prompt("slack", "Slack")]);
    }

    #[test]
    fn a_start_that_did_not_happen_frees_the_other_apps() {
        let mut h = Harness::new();
        h.take_mic("zoom.exe");
        h.poll_until(Duration::from_secs(10)).unwrap();
        h.engine.answer("zoom", Answer::Record);
        // Cancelled at the consent reminder (or failed, or refused).
        h.engine.start_finished("zoom", false);

        h.take_mic("slack.exe");
        let (_, actions) = h.poll_until(Duration::from_secs(10)).unwrap();
        assert_eq!(actions, vec![prompt("slack", "Slack")]);
        // Zoom itself is not prompted again for this call.
        h.engine.answer("slack", Answer::Ignore);
        assert!(h.poll_for(Duration::from_secs(60)).is_empty());
    }

    #[test]
    fn a_failed_auto_start_frees_the_other_apps() {
        let mut h = Harness::new();
        h.auto_start = true;
        h.take_mic("zoom.exe");
        h.poll_until(Duration::from_secs(10)).unwrap();
        h.engine.start_finished("zoom", false);
        h.take_mic("slack.exe");
        let (_, actions) = h.poll_until(Duration::from_secs(10)).unwrap();
        assert!(matches!(
            actions.as_slice(),
            [Action::StartRecording {
                app_id: "slack",
                ..
            }]
        ));
    }

    #[test]
    fn a_successful_start_keeps_blocking_until_the_recording_shows_up() {
        let mut h = Harness::new();
        h.take_mic("zoom.exe");
        h.poll_until(Duration::from_secs(10)).unwrap();
        h.engine.answer("zoom", Answer::Record);
        h.engine.start_finished("zoom", true);
        h.take_mic("slack.exe");
        assert!(h.poll_for(Duration::from_secs(10)).is_empty());
    }

    /// A detected recording whose app released the microphone long enough
    /// for the countdown to be offered.
    fn countdown_offered() -> Harness {
        let mut h = Harness::new();
        h.auto_stop = MeetingAutoStop::Auto;
        h.detected("zoom");
        let (_, actions) = h.poll_until(Duration::from_secs(60)).unwrap();
        assert!(matches!(
            actions.as_slice(),
            [Action::OfferStop { auto: true, .. }]
        ));
        h
    }

    #[test]
    fn switching_to_never_withdraws_the_countdown() {
        let mut h = countdown_offered();
        h.auto_stop = MeetingAutoStop::Never;
        assert_eq!(h.poll(), vec![Action::WithdrawStopOffer]);
        assert!(h.poll_for(Duration::from_secs(60)).is_empty());
    }

    #[test]
    fn switching_to_ask_replaces_the_countdown_with_the_question() {
        let mut h = countdown_offered();
        h.auto_stop = MeetingAutoStop::Ask;
        assert_eq!(
            h.poll(),
            vec![
                Action::WithdrawStopOffer,
                Action::OfferStop {
                    meeting_id: "m1".into(),
                    app_name: "Zoom",
                    auto: false
                }
            ]
        );
        assert!(h.poll_for(Duration::from_secs(60)).is_empty());
    }

    #[test]
    fn keep_recording_is_not_offered_again_even_after_a_mode_change() {
        let mut h = countdown_offered();
        h.engine.stop_declined("m1");
        assert!(h.poll_for(Duration::from_secs(60)).is_empty());
        h.auto_stop = MeetingAutoStop::Ask;
        assert!(h.poll_for(Duration::from_secs(60)).is_empty());

        // A new release of the microphone is a new end of meeting.
        h.take_mic("zoom.exe");
        h.poll();
        h.release_mic("zoom.exe");
        let (_, actions) = h.poll_until(Duration::from_secs(60)).unwrap();
        assert!(matches!(
            actions.as_slice(),
            [Action::OfferStop { auto: false, .. }]
        ));
    }

    #[test]
    fn apps_seen_while_a_start_is_pending_are_offered_once_it_failed() {
        let mut h = Harness::new();
        h.take_mic("zoom.exe");
        h.poll_until(Duration::from_secs(10)).unwrap();
        h.engine.answer("zoom", Answer::Record);

        // The consent reminder is still open.
        h.take_mic("slack.exe");
        assert!(h.poll_for(Duration::from_secs(10)).is_empty());
        h.engine.start_finished("zoom", false);

        assert_eq!(h.poll(), vec![prompt("slack", "Slack")]);
    }

    #[test]
    fn a_candidate_before_the_start_request_is_offered_once_it_failed() {
        let mut h = Harness::new();
        h.take_mic("zoom.exe");
        h.take_mic("slack.exe");
        // Zoom prompted, Slack waits as a candidate.
        h.poll_until(Duration::from_secs(10)).unwrap();
        h.engine.answer("zoom", Answer::Record);
        assert!(h.poll_for(Duration::from_secs(10)).is_empty());
        h.engine.start_finished("zoom", false);

        assert_eq!(h.poll(), vec![prompt("slack", "Slack")]);
    }

    #[test]
    fn apps_seen_while_a_start_is_pending_are_covered_once_it_records() {
        let mut h = Harness::new();
        h.take_mic("zoom.exe");
        h.poll_until(Duration::from_secs(10)).unwrap();
        h.engine.answer("zoom", Answer::Record);
        h.take_mic("slack.exe");
        h.poll_for(Duration::from_secs(10));

        h.engine.start_finished("zoom", true);
        h.detected("zoom");
        assert!(h.poll_for(Duration::from_secs(10)).is_empty());
        // Not after the recording ends either: that call was covered.
        h.recording = Recording::None;
        assert!(h.poll_for(Duration::from_secs(60)).is_empty());
    }
}
