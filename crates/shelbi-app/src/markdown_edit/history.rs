//! Undo and redo for the in-place editor, kept as edits rather than
//! snapshots so a change made elsewhere (merged into the draft while the
//! editor is open) can be carried through them instead of being undone with
//! the user's own typing.

use std::time::{Duration, Instant};

/// Typing within this long of the last keystroke undoes with it.
const COALESCE: Duration = Duration::from_millis(1500);
/// How many steps are kept.
const LIMIT: usize = 500;

/// One step: at `at`, `old` was replaced with `new`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub at: usize,
    pub old: String,
    pub new: String,
    /// The selection before the step (anchor, head), to put back on undo.
    pub before: (usize, usize),
}

/// What kind of edit a step is, for coalescing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Typing,
    Deleting,
    Other,
}

#[derive(Debug, Default, Clone)]
pub struct History {
    undo: Vec<Step>,
    redo: Vec<Step>,
    last: Option<(Kind, Instant)>,
}

impl History {
    /// Record an edit just made.
    pub fn record(&mut self, step: Step, kind: Kind, now: Instant) {
        self.redo.clear();
        let recent = self
            .last
            .is_some_and(|(k, t)| k == kind && now.duration_since(t) < COALESCE);
        let merged = recent
            && match (kind, self.undo.last_mut()) {
                // Typing on from where the last typing ended.
                (Kind::Typing, Some(prev))
                    if prev.at + prev.new.len() == step.at && step.old.is_empty() =>
                {
                    prev.new.push_str(&step.new);
                    true
                }
                // Backspacing on from where the last deletion was.
                (Kind::Deleting, Some(prev))
                    if step.new.is_empty()
                        && prev.new.is_empty()
                        && step.at + step.old.len() == prev.at =>
                {
                    prev.old.insert_str(0, &step.old);
                    prev.at = step.at;
                    true
                }
                // Deleting forward from the same spot.
                (Kind::Deleting, Some(prev))
                    if step.new.is_empty() && prev.new.is_empty() && step.at == prev.at =>
                {
                    prev.old.push_str(&step.old);
                    true
                }
                _ => false,
            };
        if !merged {
            self.undo.push(step);
            if self.undo.len() > LIMIT {
                self.undo.remove(0);
            }
        }
        self.last = (kind != Kind::Other).then_some((kind, now));
    }

    /// End coalescing (the caret moved, a command ran).
    pub fn seal(&mut self) {
        self.last = None;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// The step to undo, moved to the redo stack.
    pub fn undo(&mut self) -> Option<Step> {
        self.last = None;
        let step = self.undo.pop()?;
        self.redo.push(step.clone());
        Some(step)
    }

    /// The step to redo, moved back to the undo stack.
    pub fn redo(&mut self) -> Option<Step> {
        self.last = None;
        let step = self.redo.pop()?;
        self.undo.push(step.clone());
        Some(step)
    }

    /// Carry the history through a change made elsewhere: `start..end` of
    /// the current text was replaced with `len` bytes. Steps clear of it
    /// shift; the first that overlaps it, and everything older (or, for
    /// redo, newer), is dropped, since undoing it would undo the other
    /// change too.
    pub fn rebase(&mut self, start: usize, end: usize, len: usize) {
        self.last = None;
        // Undo steps, newest first: each turned the text before it into the
        // text after it, so the change is carried back through them.
        let (mut s, mut e) = (start, end);
        let mut keep_from = 0;
        for i in (0..self.undo.len()).rev() {
            let step = &mut self.undo[i];
            let step_end = step.at + step.new.len();
            if e <= step.at {
                step.at = step.at + len - (e - s);
            } else if s >= step_end {
                let shift = step.new.len() as isize - step.old.len() as isize;
                s = (s as isize - shift) as usize;
                e = (e as isize - shift) as usize;
            } else {
                keep_from = i + 1;
                break;
            }
        }
        self.undo.drain(..keep_from);
        // Redo steps, next first: each applies to the text the one before
        // it leaves, so the change is carried forward through them.
        let (mut s, mut e) = (start, end);
        let mut cut = None;
        for i in (0..self.redo.len()).rev() {
            let step = &mut self.redo[i];
            if e <= step.at {
                step.at = step.at + len - (e - s);
            } else if s >= step.at + step.old.len() {
                let shift = step.new.len() as isize - step.old.len() as isize;
                s = (s as isize + shift) as usize;
                e = (e as isize + shift) as usize;
            } else {
                cut = Some(i);
                break;
            }
        }
        if let Some(i) = cut {
            self.redo.drain(..=i);
        }
        // Selections recorded before the change may point anywhere now.
        for step in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            step.before = (step.at, step.at);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(at: usize, old: &str, new: &str) -> Step {
        Step {
            at,
            old: old.into(),
            new: new.into(),
            before: (at, at),
        }
    }

    #[test]
    fn typing_coalesces_until_a_pause_or_a_seal() {
        let mut h = History::default();
        let t = Instant::now();
        h.record(step(0, "", "a"), Kind::Typing, t);
        h.record(
            step(1, "", "b"),
            Kind::Typing,
            t + Duration::from_millis(100),
        );
        assert_eq!(h.undo.len(), 1);
        assert_eq!(h.undo[0].new, "ab");
        h.record(step(2, "", "c"), Kind::Typing, t + Duration::from_secs(5));
        assert_eq!(h.undo.len(), 2);
        h.seal();
        h.record(step(3, "", "d"), Kind::Typing, t + Duration::from_secs(5));
        assert_eq!(h.undo.len(), 3);
    }

    #[test]
    fn backspacing_coalesces_backwards() {
        let mut h = History::default();
        let t = Instant::now();
        h.record(step(4, "d", ""), Kind::Deleting, t);
        h.record(step(3, "c", ""), Kind::Deleting, t);
        assert_eq!(
            (h.undo.len(), h.undo[0].at, h.undo[0].old.as_str()),
            (1, 3, "cd")
        );
    }

    #[test]
    fn a_change_elsewhere_shifts_steps_after_it_and_drops_ones_it_touches() {
        let mut h = History::default();
        let t = Instant::now();
        // "hello" -> "hello world" (typed at 5).
        h.record(step(5, "", " world"), Kind::Typing, t);
        // Someone inserts "> " at the start: the step shifts by 2.
        h.rebase(0, 0, 2);
        assert_eq!(h.undo[0].at, 7);
        // A change after it leaves it be.
        h.rebase(13, 13, 1);
        assert_eq!(h.undo[0].at, 7);
        // One inside the typed text drops it.
        h.rebase(9, 10, 0);
        assert!(h.undo.is_empty());
    }

    #[test]
    fn older_steps_see_the_change_in_their_own_coordinates() {
        let mut h = History::default();
        let t = Instant::now();
        // "ab" -> "aXb" (at 1), then "aXb" -> "aXbY" (at 3).
        h.record(step(1, "", "X"), Kind::Other, t);
        h.record(step(3, "", "Y"), Kind::Other, t);
        // Someone replaces "b" (2..3 now) with "BB".
        h.rebase(2, 3, 2);
        // The newest step (at 3, after the change) moves by +1.
        assert_eq!(h.undo[1].at, 4);
        // The older one, before it in its own text, stays.
        assert_eq!(h.undo[0].at, 1);
    }
}
