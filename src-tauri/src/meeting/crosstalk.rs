//! Cross-talk de-duplication for the two-track (mic + system loopback) path.
//!
//! Ported from fly-on-the-wall's `crates/fly-core/src/crosstalk.rs`
//! (<https://github.com/swarnavspujari/fly-on-the-wall>), MIT License,
//! Copyright (c) 2026 WizzyWYG. The word-level matcher and its tests are
//! fly's; the segment-level fallback ([`drop_echoed_segments`]) is ours, for
//! models that return no word timestamps.
//!
//! On a headset-less recording the built-in mic naturally re-captures the
//! far-end voice played through the speakers, so the mic track transcribes
//! BOTH sides of the call (all labelled "Me") and the same words are
//! transcribed again on the system track. Merged unchanged, every far-end
//! word is double-counted and mis-attributed.
//!
//! Echo is detected as a *run* of the same words appearing on both tracks
//! within a short window: genuine talk-over (DIFFERENT words on the two
//! tracks at the same instant) has no cross-track token match and survives
//! untouched.
//!
//! Known, bounded limitations (from fly's measurements):
//! - An ISOLATED far-end echo word (a run of 1, below `min_run`) is not
//!   removed, so a lone echoed backchannel can survive on both tracks.
//! - The greedy 1:1 token matcher can leave one word of an echo run unmatched
//!   when the mic holds more copies of a token than the system within the
//!   window, splitting the run so a boundary word survives.
//! - A near speaker who mirrors 2+ of the far-end's exact words inside the
//!   window can be misread as echo (see [`CrosstalkOptions::window_ms`]).

use super::transcript::{TimedSegment, TimedWord};

#[derive(Debug, Clone)]
pub struct CrosstalkOptions {
    /// A mic word and a system word with the same token whose starts differ by
    /// at most this are echo candidates. Wide, because the two tracks are
    /// decoded independently and the same echoed word gets timestamps up to
    /// ~1 s apart.
    pub window_ms: u64,
    /// Minimum length of a run of consecutive twinned mic words for it to count
    /// as echo. A run of 1 is treated as a coincidence (a shared "yeah").
    pub min_run: usize,
    /// Which track keeps the far-end copy of a duplicated run. `true` keeps
    /// the mic's copy (re-attributed to the far end) and drops the system
    /// copy; `false` keeps the system copy and drops the mic echo.
    pub keep_mic_far_end: bool,
    /// Only de-duplicate when at least this fraction of the system words are
    /// echoed into the mic. A headset recording has no acoustic echo, so its
    /// few cross-track matches are coincidental and left alone.
    pub min_echo_ratio: f64,
    /// Segment-level fallback: a mic segment is echo when more than this
    /// fraction of its tokens are found in the system segments around it.
    pub segment_overlap: f64,
}

impl Default for CrosstalkOptions {
    fn default() -> Self {
        // fly's production values, measured against a human-verified
        // reference: keeping the system copy gave fewer substitutions than the
        // mic's acoustic echo, and 1500 ms halved the merged WER against
        // 700 ms.
        Self {
            window_ms: 1_500,
            min_run: 2,
            keep_mic_far_end: false,
            min_echo_ratio: 0.15,
            segment_overlap: 0.6,
        }
    }
}

/// The two de-duplicated word streams: the near speaker ("Me") and every
/// far-end word, each kept once ("Others").
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelSplit {
    pub you_words: Vec<TimedWord>,
    pub far_words: Vec<TimedWord>,
}

/// Comparison token: lowercase alphanumerics and apostrophes, so "Boat." and
/// "boat" match. A token with no alphanumeric content keeps its trimmed text.
pub fn loop_token(text: &str) -> String {
    let norm: String = text
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'')
        .flat_map(char::to_lowercase)
        .collect();
    if norm.is_empty() {
        text.trim().to_string()
    } else {
        norm
    }
}

/// Split the mic and system word streams into a near ("you") and far-end
/// stream with cross-talk echo removed. Words are assumed sorted by start time.
pub fn split_crosstalk(
    mic: &[TimedWord],
    sys: &[TimedWord],
    opts: &CrosstalkOptions,
) -> ChannelSplit {
    let mic_tok: Vec<String> = mic.iter().map(|w| loop_token(&w.text)).collect();
    let sys_tok: Vec<String> = sys.iter().map(|w| loop_token(&w.text)).collect();

    // 1. Greedy time-windowed token matching: pair each mic word with at most
    //    one, closest-in-time, unused system word carrying the same token.
    //    Both streams are sorted, so a single forward-sliding window is linear.
    let mut sys_used = vec![false; sys.len()];
    let mut mic_match: Vec<Option<usize>> = vec![None; mic.len()];
    let mut lo = 0usize;
    for (i, mw) in mic.iter().enumerate() {
        while lo < sys.len() && sys[lo].start_ms + opts.window_ms < mw.start_ms {
            lo += 1;
        }
        let mut best: Option<(usize, u64)> = None;
        let mut k = lo;
        while k < sys.len() && sys[k].start_ms <= mw.start_ms + opts.window_ms {
            if !sys_used[k] && sys_tok[k] == mic_tok[i] {
                let d = sys[k].start_ms.abs_diff(mw.start_ms);
                if best.map(|(_, bd)| d < bd).unwrap_or(true) {
                    best = Some((k, d));
                }
            }
            k += 1;
        }
        if let Some((k, _)) = best {
            sys_used[k] = true;
            mic_match[i] = Some(k);
        }
    }

    // 2. Echo = a run of `min_run`+ CONSECUTIVE matched mic words. A lone match
    //    (a shared backchannel, a coincidental common word, talk-over) is not.
    let mut mic_echo = vec![false; mic.len()];
    let mut i = 0;
    while i < mic.len() {
        if mic_match[i].is_some() {
            let run_start = i;
            while i < mic.len() && mic_match[i].is_some() {
                i += 1;
            }
            if i - run_start >= opts.min_run {
                mic_echo[run_start..i].fill(true);
            }
        } else {
            i += 1;
        }
    }

    // 3. A system word is echo only if its partner mic word is an echo word.
    let mut sys_echo = vec![false; sys.len()];
    for (i, m) in mic_match.iter().enumerate() {
        if let (true, Some(k)) = (mic_echo[i], m) {
            sys_echo[*k] = true;
        }
    }

    // Safety gate: barely any of the far end echoed into the mic means a
    // clean/headset recording; its few matches are coincidental, so leave
    // BOTH streams untouched rather than delete genuine same-phrase speech.
    if !sys.is_empty() {
        let echoed = sys_echo.iter().filter(|&&e| e).count();
        if (echoed as f64) < opts.min_echo_ratio * sys.len() as f64 {
            return ChannelSplit {
                you_words: mic.to_vec(),
                far_words: sys.to_vec(),
            };
        }
    }

    // 4. Assemble. `you` is always the mic minus its echo. The far end is
    //    sourced per the keep-direction.
    let you_words: Vec<TimedWord> = mic
        .iter()
        .zip(&mic_echo)
        .filter(|(_, echo)| !**echo)
        .map(|(w, _)| w.clone())
        .collect();

    let mut far_words: Vec<TimedWord> = if opts.keep_mic_far_end {
        let from_mic = mic
            .iter()
            .zip(&mic_echo)
            .filter(|(_, echo)| **echo)
            .map(|(w, _)| w.clone());
        let residual_sys = sys
            .iter()
            .zip(&sys_echo)
            .filter(|(_, echo)| !**echo)
            .map(|(w, _)| w.clone());
        from_mic.chain(residual_sys).collect()
    } else {
        sys.to_vec()
    };
    far_words.sort_by_key(|w| (w.start_ms, w.end_ms));

    ChannelSplit {
        you_words,
        far_words,
    }
}

/// Segment-level echo removal, for models without word timestamps: the mic
/// segments that are not echo of the system track. A mic segment is echo when
/// more than `segment_overlap` of its tokens appear in the system segments
/// overlapping it (widened by `window_ms`, since the tracks' timestamps
/// drift). As with `min_run` for words, a segment of fewer than `min_run`
/// tokens is never echo: a lone "yes" is as likely the user's own.
pub fn drop_echoed_segments(
    mic: &[TimedSegment],
    sys: &[TimedSegment],
    opts: &CrosstalkOptions,
) -> Vec<TimedSegment> {
    mic.iter()
        .filter(|segment| !is_echo_segment(segment, sys, opts))
        .cloned()
        .collect()
}

fn tokens(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(loop_token)
        .filter(|t| t.chars().any(char::is_alphanumeric))
        .collect()
}

fn is_echo_segment(segment: &TimedSegment, sys: &[TimedSegment], opts: &CrosstalkOptions) -> bool {
    let mic_tokens = tokens(&segment.text);
    if mic_tokens.len() < opts.min_run.max(1) {
        return false;
    }
    let from = segment.start_ms.saturating_sub(opts.window_ms);
    let to = segment.end_ms + opts.window_ms;
    // A multiset: each system token can only twin one mic token.
    let mut pool: Vec<String> = sys
        .iter()
        .filter(|s| s.start_ms <= to && s.end_ms >= from)
        .flat_map(|s| tokens(&s.text))
        .collect();
    if pool.is_empty() {
        return false;
    }
    let mut matched = 0usize;
    for token in &mic_tokens {
        if let Some(at) = pool.iter().position(|p| p == token) {
            pool.swap_remove(at);
            matched += 1;
        }
    }
    matched as f64 > opts.segment_overlap * mic_tokens.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, start: u64, end: u64) -> TimedWord {
        TimedWord {
            text: text.into(),
            start_ms: start,
            end_ms: end,
        }
    }

    fn s(text: &str, start: u64, end: u64) -> TimedSegment {
        TimedSegment {
            text: text.into(),
            start_ms: start,
            end_ms: end,
        }
    }

    fn texts(ws: &[TimedWord]) -> Vec<&str> {
        ws.iter().map(|x| x.text.as_str()).collect()
    }

    // Core-logic tests disable the echo-ratio gate (min_echo_ratio: 0.0) so they
    // exercise the matching/run logic directly; the gate has its own tests.
    fn opts(min_run: usize, keep_mic: bool) -> CrosstalkOptions {
        CrosstalkOptions {
            min_run,
            keep_mic_far_end: keep_mic,
            min_echo_ratio: 0.0,
            ..CrosstalkOptions::default()
        }
    }

    #[test]
    fn defaults_match_fly() {
        let o = CrosstalkOptions::default();
        assert_eq!(o.window_ms, 1_500);
        assert_eq!(o.min_run, 2);
    }

    #[test]
    fn loop_token_normalizes_case_and_punctuation() {
        assert_eq!(loop_token("Boat."), "boat");
        assert_eq!(loop_token("I'm"), "i'm");
        assert_eq!(loop_token("  Test,"), "test");
        assert_eq!(loop_token(" ... "), "...");
    }

    #[test]
    fn no_crosstalk_is_a_noop_in_both_directions() {
        let mic = vec![w("hello", 0, 300), w("world", 350, 700)];
        let sys = vec![w("goodbye", 1_000, 1_300), w("now", 1_350, 1_700)];
        for keep_mic in [false, true] {
            let split = split_crosstalk(&mic, &sys, &opts(2, keep_mic));
            assert_eq!(texts(&split.you_words), vec!["hello", "world"]);
            assert_eq!(texts(&split.far_words), vec!["goodbye", "now"]);
        }
    }

    #[test]
    fn echo_run_keep_system_drops_mic_copy() {
        let mic = vec![
            w("hi", 0, 200),
            w("the", 1_050, 1_200),
            w("quick", 1_350, 1_500),
            w("fox", 1_650, 1_800),
        ];
        let sys = vec![
            w("the", 1_000, 1_150),
            w("quick", 1_300, 1_450),
            w("fox", 1_600, 1_750),
        ];
        let split = split_crosstalk(&mic, &sys, &opts(2, false));
        assert_eq!(texts(&split.you_words), vec!["hi"]);
        assert_eq!(texts(&split.far_words), vec!["the", "quick", "fox"]);
        assert_eq!(split.far_words[0].start_ms, 1_000);
    }

    #[test]
    fn echo_run_keep_mic_keeps_mic_copy() {
        let mic = vec![
            w("hi", 0, 200),
            w("the", 1_050, 1_200),
            w("quick", 1_350, 1_500),
            w("fox", 1_650, 1_800),
        ];
        let sys = vec![
            w("the", 1_000, 1_150),
            w("quick", 1_300, 1_450),
            w("fox", 1_600, 1_750),
        ];
        let split = split_crosstalk(&mic, &sys, &opts(2, true));
        assert_eq!(texts(&split.you_words), vec!["hi"]);
        assert_eq!(texts(&split.far_words), vec!["the", "quick", "fox"]);
        assert_eq!(split.far_words[0].start_ms, 1_050);
    }

    #[test]
    fn talk_over_survives() {
        let mic = vec![
            w("the", 1_050, 1_200),
            w("yeah", 1_260, 1_340),
            w("fox", 1_650, 1_800),
        ];
        let sys = vec![w("the", 1_000, 1_150), w("fox", 1_600, 1_750)];
        let split = split_crosstalk(&mic, &sys, &opts(2, false));
        assert!(texts(&split.you_words).contains(&"yeah"));
    }

    #[test]
    fn keep_mic_reattributes_run_and_keeps_system_residual() {
        let mic = vec![
            w("hello", 0, 300),
            w("the", 1_000, 1_150),
            w("fox", 1_600, 1_750),
        ];
        let sys = vec![
            w("the", 1_000, 1_150),
            w("quick", 1_300, 1_450),
            w("fox", 1_600, 1_750),
        ];
        let split = split_crosstalk(&mic, &sys, &opts(2, true));
        assert_eq!(texts(&split.you_words), vec!["hello"]);
        assert_eq!(texts(&split.far_words), vec!["the", "quick", "fox"]);
    }

    #[test]
    fn min_run_2_keeps_a_lone_coincidental_match() {
        let mic = vec![w("okay", 1_000, 1_200)];
        let sys = vec![w("okay", 1_100, 1_300)];
        let split = split_crosstalk(&mic, &sys, &opts(2, false));
        assert_eq!(texts(&split.you_words), vec!["okay"]);
        assert_eq!(texts(&split.far_words), vec!["okay"]);
    }

    #[test]
    fn min_run_1_dedups_even_a_lone_match() {
        let mic = vec![w("okay", 1_000, 1_200)];
        let sys = vec![w("okay", 1_100, 1_300)];
        let split = split_crosstalk(&mic, &sys, &opts(1, false));
        assert!(split.you_words.is_empty());
        assert_eq!(texts(&split.far_words), vec!["okay"]);
    }

    #[test]
    fn outside_window_is_not_a_match() {
        let mic = vec![w("report", 0, 300), w("budget", 500, 800)];
        let sys = vec![w("report", 5_000, 5_300), w("budget", 5_500, 5_800)];
        let split = split_crosstalk(&mic, &sys, &opts(2, false));
        assert_eq!(texts(&split.you_words), vec!["report", "budget"]);
        assert_eq!(texts(&split.far_words), vec!["report", "budget"]);
    }

    #[test]
    fn far_words_are_sorted_by_start() {
        let mic = vec![
            w("a", 0, 100),
            w("one", 1_000, 1_100),
            w("three", 3_000, 3_100),
        ];
        let sys = vec![
            w("one", 1_000, 1_100),
            w("two", 2_000, 2_100),
            w("three", 3_000, 3_100),
        ];
        let split = split_crosstalk(&mic, &sys, &opts(2, true));
        let starts: Vec<u64> = split.far_words.iter().map(|w| w.start_ms).collect();
        assert!(starts.windows(2).all(|p| p[0] <= p[1]));
    }

    #[test]
    fn empty_inputs_produce_empty_output() {
        let split = split_crosstalk(&[], &[], &CrosstalkOptions::default());
        assert!(split.you_words.is_empty() && split.far_words.is_empty());
    }

    #[test]
    fn low_echo_recording_is_left_untouched_by_the_gate() {
        // 2/20 system words echoed = 0.10 < the 0.15 floor: a no-op.
        let sys: Vec<TimedWord> = (0..20u64)
            .map(|i| w(&format!("s{i}"), i * 1_000, i * 1_000 + 300))
            .collect();
        let mic = vec![
            w("hello", 0, 300),
            w("there", 400, 700),
            w("s3", 3_050, 3_300),
            w("s4", 4_050, 4_300),
            w("bye", 9_000, 9_300),
        ];
        let split = split_crosstalk(&mic, &sys, &CrosstalkOptions::default());
        assert_eq!(split.you_words.len(), mic.len());
        assert_eq!(split.far_words.len(), sys.len());
    }

    #[test]
    fn high_echo_recording_is_deduped_under_the_default_gate() {
        let mic = vec![
            w("hi", 0, 200),
            w("the", 1_050, 1_200),
            w("quick", 1_350, 1_500),
            w("fox", 1_650, 1_800),
        ];
        let sys = vec![
            w("the", 1_000, 1_150),
            w("quick", 1_300, 1_450),
            w("fox", 1_600, 1_750),
        ];
        let split = split_crosstalk(&mic, &sys, &CrosstalkOptions::default());
        assert_eq!(texts(&split.you_words), vec!["hi"]);
        assert_eq!(texts(&split.far_words), vec!["the", "quick", "fox"]);
    }

    #[test]
    fn tokens_match_case_and_punctuation_insensitively() {
        let mic = vec![w("The", 1_050, 1_200), w("Fox.", 1_650, 1_800)];
        let sys = vec![w("the", 1_000, 1_150), w("fox", 1_600, 1_750)];
        let split = split_crosstalk(&mic, &sys, &opts(2, false));
        assert!(split.you_words.is_empty());
        assert_eq!(texts(&split.far_words), vec!["the", "fox"]);
    }

    #[test]
    fn echoed_segment_is_dropped() {
        let mic = vec![
            s("Bonjour à tous.", 0, 1_000),
            s("On commence par le budget du trimestre", 5_200, 8_000),
        ];
        let sys = vec![s("on commence par le budget du trimestre.", 5_000, 7_800)];
        let kept = drop_echoed_segments(&mic, &sys, &CrosstalkOptions::default());
        assert_eq!(kept, vec![s("Bonjour à tous.", 0, 1_000)]);
    }

    #[test]
    fn partially_overlapping_segment_survives() {
        // 2 of 5 tokens shared (40 %) is not echo.
        let mic = vec![s("je pense que le budget", 1_000, 3_000)];
        let sys = vec![s("le budget est validé", 1_200, 3_200)];
        let kept = drop_echoed_segments(&mic, &sys, &CrosstalkOptions::default());
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn segment_outside_window_survives() {
        let mic = vec![s("le budget du trimestre", 0, 2_000)];
        let sys = vec![s("le budget du trimestre", 10_000, 12_000)];
        let kept = drop_echoed_segments(&mic, &sys, &CrosstalkOptions::default());
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn single_word_segment_is_never_echo() {
        let mic = vec![s("Oui.", 1_000, 1_300)];
        let sys = vec![s("oui", 1_000, 1_300)];
        let kept = drop_echoed_segments(&mic, &sys, &CrosstalkOptions::default());
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn segment_overlap_spans_neighbouring_system_segments() {
        // The tracks cut segments differently: the echo spans two system rows.
        let mic = vec![s("on commence par le budget", 1_000, 4_000)];
        let sys = vec![
            s("on commence", 900, 1_800),
            s("par le budget", 1_900, 3_900),
        ];
        let kept = drop_echoed_segments(&mic, &sys, &CrosstalkOptions::default());
        assert!(kept.is_empty());
    }
}
