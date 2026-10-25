//! Keeping the local playback, which is muted while a speaker plays but still drives the
//! visualizers, the progress bar and the play order, in step with the speaker.
//!
//! A speaker reports its position in whole seconds. A report of `r` means the speaker was
//! somewhere in `[r, r + 1)` at a moment between asking and answering, which bounds the
//! offset between the speaker and the local position. Both clocks run at the same rate, so
//! the bounds of successive reports intersect and pin the offset down to a fraction of a
//! second within a few reports. Bounds that no longer meet mean the speaker jumped (a seek,
//! a stall), and the estimate starts over.

/// The local playback trails the speaker by this much (seconds), so a track always ends on
/// the speaker before the local playback moves on to the next one.
pub const LAG: f64 = 0.1;
/// Offsets off by less than this (seconds) are left alone.
const TOLERANCE: f64 = 0.15;
/// Timing noise (seconds): the local position is updated every few milliseconds.
const SLACK: f64 = 0.05;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Offset {
    /// Lowest and highest possible offset (speaker minus local), in seconds.
    bounds: Option<(f64, f64)>,
}

impl Offset {
    /// Adds a report: the speaker said `reported` whole seconds, asked `asked_ago` and
    /// answered `answered_ago` seconds ago; the local position is `local` seconds now.
    pub fn observe(&mut self, reported: f64, asked_ago: f64, answered_ago: f64, local: f64) {
        let lo = reported + answered_ago - local - SLACK;
        let hi = reported + 1.0 + asked_ago - local + SLACK;
        self.bounds = match self.bounds {
            Some((a, b)) if a.max(lo) <= b.min(hi) => Some((a.max(lo), b.min(hi))),
            _ => Some((lo, hi)),
        };
    }

    pub fn reset(&mut self) {
        self.bounds = None;
    }

    /// The offset (speaker minus local, seconds) and how far off it may be.
    pub fn estimate(&self) -> Option<(f64, f64)> {
        self.bounds
            .map(|(lo, hi)| ((lo + hi) / 2.0, (hi - lo) / 2.0))
    }

    /// How far to move the local playback (seconds, + is forward) to trail the speaker by
    /// `LAG`, when it is certainly off by more than the tolerance.
    pub fn correction(&self) -> Option<f64> {
        let (offset, uncertainty) = self.estimate()?;
        let error = offset - LAG;
        (error.abs() - uncertainty > TOLERANCE).then_some(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reports from a speaker `true_offset` seconds ahead of the local playback, asked about
    /// every half second (half a second after the last answer) with 20 ms round trips.
    fn simulate(true_offset: f64, reports: usize) -> Offset {
        let mut offset = Offset::default();
        let mut local = 10.0;
        for _ in 0..reports {
            local += 0.53;
            // Asked at local - 0.02, answered at local, the speaker read in between.
            let speaker_at_read = local - 0.01 + true_offset;
            offset.observe(speaker_at_read.floor(), 0.02, 0.0, local);
        }
        offset
    }

    #[test]
    fn narrows_down_to_the_true_offset() {
        for true_offset in [-1.3, -0.4, 0.0, 0.1, 0.77, 2.5] {
            let offset = simulate(true_offset, 12);
            let (estimate, uncertainty) = offset.estimate().unwrap();
            assert!(
                (estimate - true_offset).abs() <= uncertainty && uncertainty < 0.3,
                "{true_offset}: {estimate} ± {uncertainty}"
            );
        }
    }

    #[test]
    fn corrects_only_clear_differences() {
        // Trailing by about LAG: nothing to do.
        assert_eq!(simulate(LAG, 12).correction(), None);
        // The speaker is 1.3 s ahead (it started later): move forward. The first report is
        // already enough for a difference this size.
        let c = simulate(1.3, 1).correction().unwrap();
        assert!(c > 0.5, "{c}");
        let c = simulate(1.3, 12).correction().unwrap();
        assert!((c - (1.3 - LAG)).abs() < 0.3, "{c}");
        // The local playback is ahead: move back.
        assert!(simulate(-0.8, 12).correction().unwrap() < -0.6);
        // A single report cannot tell 0.4 s from LAG.
        assert_eq!(simulate(0.4, 1).correction(), None);
    }

    #[test]
    fn a_jump_starts_over() {
        let mut offset = simulate(0.2, 10);
        // The speaker was seeked 30 s ahead.
        offset.observe(45.0, 0.02, 0.0, 15.2);
        let (estimate, _) = offset.estimate().unwrap();
        assert!(estimate > 29.0, "{estimate}");
    }
}
