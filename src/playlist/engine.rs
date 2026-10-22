//! Play order (spec §6.1): play modes, per-group shuffle, repeat modes and history.
//!
//! The engine works on track uids, so playlist edits never confuse it. An order covers a
//! *scope* (the whole playlist, or the group playback was started in) and is rebuilt when
//! the playlist, the mode or the scope changes. Randomised orders remember which tracks
//! were already played in the current cycle, so a rebuild never repeats them.

use std::collections::HashSet;

use rand::SeedableRng;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;

use crate::model::{Group, PlayMode, RepeatMode};

const HISTORY_LIMIT: usize = 1000;

pub struct Engine {
    rng: StdRng,
    order: Vec<u64>,
    built: Option<(PlayMode, Vec<usize>, u64)>,
    randomized: bool,
    /// Group playback was started in; the scope for `CurrentGroup` and `RepeatGroup`.
    anchor: Vec<usize>,
    /// Tracks played in the current randomised cycle, in play order.
    played: Vec<u64>,
    history: Vec<u64>,
}

/// The playlist state an order depends on.
#[derive(Clone, Copy)]
pub struct PlayState<'a> {
    pub root: &'a Group,
    pub mode: PlayMode,
    pub repeat: RepeatMode,
    /// Bumped by the caller whenever the playlist is edited.
    pub version: u64,
}

fn unit_tracks(g: &Group, rng: &mut StdRng) -> Vec<u64> {
    let mut v: Vec<u64> = g.tracks.iter().map(|t| t.uid).collect();
    if g.shuffle {
        v.shuffle(rng);
    }
    v
}

/// The play order for the subtree at `scope`. Each group is a "unit" (its own tracks), in
/// pre-order; a group's tracks are shuffled when its shuffle flag is set.
pub fn build_order(root: &Group, scope: &[usize], mode: PlayMode, rng: &mut StdRng) -> Vec<u64> {
    let Some(base) = root.group_at(scope) else { return Vec::new() };
    let mut units = Vec::new();
    base.walk(&mut |g| units.push(g));
    match mode {
        PlayMode::AllTracksShuffled => {
            let mut all: Vec<u64> = units.iter().flat_map(|g| g.tracks.iter().map(|t| t.uid)).collect();
            all.shuffle(rng);
            all
        }
        PlayMode::AllGroupsShuffled => {
            units.shuffle(rng);
            units.iter().flat_map(|g| unit_tracks(g, rng)).collect()
        }
        PlayMode::CurrentGroup | PlayMode::AllGroups => units.iter().flat_map(|g| unit_tracks(g, rng)).collect(),
    }
}

fn playable(root: &Group, uid: u64) -> bool {
    root.track_by_uid(uid).is_some_and(|t| t.is_playable())
}

impl Engine {
    pub fn new(seed: Option<u64>) -> Self {
        Engine {
            rng: seed.map_or_else(rand::make_rng, StdRng::seed_from_u64),
            order: Vec::new(),
            built: None,
            randomized: false,
            anchor: Vec::new(),
            played: Vec::new(),
            history: Vec::new(),
        }
    }

    /// The user started playback of `uid`: its group becomes the anchor for group scopes.
    pub fn anchor_at(&mut self, root: &Group, uid: u64) {
        if let Some(r) = root.locate(uid) {
            if r.group != self.anchor {
                self.anchor = r.group;
                self.played.clear();
            }
        }
    }

    pub fn anchor(&self) -> &[usize] {
        &self.anchor
    }

    /// Moves the anchor after the group tree was rearranged (keeps the shuffle cycle).
    pub fn set_anchor(&mut self, anchor: Vec<usize>) {
        self.anchor = anchor;
    }

    /// Records that `uid` became audible.
    pub fn started(&mut self, uid: u64) {
        if self.history.last() != Some(&uid) {
            self.history.push(uid);
            if self.history.len() > HISTORY_LIMIT {
                self.history.drain(..self.history.len() - HISTORY_LIMIT);
            }
        }
        if self.randomized && !self.played.contains(&uid) {
            self.played.push(uid);
        }
    }

    fn scope(&self, st: &PlayState<'_>) -> Vec<usize> {
        let group_scoped = st.mode == PlayMode::CurrentGroup || st.repeat == RepeatMode::RepeatGroup;
        if group_scoped && st.root.group_at(&self.anchor).is_some() {
            return self.anchor.clone();
        }
        Vec::new()
    }

    fn ensure_order(&mut self, st: &PlayState<'_>, force: bool) {
        let scope = self.scope(st);
        let key = (st.mode, scope.clone(), st.version);
        if !force && self.built.as_ref() == Some(&key) {
            return;
        }
        let fresh = build_order(st.root, &scope, st.mode, &mut self.rng);
        let mut randomized = st.mode == PlayMode::AllTracksShuffled || st.mode == PlayMode::AllGroupsShuffled;
        if let Some(base) = st.root.group_at(&scope) {
            base.walk(&mut |g| randomized |= g.shuffle);
        }
        self.order = if randomized && !force {
            // Keep this cycle's already-played tracks in front so they are not repeated.
            let present: HashSet<u64> = fresh.iter().copied().collect();
            let mut seen = HashSet::new();
            let mut order: Vec<u64> =
                self.played.iter().copied().filter(|u| present.contains(u) && seen.insert(*u)).collect();
            order.extend(fresh.into_iter().filter(|u| !seen.contains(u)));
            order
        } else {
            fresh
        };
        self.randomized = randomized;
        self.built = Some(key);
    }

    /// The track to play when the user asks for the next one (repeat-track is ignored).
    pub fn next(&mut self, st: &PlayState<'_>, current: Option<u64>) -> Option<u64> {
        self.ensure_order(st, false);
        let start = current.and_then(|c| self.order.iter().position(|&u| u == c)).map_or(0, |p| p + 1);
        if let Some(&u) = self.order[start..].iter().find(|&&u| playable(st.root, u)) {
            return Some(u);
        }
        if !matches!(st.repeat, RepeatMode::RepeatAll | RepeatMode::RepeatGroup) {
            return None;
        }
        // Start a new cycle (reshuffled), avoiding an immediate repeat of the current track.
        self.played.clear();
        self.ensure_order(st, true);
        let playable_count = self.order.iter().filter(|&&u| playable(st.root, u)).count();
        self.order
            .iter()
            .copied()
            .find(|&u| playable(st.root, u) && (Some(u) != current || playable_count == 1))
    }

    /// The track to continue with automatically when `current` ends.
    pub fn successor(&mut self, st: &PlayState<'_>, current: u64) -> Option<u64> {
        if st.repeat == RepeatMode::RepeatTrack && playable(st.root, current) {
            return Some(current);
        }
        self.next(st, Some(current))
    }

    /// The previously played track, or the one before `current` in play order.
    pub fn previous(&mut self, st: &PlayState<'_>, current: Option<u64>) -> Option<u64> {
        while let Some(last) = self.history.pop() {
            if Some(last) != current && playable(st.root, last) {
                return Some(last);
            }
        }
        self.ensure_order(st, false);
        let pos = current.and_then(|c| self.order.iter().position(|&u| u == c))?;
        self.order[..pos].iter().rev().copied().find(|&u| playable(st.root, u))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Track;

    /// Root with 2 tracks, "A" with 3 tracks (and subgroup "A1" with 2), "B" with 4.
    fn playlist() -> Group {
        let mut root = Group::new(0, "Root");
        root.tracks = (0..2).map(|i| Track::new(format!("r{i}"))).collect();
        let mut a = Group::new(1, "A");
        a.tracks = (0..3).map(|i| Track::new(format!("a{i}"))).collect();
        let mut a1 = Group::new(2, "A1");
        a1.tracks = (0..2).map(|i| Track::new(format!("a1-{i}"))).collect();
        a.subgroups.push(a1);
        let mut b = Group::new(3, "B");
        b.tracks = (0..4).map(|i| Track::new(format!("b{i}"))).collect();
        root.subgroups.push(a);
        root.subgroups.push(b);
        root
    }

    fn names(root: &Group, uids: &[u64]) -> Vec<String> {
        uids.iter().map(|&u| root.track_by_uid(u).unwrap().path.display().to_string()).collect()
    }

    /// Plays from the start until the engine stops or `limit` tracks have played.
    fn play_through(engine: &mut Engine, st: &PlayState<'_>, limit: usize) -> Vec<u64> {
        let mut out = Vec::new();
        let mut cur = engine.next(st, None);
        while let Some(u) = cur {
            engine.started(u);
            out.push(u);
            if out.len() >= limit {
                break;
            }
            cur = engine.successor(st, u);
        }
        out
    }

    fn state(root: &Group, mode: PlayMode, repeat: RepeatMode) -> PlayState<'_> {
        PlayState { root, mode, repeat, version: 0 }
    }

    #[test]
    fn all_groups_plays_in_playlist_order_and_stops() {
        let root = playlist();
        let mut e = Engine::new(Some(1));
        let played = play_through(&mut e, &state(&root, PlayMode::AllGroups, RepeatMode::Off), 100);
        assert_eq!(
            names(&root, &played),
            ["r0", "r1", "a0", "a1", "a2", "a1-0", "a1-1", "b0", "b1", "b2", "b3"]
        );
    }

    #[test]
    fn every_mode_plays_each_track_exactly_once_per_cycle() {
        let root = playlist();
        for mode in [PlayMode::AllGroups, PlayMode::AllGroupsShuffled, PlayMode::AllTracksShuffled] {
            for seed in 0..20 {
                let mut e = Engine::new(Some(seed));
                let played = play_through(&mut e, &state(&root, mode, RepeatMode::Off), 100);
                let unique: HashSet<_> = played.iter().collect();
                assert_eq!(played.len(), 11, "{mode:?} seed {seed}");
                assert_eq!(unique.len(), 11, "{mode:?} seed {seed}: repeated a track");
            }
        }
    }

    #[test]
    fn shuffled_groups_keep_each_group_together() {
        let root = playlist();
        for seed in 0..20 {
            let mut e = Engine::new(Some(seed));
            let played = play_through(&mut e, &state(&root, PlayMode::AllGroupsShuffled, RepeatMode::Off), 100);
            let groups: Vec<Vec<usize>> = played.iter().map(|&u| root.locate(u).unwrap().group).collect();
            let mut runs = groups.clone();
            runs.dedup();
            assert_eq!(runs.len(), 4, "seed {seed}: a group was split: {groups:?}");
        }
    }

    #[test]
    fn group_shuffle_flag_shuffles_only_that_group() {
        let mut root = playlist();
        root.subgroups[1].shuffle = true;
        let orders: HashSet<Vec<String>> = (0..20)
            .map(|seed| {
                let mut e = Engine::new(Some(seed));
                let played = play_through(&mut e, &state(&root, PlayMode::AllGroups, RepeatMode::Off), 100);
                names(&root, &played)
            })
            .collect();
        assert!(orders.len() > 1, "group B was never shuffled");
        for order in orders {
            assert_eq!(order[..7], ["r0", "r1", "a0", "a1", "a2", "a1-0", "a1-1"]);
        }
    }

    #[test]
    fn current_group_mode_plays_the_anchor_subtree() {
        let root = playlist();
        let mut e = Engine::new(Some(3));
        let a0 = root.subgroups[0].tracks[0].uid;
        e.anchor_at(&root, a0);
        let st = state(&root, PlayMode::CurrentGroup, RepeatMode::Off);
        let mut played = vec![a0];
        e.started(a0);
        while let Some(u) = e.successor(&st, *played.last().unwrap()) {
            e.started(u);
            played.push(u);
        }
        assert_eq!(names(&root, &played), ["a0", "a1", "a2", "a1-0", "a1-1"]);
    }

    #[test]
    fn repeat_modes() {
        let root = playlist();
        let b = &root.subgroups[1];
        let (b0, b3) = (b.tracks[0].uid, b.tracks[3].uid);

        let mut e = Engine::new(Some(0));
        assert_eq!(e.successor(&state(&root, PlayMode::AllGroups, RepeatMode::RepeatTrack), b0), Some(b0));
        // Repeat-track does not stop the user from skipping.
        assert_ne!(e.next(&state(&root, PlayMode::AllGroups, RepeatMode::RepeatTrack), Some(b0)), Some(b0));

        let mut e = Engine::new(Some(0));
        assert_eq!(e.successor(&state(&root, PlayMode::AllGroups, RepeatMode::Off), b3), None);
        let first = root.tracks[0].uid;
        assert_eq!(e.successor(&state(&root, PlayMode::AllGroups, RepeatMode::RepeatAll), b3), Some(first));

        let mut e = Engine::new(Some(0));
        e.anchor_at(&root, b0);
        assert_eq!(e.successor(&state(&root, PlayMode::AllGroups, RepeatMode::RepeatGroup), b3), Some(b0));
    }

    #[test]
    fn repeat_all_reshuffles_without_immediate_repeat() {
        let root = playlist();
        let st = state(&root, PlayMode::AllTracksShuffled, RepeatMode::RepeatAll);
        let mut e = Engine::new(Some(9));
        let played = play_through(&mut e, &st, 33);
        for cycle in played.chunks(11) {
            assert_eq!(cycle.iter().collect::<HashSet<_>>().len(), 11);
        }
        assert!(played.windows(2).all(|w| w[0] != w[1]));
    }

    #[test]
    fn skips_unplayable_tracks_and_stops_when_none_left() {
        let mut root = playlist();
        root.walk_mut(&mut |g| g.tracks.iter_mut().for_each(|t| t.missing = true));
        let mut e = Engine::new(Some(0));
        assert_eq!(e.next(&state(&root, PlayMode::AllGroups, RepeatMode::RepeatAll), None), None);

        let mut root = playlist();
        root.tracks[1].broken = true;
        let r0 = root.tracks[0].uid;
        let a0 = root.subgroups[0].tracks[0].uid;
        let mut e = Engine::new(Some(0));
        assert_eq!(e.successor(&state(&root, PlayMode::AllGroups, RepeatMode::Off), r0), Some(a0));
    }

    #[test]
    fn previous_walks_back_through_history() {
        let root = playlist();
        let st = state(&root, PlayMode::AllTracksShuffled, RepeatMode::Off);
        let mut e = Engine::new(Some(4));
        let played = play_through(&mut e, &st, 3);
        assert_eq!(e.previous(&st, Some(played[2])), Some(played[1]));
        e.started(played[1]);
        assert_eq!(e.previous(&st, Some(played[1])), Some(played[0]));
    }

    #[test]
    fn edits_during_a_shuffle_cycle_do_not_repeat_played_tracks() {
        let mut root = playlist();
        let mut e = Engine::new(Some(5));
        let mut played = Vec::new();
        let mut version = 0;
        let mut cur = e.next(&PlayState { root: &root, mode: PlayMode::AllTracksShuffled, repeat: RepeatMode::Off, version }, None);
        while let Some(u) = cur {
            e.started(u);
            played.push(u);
            if played.len() == 5 {
                // Add a track mid-cycle; the order is rebuilt.
                root.tracks.push(Track::new("new"));
                version += 1;
            }
            let st = PlayState { root: &root, mode: PlayMode::AllTracksShuffled, repeat: RepeatMode::Off, version };
            cur = e.successor(&st, u);
        }
        assert_eq!(played.len(), 12);
        assert_eq!(played.iter().collect::<HashSet<_>>().len(), 12);
    }
}
