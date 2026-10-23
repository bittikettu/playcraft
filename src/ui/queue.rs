//! The play queue: tracks the user lined up to play next. Once it runs out, playback
//! continues in normal order from where it was before the queue (see `App::up_next`).

use super::app::{App, BannerLevel, Overlay, Row};
use super::keymap::Action;

impl App {
    /// Queues the track under the cursor, or takes it out of the queue if it is already
    /// queued. On a group, queues all of its tracks (subgroups included) in play order.
    pub(crate) fn enqueue_at_cursor(&mut self) {
        match self.cursor_row() {
            Some(Row::Track(i)) => {
                let Some(track) = self.current_group().tracks.get(i) else {
                    return;
                };
                let (uid, name, playable) = (track.uid, track.display_name(), track.is_playable());
                if self.take_from_queue(uid) {
                    self.banner(BannerLevel::Info, format!("Unqueued: {name}"));
                } else if !playable {
                    self.banner(BannerLevel::Warning, format!("Cannot queue: {name}"));
                    return;
                } else {
                    self.queue.push(uid);
                    let n = self.queue.len();
                    self.banner(BannerLevel::Info, format!("Queued #{n}: {name}"));
                }
            }
            Some(Row::Group(i)) => {
                let group = &self.current_group().subgroups[i];
                let mut uids = Vec::new();
                group.walk(&mut |g| {
                    uids.extend(g.tracks.iter().filter(|t| t.is_playable()).map(|t| t.uid))
                });
                let name = group.name.clone();
                uids.retain(|u| !self.queue.contains(u));
                let added = uids.len();
                self.queue.extend(uids);
                self.banner(
                    BannerLevel::Info,
                    format!("Queued {added} track(s) from \"{name}\""),
                );
            }
            None => return,
        }
        self.requeue();
    }

    pub(crate) fn clear_queue(&mut self) {
        let n = self.queue.len();
        self.queue.clear();
        self.banner(BannerLevel::Info, format!("Queue cleared ({n} track(s))"));
        self.requeue();
    }

    /// Position (1-based) of `uid` in the queue.
    pub fn queue_position(&self, uid: u64) -> Option<usize> {
        self.queue.iter().position(|&u| u == uid).map(|i| i + 1)
    }

    /// Keys while the queue list is open: move, reorder, remove, play now.
    pub(crate) fn on_queue_key(&mut self, action: Action, cursor: usize) {
        self.prune_queue();
        let last = self.queue.len().saturating_sub(1);
        let cursor = cursor.min(last);
        let mut moved_to = cursor;
        match action {
            Action::CursorUp => moved_to = cursor.saturating_sub(1),
            Action::CursorDown => moved_to = (cursor + 1).min(last),
            Action::PageUp => moved_to = cursor.saturating_sub(10),
            Action::PageDown => moved_to = (cursor + 10).min(last),
            Action::Home => moved_to = 0,
            Action::End => moved_to = last,
            Action::MoveUp | Action::MoveDown if !self.queue.is_empty() => {
                let to = if action == Action::MoveUp {
                    cursor.saturating_sub(1)
                } else {
                    (cursor + 1).min(last)
                };
                self.queue.swap(cursor, to);
                moved_to = to;
                self.requeue();
            }
            Action::Delete | Action::Enqueue if !self.queue.is_empty() => {
                self.queue.remove(cursor);
                moved_to = cursor.min(self.queue.len().saturating_sub(1));
                self.requeue();
            }
            Action::ClearQueue => {
                self.clear_queue();
                moved_to = 0;
            }
            // Plays it now; the rest of the queue stays.
            Action::Activate => {
                if let Some(&uid) = self.queue.get(cursor) {
                    self.play(uid, false);
                }
            }
            Action::Close | Action::Back | Action::ShowQueue | Action::Quit | Action::Help => {
                self.overlay = Overlay::None;
                return;
            }
            Action::MoveUp | Action::MoveDown | Action::Delete | Action::Enqueue => {}
            // Playback keys keep working while the list is open.
            other => return self.on_action(other),
        }
        self.overlay = Overlay::Queue { cursor: moved_to };
    }
}
