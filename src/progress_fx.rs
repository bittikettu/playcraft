//! The animated progress bar: Pac-Man eating his way through the song, a cat with a
//! rainbow trail, or the song's loudness drawn as it plays. This holds the animation state,
//! fed from the spectrum analyser; `ui/views/progress.rs` draws it.

use crate::hyperspace::BeatDetector;

/// Loudness is remembered at this many points along each song.
const RESOLUTION: usize = 512;

/// How the progress bar is drawn (`p` cycles).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressStyle {
    /// Pac-Man chomps along the bar eating pellets, a ghost on his heels.
    PacMan,
    /// A cat flies ahead of a rainbow trail, into twinkling stars.
    Nyan,
    /// The loudness of the song so far, with a note bouncing to the beat.
    Waveform,
    /// `[=====>-----]`
    Classic,
}

impl ProgressStyle {
    pub const ALL: [ProgressStyle; 4] = [
        ProgressStyle::PacMan,
        ProgressStyle::Nyan,
        ProgressStyle::Waveform,
        ProgressStyle::Classic,
    ];

    pub fn cycle(self) -> Self {
        let i = Self::ALL.iter().position(|&s| s == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    pub fn label(self) -> &'static str {
        match self {
            ProgressStyle::PacMan => "Pac-Man",
            ProgressStyle::Nyan => "Nyan cat",
            ProgressStyle::Waveform => "waveform",
            ProgressStyle::Classic => "classic",
        }
    }

    /// The name in `config.toml` and for AI assistants.
    pub fn name(self) -> &'static str {
        match self {
            ProgressStyle::PacMan => "pacman",
            ProgressStyle::Nyan => "nyan",
            ProgressStyle::Waveform => "waveform",
            ProgressStyle::Classic => "classic",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }

    /// Whether it moves with the music (and so needs the spectrum analysed).
    pub fn reacts(self) -> bool {
        self != ProgressStyle::Classic
    }
}

#[derive(Debug)]
pub struct ProgressFx {
    beat: BeatDetector,
    /// Seconds of animation; it only runs while music plays.
    pub time: f32,
    /// Chomps (or steps) taken: it goes faster with louder music.
    pub phase: f32,
    /// How loud the music is now (smoothed), 0–1.
    pub level: f32,
    /// The loudness heard at each point of the song; negative where nothing was heard.
    history: Vec<f32>,
    track: Option<u64>,
}

impl Default for ProgressFx {
    fn default() -> Self {
        ProgressFx {
            beat: BeatDetector::default(),
            time: 0.0,
            phase: 0.0,
            level: 0.0,
            history: vec![-1.0; RESOLUTION],
            track: None,
        }
    }
}

impl ProgressFx {
    /// 1.0 right after a beat, fading towards 0.
    pub fn pulse(&self) -> f32 {
        self.beat.pulse
    }

    /// Advances the animation by `dt` seconds. `bands` is the spectrum (0–1 each), `at` the
    /// fraction of the song played, `track` identifies the song.
    pub fn update(
        &mut self,
        dt: f32,
        bands: &[f32],
        at: Option<f32>,
        track: Option<u64>,
        playing: bool,
    ) {
        if track != self.track {
            self.track = track;
            self.history.fill(-1.0);
            self.beat.reset_tempo();
        }
        if !playing {
            return;
        }
        self.time += dt;
        let mean = |b: &[f32]| {
            if b.is_empty() {
                0.0
            } else {
                b.iter().sum::<f32>() / b.len() as f32
            }
        };
        let loud = mean(bands).clamp(0.0, 1.0);
        let bass = mean(&bands[..bands.len().div_ceil(4)]);
        self.level += (loud - self.level) * (1.0 - (-dt * 12.0).exp());
        self.beat.update(self.time, dt, bass);
        // About two and a half chomps a second in silence, up to eight when it is loud.
        self.phase += dt * (2.5 + 5.5 * self.level.min(1.0));
        if let Some(at) = at {
            let i = (at.clamp(0.0, 1.0) * (RESOLUTION - 1) as f32) as usize;
            self.history[i] = self.history[i].max(loud);
        }
    }

    /// The loudest moment heard between the fractions `from` and `to` of the song, if any of
    /// it was heard.
    pub fn loudness(&self, from: f32, to: f32) -> Option<f32> {
        let index = |f: f32| ((f.clamp(0.0, 1.0) * RESOLUTION as f32) as usize).min(RESOLUTION);
        let (a, b) = (index(from), index(to).max(index(from) + 1).min(RESOLUTION));
        self.history[a.min(RESOLUTION - 1)..b]
            .iter()
            .copied()
            .filter(|&v| v >= 0.0)
            .reduce(f32::max)
    }

    /// The loudest moment heard in the song so far.
    pub fn peak(&self) -> f32 {
        self.history.iter().copied().fold(0.0, f32::max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles_cycle_through_all_and_back() {
        let mut s = ProgressStyle::PacMan;
        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.push(s.name());
            assert_eq!(ProgressStyle::from_name(s.name()), Some(s));
            s = s.cycle();
        }
        assert_eq!(s, ProgressStyle::PacMan);
        assert_eq!(seen, ["pacman", "nyan", "waveform", "classic"]);
        assert!(!ProgressStyle::Classic.reacts());
        assert_eq!(ProgressStyle::from_name("disco"), None);
    }

    #[test]
    fn remembers_how_loud_each_part_of_the_song_was() {
        let mut fx = ProgressFx::default();
        let quiet = [0.1; 16];
        let loud = [0.8; 16];
        fx.update(0.016, &quiet, Some(0.1), Some(1), true);
        fx.update(0.016, &loud, Some(0.5), Some(1), true);
        assert_eq!(fx.loudness(0.0, 0.05), None, "not heard yet");
        assert!((fx.loudness(0.09, 0.11).unwrap() - 0.1).abs() < 1e-6);
        assert!((fx.loudness(0.0, 1.0).unwrap() - 0.8).abs() < 1e-6);
        assert!((fx.peak() - 0.8).abs() < 1e-6);
        // A new song starts with a clean slate.
        fx.update(0.016, &quiet, Some(0.0), Some(2), true);
        assert_eq!(fx.loudness(0.4, 0.6), None);
    }

    #[test]
    fn moves_only_while_playing_and_faster_when_loud() {
        let mut fx = ProgressFx::default();
        fx.update(1.0, &[0.9; 16], Some(0.2), Some(1), false);
        assert_eq!((fx.time, fx.phase), (0.0, 0.0));
        let mut quiet = ProgressFx::default();
        let mut loud = ProgressFx::default();
        for _ in 0..60 {
            quiet.update(1.0 / 60.0, &[0.0; 16], None, Some(1), true);
            loud.update(1.0 / 60.0, &[0.9; 16], None, Some(1), true);
        }
        assert!(quiet.phase > 2.0 && loud.phase > quiet.phase * 2.0);
    }
}
