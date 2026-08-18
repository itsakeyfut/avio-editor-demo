//! `EditHistory` — a host-owned Do/Undo/Redo history over the demo's editing
//! document (`avio::Timeline`).
//!
//! Phase 2 of migrating the demo onto avio 0.16's edit model. avio ships an
//! `Editor`, but it is not usable by a host with in-place rich edits + drag
//! gestures: it exposes only `current(&self)` and `apply(&Command)`, keeps
//! `history`/`cursor` private, pushes one undo step per command, and offers no
//! amend-in-place, no coalescing, and no way to seat an externally-rebuilt
//! `Timeline` (catalogued as gap **B6**). The public **pure**
//! `avio::apply(&Timeline, &Command)` IS usable, so this owns the history over it
//! and adds the two missing pieces: [`amend`](EditHistory::amend) (replace the
//! current version without a new step — for non-undoable rich edits) and gesture
//! coalescing ([`begin_gesture`](EditHistory::begin_gesture) /
//! [`commit_gesture`](EditHistory::commit_gesture)).
//!
//! `avio::Timeline` is opaque (clips are exposed only as `&[Vec<Clip>]`, fields
//! are private), so every edit yields a **new** `Timeline`: structural edits via
//! `avio::apply`, rich edits by rebuilding and handing the result to `amend`.
//!
//! Unwired in Phase 2 (`#![allow(dead_code)]`); validated by unit tests.
#![allow(dead_code)]

use avio::{Command, EditError, Timeline};

/// Maximum retained undo versions (matches the demo's previous 50-entry cap).
const MAX_HISTORY: usize = 50;

/// A linear snapshot history of [`Timeline`] versions with a cursor.
///
/// The version at `cursor` is current. Structural edits push a new version;
/// [`amend`](Self::amend) replaces the current one in place; a gesture coalesces
/// several live amends into a single undo step.
pub struct EditHistory {
    history: Vec<Timeline>,
    cursor: usize,
    /// Pre-gesture snapshot; `Some` while a coalescing gesture is open.
    gesture_pre: Option<Timeline>,
}

impl EditHistory {
    /// Starts a history at `initial` (not undoable).
    pub fn new(initial: Timeline) -> Self {
        Self {
            history: vec![initial],
            cursor: 0,
            gesture_pre: None,
        }
    }

    /// The current document version.
    pub fn current(&self) -> &Timeline {
        &self.history[self.cursor]
    }

    /// Pushes `next` as a new undo step: discards the redo tail and caps history.
    fn record(&mut self, next: Timeline) {
        self.history.truncate(self.cursor + 1);
        self.history.push(next);
        self.cursor += 1;
        if self.history.len() > MAX_HISTORY {
            self.history.remove(0);
            self.cursor -= 1;
        }
    }

    /// Applies one structural `command` via `avio::apply` as a single undo step.
    ///
    /// # Errors
    /// Returns the [`EditError`] from `avio::apply` when the command is invalid;
    /// the history is left unchanged.
    pub fn apply(&mut self, command: &Command) -> Result<&Timeline, EditError> {
        let next = avio::apply(&self.history[self.cursor], command)?;
        self.record(next);
        Ok(&self.history[self.cursor])
    }

    /// Applies several commands as **one** undo step (for host-composed ops avio
    /// has no single command for — split, ripple, cross-track move). Empty input
    /// is a no-op. Aborts atomically: on the first error nothing is recorded.
    ///
    /// # Errors
    /// Returns the [`EditError`] from the first failing command.
    pub fn apply_batch(&mut self, commands: &[Command]) -> Result<&Timeline, EditError> {
        if commands.is_empty() {
            return Ok(&self.history[self.cursor]);
        }
        let mut next = self.history[self.cursor].clone();
        for command in commands {
            next = avio::apply(&next, command)?;
        }
        self.record(next);
        Ok(&self.history[self.cursor])
    }

    /// Replaces the current version with `next` **without** adding an undo step,
    /// discarding the redo tail. Used for rich, non-undoable edits (colour,
    /// effects, keyframes — rebuilt into a new `Timeline`) and for live drag
    /// feedback. Undoing after an amend returns to the previous committed version;
    /// a later [`apply`](Self::apply) builds on the amended state, so amended rich
    /// edits are preserved across a subsequent structural undo.
    pub fn amend(&mut self, next: Timeline) {
        self.history.truncate(self.cursor + 1);
        self.history[self.cursor] = next;
    }

    /// Opens a coalescing gesture: snapshots the current version so a run of live
    /// [`amend`](Self::amend)s can be committed as a single undo step. Idempotent
    /// while a gesture is already open.
    pub fn begin_gesture(&mut self) {
        if self.gesture_pre.is_none() {
            self.gesture_pre = Some(self.history[self.cursor].clone());
        }
    }

    /// Commits an open gesture: the amended current version becomes one undo step
    /// over the pre-gesture snapshot. No-op if no gesture is open. (The caller
    /// should only commit when the gesture actually changed the document.)
    pub fn commit_gesture(&mut self) {
        if let Some(pre) = self.gesture_pre.take() {
            let post = self.history[self.cursor].clone();
            self.history[self.cursor] = pre;
            self.record(post);
        }
    }

    /// Cancels an open gesture, restoring the pre-gesture version. No-op if none.
    pub fn cancel_gesture(&mut self) {
        if let Some(pre) = self.gesture_pre.take() {
            self.history.truncate(self.cursor + 1);
            self.history[self.cursor] = pre;
        }
    }

    /// Moves back one version, or `None` at the start of history.
    pub fn undo(&mut self) -> Option<&Timeline> {
        if self.cursor == 0 {
            return None;
        }
        self.cursor -= 1;
        Some(&self.history[self.cursor])
    }

    /// Moves forward one version, or `None` at the end of history.
    pub fn redo(&mut self) -> Option<&Timeline> {
        if self.cursor + 1 >= self.history.len() {
            return None;
        }
        self.cursor += 1;
        Some(&self.history[self.cursor])
    }

    pub fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_redo(&self) -> bool {
        self.cursor + 1 < self.history.len()
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    fn timeline(fps: f64) -> Timeline {
        Timeline::builder()
            .canvas(1920, 1080)
            .frame_rate(fps)
            .video_track(vec![avio::Clip::new("a.mp4")])
            .build()
            .expect("timeline builds with explicit canvas + fps")
    }

    fn set_fps(fps: f64) -> Command {
        Command::SetFrameRate { fps }
    }

    fn fps(h: &EditHistory) -> f64 {
        h.current().frame_rate()
    }

    #[test]
    fn new_has_no_undo_or_redo() {
        let h = EditHistory::new(timeline(30.0));
        assert!(!h.can_undo());
        assert!(!h.can_redo());
        assert_eq!(fps(&h), 30.0);
    }

    #[test]
    fn apply_advances_and_enables_undo() {
        let mut h = EditHistory::new(timeline(30.0));
        h.apply(&set_fps(24.0)).expect("valid");
        assert_eq!(fps(&h), 24.0);
        assert!(h.can_undo());
        assert!(!h.can_redo());
    }

    #[test]
    fn undo_then_redo() {
        let mut h = EditHistory::new(timeline(30.0));
        h.apply(&set_fps(24.0)).expect("valid");
        assert_eq!(h.undo().map(Timeline::frame_rate), Some(30.0));
        assert!(h.can_redo());
        assert_eq!(h.redo().map(Timeline::frame_rate), Some(24.0));
    }

    #[test]
    fn apply_after_undo_truncates_redo() {
        let mut h = EditHistory::new(timeline(30.0));
        h.apply(&set_fps(24.0)).expect("valid");
        h.apply(&set_fps(48.0)).expect("valid");
        h.undo(); // back to 24.0; 48.0 is now the redo tail
        h.apply(&set_fps(60.0)).expect("valid"); // discards the 48.0 redo
        assert!(!h.can_redo());
        assert_eq!(fps(&h), 60.0);
    }

    #[test]
    fn apply_batch_is_one_undo_step() {
        let mut h = EditHistory::new(timeline(30.0));
        h.apply_batch(&[
            set_fps(24.0),
            Command::SetCanvas {
                width: 1280,
                height: 720,
            },
        ])
        .expect("valid");
        assert_eq!(fps(&h), 24.0);
        assert_eq!(h.current().explicit_canvas(), Some((1280, 720)));
        // Undo reverts BOTH in one step.
        h.undo();
        assert_eq!(fps(&h), 30.0);
        assert_eq!(h.current().explicit_canvas(), Some((1920, 1080)));
        assert!(!h.can_undo());
    }

    #[test]
    fn apply_batch_empty_is_noop() {
        let mut h = EditHistory::new(timeline(30.0));
        h.apply_batch(&[]).expect("valid");
        assert!(!h.can_undo());
    }

    #[test]
    fn apply_batch_aborts_atomically_on_error() {
        let mut h = EditHistory::new(timeline(30.0));
        let err = h.apply_batch(&[set_fps(24.0), set_fps(0.0)]); // 0 fps is invalid
        assert!(err.is_err());
        // Nothing recorded; still at the initial version.
        assert_eq!(fps(&h), 30.0);
        assert!(!h.can_undo());
    }

    #[test]
    fn amend_replaces_without_a_new_step() {
        let mut h = EditHistory::new(timeline(30.0));
        h.apply(&set_fps(24.0)).expect("valid"); // one step: [30, 24]
        h.amend(timeline(48.0)); // replace current 24 → 48, no new step
        assert_eq!(fps(&h), 48.0);
        assert!(h.can_undo());
        assert!(!h.can_redo());
        // Undo goes to the version BEFORE the amended one.
        assert_eq!(h.undo().map(Timeline::frame_rate), Some(30.0));
    }

    #[test]
    fn amend_truncates_redo() {
        let mut h = EditHistory::new(timeline(30.0));
        h.apply(&set_fps(24.0)).expect("valid");
        h.apply(&set_fps(48.0)).expect("valid");
        h.undo(); // current 24, redo tail = 48
        h.amend(timeline(60.0));
        assert!(!h.can_redo());
        assert_eq!(fps(&h), 60.0);
    }

    #[test]
    fn history_is_capped_and_keeps_newest() {
        let mut h = EditHistory::new(timeline(1.0));
        for i in 2..=(MAX_HISTORY + 5) {
            h.apply(&set_fps(i as f64)).expect("valid");
        }
        assert!(h.can_undo());
        assert_eq!(fps(&h), (MAX_HISTORY + 5) as f64);
    }

    #[test]
    fn gesture_coalesces_amends_into_one_step() {
        let mut h = EditHistory::new(timeline(30.0));
        h.begin_gesture();
        h.amend(timeline(24.0)); // live
        h.amend(timeline(48.0)); // live
        h.commit_gesture(); // one step: 30 -> 48
        assert_eq!(fps(&h), 48.0);
        assert_eq!(h.undo().map(Timeline::frame_rate), Some(30.0));
        assert!(!h.can_undo());
    }

    #[test]
    fn gesture_cancel_restores_pre() {
        let mut h = EditHistory::new(timeline(30.0));
        h.begin_gesture();
        h.amend(timeline(24.0));
        h.cancel_gesture();
        assert_eq!(fps(&h), 30.0);
        assert!(!h.can_undo());
    }
}
