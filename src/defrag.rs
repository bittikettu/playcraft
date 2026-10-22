//! A Windows 98-style "Disk Defragmenter" whose progress is the song's progress.
//!
//! Each song gets its own fragmented "disk" (a grid of clusters, seeded from the file
//! path). Defragmenting moves data cluster `k` to cell `k`, one cluster per step; the number
//! of steps done is the fraction of the song played, so at the end of the song every file
//! is contiguous at the start of the disk and the free space is at the end. The state is
//! a pure function of (seed, size, progress), so seeking simply recomputes it.

/// How a cluster looks on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClusterKind {
    Free,
    /// Belongs to a file split into several fragments.
    Fragmented,
    /// Contiguous file that is not at its final place yet.
    Unoptimized,
    /// Defragmented: in its final place.
    Optimized,
    /// Being read by the defragmenter right now.
    Reading,
    /// Being written right now.
    Writing,
}

/// Share of the disk holding data.
const FILL: f64 = 0.6;

#[derive(Debug, Clone)]
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // splitmix64: good spread even for nearby seeds.
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

#[derive(Debug, Default)]
pub struct Defrag {
    seed: u64,
    cells: usize,
    /// Initial layout: cell -> data cluster id.
    initial: Vec<Option<u32>>,
    /// Current layout and its inverse.
    layout: Vec<Option<u32>>,
    position: Vec<usize>,
    /// Per data cluster: whether its file was fragmented.
    fragmented: Vec<bool>,
    steps: usize,
    progress: f64,
    /// How many clusters the read head grabs at once (louder music reads more).
    burst: usize,
}

impl Defrag {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of data clusters on the disk.
    pub fn data_clusters(&self) -> usize {
        self.fragmented.len()
    }

    pub fn cells(&self) -> usize {
        self.cells
    }

    /// Builds a fresh fragmented disk with `cells` clusters for the song identified by
    /// `seed`. Does nothing if neither changed.
    pub fn configure(&mut self, cells: usize, seed: u64) {
        if cells == self.cells && seed == self.seed && !self.initial.is_empty() {
            return;
        }
        self.cells = cells;
        self.seed = seed;
        let mut rng = Rng(seed);
        let data = ((cells as f64) * FILL).round() as usize;

        // Split the data into files, and each file into one or more fragments.
        let max_file = (data / 25).max(2);
        let mut fragments: Vec<(u32, usize, bool)> = Vec::new(); // (first id, len, fragmented)
        let mut id = 0usize;
        while id < data {
            let len = (1 + rng.below(max_file)).min(data - id);
            let pieces = if len > 1 && rng.below(3) > 0 {
                2 + rng.below(3.min(len - 1))
            } else {
                1
            };
            let pieces = pieces.min(len);
            let mut start = id;
            for p in 0..pieces {
                let remaining = len - (start - id);
                let size = if p + 1 == pieces {
                    remaining
                } else {
                    (1 + rng.below(remaining - (pieces - p - 1))).max(1)
                };
                fragments.push((start as u32, size, pieces > 1));
                start += size;
            }
            id += len;
        }
        // Scatter the fragments over the disk in random order with random gaps.
        for i in (1..fragments.len()).rev() {
            fragments.swap(i, rng.below(i + 1));
        }
        let free = cells.saturating_sub(data);
        let avg_gap = free as f64 / fragments.len().max(1) as f64;
        self.initial = vec![None; cells];
        self.fragmented = vec![false; data];
        let mut cursor = 0usize;
        let mut free_left = free;
        for (first, len, split) in fragments {
            let gap = rng
                .below((avg_gap * 2.0).round() as usize + 1)
                .min(free_left);
            free_left -= gap;
            cursor += gap;
            for k in 0..len {
                let cid = first as usize + k;
                self.initial[cursor] = Some(cid as u32);
                self.fragmented[cid] = split;
                cursor += 1;
            }
        }
        self.steps = usize::MAX; // force a rebuild
        let progress = self.progress;
        self.progress = -1.0;
        self.set_progress(progress, self.burst);
    }

    fn rebuild(&mut self) {
        self.layout = self.initial.clone();
        self.position = vec![0; self.fragmented.len()];
        for (cell, slot) in self.layout.iter().enumerate() {
            if let Some(id) = slot {
                self.position[*id as usize] = cell;
            }
        }
        self.steps = 0;
    }

    /// Moves data cluster `k` to cell `k`, swapping out whatever was there.
    fn step(&mut self) {
        let k = self.steps;
        let src = self.position[k];
        if src != k {
            let occupant = self.layout[k];
            self.layout[k] = Some(k as u32);
            self.layout[src] = occupant;
            if let Some(o) = occupant {
                self.position[o as usize] = src;
            }
            self.position[k] = k;
        }
        self.steps += 1;
    }

    /// Sets the song progress (0.0–1.0). `burst` is how many clusters are highlighted as
    /// being read (1 for quiet passages, more when loud).
    pub fn set_progress(&mut self, progress: f64, burst: usize) {
        self.burst = burst.max(1);
        self.progress = progress.clamp(0.0, 1.0);
        let data = self.fragmented.len();
        let target = ((self.progress * data as f64).floor() as usize).min(data);
        if target < self.steps || self.layout.len() != self.cells {
            self.rebuild();
        }
        while self.steps < target {
            self.step();
        }
    }

    pub fn percent(&self) -> u8 {
        (self.progress * 100.0).floor() as u8
    }

    pub fn is_complete(&self) -> bool {
        self.steps >= self.fragmented.len() && !self.fragmented.is_empty()
    }

    /// How cell `i` should be drawn.
    pub fn kind(&self, i: usize) -> ClusterKind {
        let data = self.fragmented.len();
        let s = self.steps;
        if s < data {
            // Within a step: first read the next clusters, then write the target cell.
            let phase = self.progress * data as f64 - s as f64;
            if phase >= 0.5 && i == s {
                return ClusterKind::Writing;
            }
            let reading_end = (s + self.burst).min(data);
            if (s..reading_end).any(|k| self.position[k] == i) && i != s {
                return ClusterKind::Reading;
            }
        }
        match self.layout.get(i).copied().flatten() {
            None => ClusterKind::Free,
            Some(id) if (id as usize) < s => ClusterKind::Optimized,
            Some(id) if self.fragmented[id as usize] => ClusterKind::Fragmented,
            Some(_) => ClusterKind::Unoptimized,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(d: &Defrag) -> Vec<ClusterKind> {
        (0..d.cells()).map(|i| d.kind(i)).collect()
    }

    fn count(d: &Defrag, k: ClusterKind) -> usize {
        kinds(d).into_iter().filter(|&x| x == k).count()
    }

    #[test]
    fn starts_fragmented_and_ends_fully_defragmented() {
        let mut d = Defrag::new();
        d.configure(1200, 42);
        d.set_progress(0.0, 1);
        let data = d.data_clusters();
        assert_eq!(data, 720);
        assert_eq!(count(&d, ClusterKind::Optimized), 0);
        assert!(
            count(&d, ClusterKind::Fragmented) > 100,
            "disk should start fragmented"
        );
        assert!(count(&d, ClusterKind::Free) > 400);

        d.set_progress(1.0, 1);
        assert!(d.is_complete());
        assert_eq!(d.percent(), 100);
        let k = kinds(&d);
        assert!(k[..data].iter().all(|&x| x == ClusterKind::Optimized));
        assert!(k[data..].iter().all(|&x| x == ClusterKind::Free));
    }

    #[test]
    fn data_is_never_lost_and_progress_only_adds_optimized_clusters() {
        let mut d = Defrag::new();
        d.configure(800, 7);
        let mut last = 0;
        for i in 0..=50 {
            d.set_progress(i as f64 / 50.0, 3);
            let used = (0..d.cells()).filter(|&c| d.layout[c].is_some()).count();
            assert_eq!(used, d.data_clusters());
            let optimized = count(&d, ClusterKind::Optimized);
            assert!(optimized >= last);
            last = optimized;
        }
    }

    #[test]
    fn seeking_back_matches_a_fresh_disk() {
        let mut a = Defrag::new();
        a.configure(600, 9);
        a.set_progress(0.8, 1);
        a.set_progress(0.3, 1);
        let mut b = Defrag::new();
        b.configure(600, 9);
        b.set_progress(0.3, 1);
        assert_eq!(kinds(&a), kinds(&b));
    }

    #[test]
    fn each_song_gets_its_own_disk() {
        let mut a = Defrag::new();
        a.configure(500, 1);
        let mut b = Defrag::new();
        b.configure(500, 2);
        assert_ne!(kinds(&a), kinds(&b));
        let mut c = Defrag::new();
        c.configure(500, 1);
        assert_eq!(kinds(&a), kinds(&c));
    }

    #[test]
    fn shows_read_and_write_activity_mid_song() {
        let mut d = Defrag::new();
        d.configure(1000, 5);
        let data = d.data_clusters() as f64;
        // Just past the middle of a step: writing the target cell.
        d.set_progress((300.0 + 0.75) / data, 4);
        assert_eq!(d.kind(300), ClusterKind::Writing);
        // Early in a step: reading clusters ahead.
        d.set_progress((300.0 + 0.25) / data, 4);
        assert!(count(&d, ClusterKind::Reading) >= 1);
    }

    #[test]
    fn tiny_disks_do_not_panic() {
        for cells in [0, 1, 2, 3, 10] {
            let mut d = Defrag::new();
            d.configure(cells, 3);
            for p in [0.0, 0.5, 1.0] {
                d.set_progress(p, 2);
                let _ = kinds(&d);
            }
        }
    }
}
