//! Bookkeeping for read and star changes that are shown before the database confirms them.
//!
//! The window shows a change at once and writes it in the background. Writes run in order,
//! but their results arrive later, possibly after the user changed the same article again,
//! so every result is checked against what has happened since it was started.

use std::collections::HashMap;

/// A part of an article's state that can be changed on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Field {
    Read,
    Starred,
}

#[derive(Clone, Copy, Debug)]
struct Change {
    sequence: u64,
    value: bool,
    finished: bool,
}

/// The newest change of each article field, numbered in the order they were started.
#[derive(Debug, Default)]
pub(super) struct Changes {
    last: u64,
    newest: HashMap<(i64, Field), Change>,
}

impl Changes {
    /// Record a change about to be written; returns its sequence number.
    pub fn start(&mut self, id: i64, field: Field, value: bool) -> u64 {
        self.last += 1;
        self.newest.insert(
            (id, field),
            Change {
                sequence: self.last,
                value,
                finished: false,
            },
        );
        self.last
    }

    /// Record that the write of change `sequence` finished, successfully or not. Returns
    /// whether it is still the newest change of its field, so that its outcome may be shown.
    pub fn finish(&mut self, id: i64, field: Field, sequence: u64) -> bool {
        match self.newest.get_mut(&(id, field)) {
            Some(change) if change.sequence == sequence => {
                change.finished = true;
                true
            }
            _ => false,
        }
    }

    /// Number a bulk change, such as "mark all read", as it is sent, so that it is ordered
    /// with every other change.
    pub fn start_bulk(&mut self) -> u64 {
        self.last += 1;
        self.last
    }

    /// Record that bulk change `sequence` set `field` of article `id` to `value`, unless a
    /// newer change has set it since. Returns whether it did.
    pub fn finish_bulk(&mut self, id: i64, field: Field, value: bool, sequence: u64) -> bool {
        if self
            .newest
            .get(&(id, field))
            .is_some_and(|c| c.sequence > sequence)
        {
            return false;
        }
        self.newest.insert(
            (id, field),
            Change {
                sequence,
                value,
                finished: true,
            },
        );
        true
    }

    /// A point in the sequence, taken when a read or a bulk change is sent to the database.
    pub fn mark(&self) -> u64 {
        self.last
    }

    /// The value an unfinished change is writing.
    #[cfg(test)]
    pub fn unfinished(&self, id: i64, field: Field) -> Option<bool> {
        self.newest
            .get(&(id, field))
            .filter(|c| !c.finished)
            .map(|c| c.value)
    }

    /// Correct `(read, starred)` as read from the database by a read sent at `mark`: a field
    /// changed since then, or still being written, shows that change instead.
    pub fn overlay(&self, id: i64, mark: u64, read: &mut bool, starred: &mut bool) {
        for (field, value) in [(Field::Read, read), (Field::Starred, starred)] {
            if let Some(change) = self.newest.get(&(id, field))
                && (change.sequence > mark || !change.finished)
            {
                *value = change.value;
            }
        }
    }
}

/// The articles one "mark all read" changed, for undo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Batch {
    id: u64,
    pub articles: Vec<i64>,
}

/// The batch Ctrl+Z restores. Only the newest batch can be undone.
#[derive(Debug, Default)]
pub(super) struct Undo {
    last: u64,
    batch: Option<Batch>,
}

impl Undo {
    /// Make `articles` the batch to undo, replacing any older one.
    pub fn push(&mut self, articles: Vec<i64>) {
        self.last += 1;
        self.batch = Some(Batch {
            id: self.last,
            articles,
        });
    }

    /// Take the batch to undo, if there is one with any articles.
    pub fn take(&mut self) -> Option<Batch> {
        self.batch.take().filter(|b| !b.articles.is_empty())
    }

    /// Put back a batch whose undo failed, unless a newer batch has replaced it since.
    pub fn give_back(&mut self, batch: Batch) {
        if batch.id == self.last && self.batch.is_none() {
            self.batch = Some(batch);
        }
    }
}

#[cfg(test)]
mod tests {
    // tests/desktop_smoke.rs includes this file without a test harness, so nothing uses it there.
    #[allow(unused_imports)]
    use super::*;

    #[test]
    fn an_earlier_result_never_overrides_a_later_change() {
        let mut c = Changes::default();
        let star = c.start(1, Field::Starred, true);
        let unstar = c.start(1, Field::Starred, false);
        assert!(!c.finish(1, Field::Starred, star), "superseded");
        assert_eq!(c.unfinished(1, Field::Starred), Some(false));
        assert!(c.finish(1, Field::Starred, unstar));
        assert_eq!(c.unfinished(1, Field::Starred), None);
    }

    #[test]
    fn fields_and_articles_are_tracked_separately() {
        let mut c = Changes::default();
        let read = c.start(1, Field::Read, true);
        let star = c.start(1, Field::Starred, true);
        let other = c.start(2, Field::Read, false);
        assert!(c.finish(1, Field::Read, read));
        assert!(c.finish(2, Field::Read, other));
        assert_eq!(c.unfinished(1, Field::Starred), Some(true));
        assert!(c.finish(1, Field::Starred, star));
    }

    #[test]
    fn unfinished_changes_are_shown_over_any_read() {
        let mut c = Changes::default();
        c.start(1, Field::Starred, true);
        let (mut read, mut starred) = (false, false);
        c.overlay(1, c.mark(), &mut read, &mut starred);
        assert_eq!((read, starred), (false, true));
        let (mut read, mut starred) = (true, false);
        c.overlay(2, c.mark(), &mut read, &mut starred);
        assert_eq!((read, starred), (true, false));
    }

    #[test]
    fn a_read_arriving_after_a_newer_write_finished_keeps_the_write() {
        let mut c = Changes::default();
        let sent = c.mark();
        let star = c.start(1, Field::Starred, true);
        c.finish(1, Field::Starred, star);
        let (mut read, mut starred) = (false, false);
        c.overlay(1, sent, &mut read, &mut starred);
        assert!(starred, "the read was sent before the star");
        // A read sent after the write finished already has it, and is trusted.
        let (mut read, mut starred) = (false, false);
        c.overlay(1, c.mark(), &mut read, &mut starred);
        assert!(!starred);
    }

    #[test]
    fn bulk_changes_take_their_place_in_the_sequence() {
        let mut c = Changes::default();
        let sent = c.mark();
        let bulk = c.start_bulk();
        assert!(c.finish_bulk(1, Field::Read, true, bulk));
        assert_eq!(c.unfinished(1, Field::Read), None);
        let (mut read, mut starred) = (false, false);
        c.overlay(1, sent, &mut read, &mut starred);
        assert!(read, "a read sent before the bulk change must not undo it");
        // A toggle after the bulk change wins over it.
        c.start(1, Field::Read, false);
        let (mut read, mut starred) = (true, false);
        c.overlay(1, sent, &mut read, &mut starred);
        assert!(!read);
    }

    #[test]
    fn bulk_changes_are_ordered_by_when_they_were_sent() {
        let mut c = Changes::default();
        // Undo, then mark all read, both sent before either finished.
        let undo = c.start_bulk();
        let mark_all = c.start_bulk();
        assert!(c.finish_bulk(1, Field::Read, false, undo));
        assert!(c.finish_bulk(1, Field::Read, true, mark_all));
        let (mut read, mut starred) = (false, false);
        c.overlay(1, 0, &mut read, &mut starred);
        assert!(read);
        // Results in the other order leave the later one in place.
        let mut c = Changes::default();
        let first = c.start_bulk();
        let second = c.start_bulk();
        assert!(c.finish_bulk(1, Field::Read, true, second));
        assert!(!c.finish_bulk(1, Field::Read, false, first));
        // A toggle sent between a bulk change and its result wins over it.
        let mut c = Changes::default();
        let bulk = c.start_bulk();
        c.start(1, Field::Read, false);
        assert!(!c.finish_bulk(1, Field::Read, true, bulk));
        assert_eq!(c.unfinished(1, Field::Read), Some(false));
    }

    #[test]
    fn a_failed_undo_returns_its_batch_unless_a_newer_one_exists() {
        let mut u = Undo::default();
        assert_eq!(u.take(), None);
        u.push(vec![1, 2]);
        let batch = u.take().unwrap();
        assert_eq!(batch.articles, [1, 2]);
        assert_eq!(u.take(), None, "taken once");
        u.give_back(batch.clone());
        assert_eq!(u.take(), Some(batch.clone()));
        u.push(vec![3]);
        u.give_back(batch);
        assert_eq!(u.take().unwrap().articles, [3], "the newer batch stays");
        u.push(vec![]);
        assert_eq!(u.take(), None, "nothing to undo");
    }

    #[test]
    fn a_failed_undo_is_not_returned_after_a_newer_batch_was_undone() {
        let mut u = Undo::default();
        u.push(vec![1]);
        let old = u.take().unwrap();
        u.push(vec![2]);
        let new = u.take().unwrap();
        u.give_back(old);
        assert_eq!(u.take(), None);
        u.give_back(new.clone());
        assert_eq!(u.take(), Some(new));
    }
}
