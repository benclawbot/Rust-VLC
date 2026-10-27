use crate::model::Track;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RepeatMode {
    #[default]
    Off,
    One,
    All,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueCheckpoint {
    pub entry_ids: Vec<i32>,
    pub order: Vec<usize>,
    pub position: Option<usize>,
    pub wrapped_from_end: bool,
    pub repeat_mode: RepeatMode,
    pub shuffle: bool,
    pub position_ms: i64,
}

impl Default for QueueCheckpoint {
    fn default() -> Self {
        Self {
            entry_ids: Vec::new(),
            order: Vec::new(),
            position: None,
            wrapped_from_end: false,
            repeat_mode: RepeatMode::Off,
            shuffle: false,
            position_ms: 0,
        }
    }
}

impl QueueCheckpoint {
    pub fn is_valid(&self) -> bool {
        if self.position_ms < 0 || self.entry_ids.len() != self.order.len() {
            return false;
        }
        if self.entry_ids.is_empty() {
            return self.position.is_none() && !self.wrapped_from_end && self.position_ms == 0;
        }
        if self
            .position
            .is_none_or(|position| position >= self.order.len())
            || (self.wrapped_from_end
                && (self.position != Some(0) || self.repeat_mode != RepeatMode::All))
        {
            return false;
        }
        let mut seen = vec![false; self.entry_ids.len()];
        self.order.iter().all(|&index| {
            if index >= seen.len() || seen[index] {
                false
            } else {
                seen[index] = true;
                true
            }
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlaybackQueue {
    entries: Vec<Track>,
    order: Vec<usize>,
    position: Option<usize>,
    repeat_mode: RepeatMode,
    shuffle: bool,
    wrapped_from_end: bool,
}

impl PlaybackQueue {
    pub fn checkpoint(&self, position_ms: i64) -> QueueCheckpoint {
        QueueCheckpoint {
            entry_ids: self.entries.iter().map(|track| track.id).collect(),
            order: self.order.clone(),
            position: self.position,
            wrapped_from_end: self.wrapped_from_end,
            repeat_mode: self.repeat_mode,
            shuffle: self.shuffle,
            position_ms: position_ms.max(0),
        }
    }

    pub fn restore(&mut self, entries: Vec<Track>, checkpoint: &QueueCheckpoint) -> bool {
        if !checkpoint.is_valid()
            || entries.iter().map(|track| track.id).collect::<Vec<_>>() != checkpoint.entry_ids
        {
            return false;
        }
        self.entries = entries;
        self.order.clone_from(&checkpoint.order);
        self.position = checkpoint.position;
        self.wrapped_from_end = checkpoint.wrapped_from_end;
        self.repeat_mode = checkpoint.repeat_mode;
        self.shuffle = checkpoint.shuffle;
        true
    }

    pub fn replace(&mut self, entries: Vec<Track>, start_index: usize) -> bool {
        if entries.is_empty() || start_index >= entries.len() {
            self.entries.clear();
            self.order.clear();
            self.position = None;
            self.wrapped_from_end = false;
            return false;
        }

        self.entries = entries;
        self.order = (0..self.entries.len()).collect();
        self.wrapped_from_end = false;
        if self.shuffle {
            fastrand::shuffle(&mut self.order);
            let start_position = self
                .order
                .iter()
                .position(|&entry_index| entry_index == start_index)
                .expect("queue order contains each entry");
            self.order.swap(0, start_position);
            self.position = Some(0);
        } else {
            self.position = Some(start_index);
        }
        true
    }

    pub fn current(&self) -> Option<&Track> {
        self.position
            .and_then(|position| self.order.get(position))
            .and_then(|&entry_index| self.entries.get(entry_index))
    }

    /// The index of the current entry in the original, unshuffled entries.
    pub fn current_index(&self) -> Option<usize> {
        self.position
            .and_then(|position| self.order.get(position).copied())
    }

    pub fn ordered_entries(&self) -> Vec<&Track> {
        self.order
            .iter()
            .filter_map(|&entry_index| self.entries.get(entry_index))
            .collect()
    }

    /// Replace every queued occurrence for a media ID without changing traversal state.
    pub fn replace_track_data(&mut self, updated: &Track) -> usize {
        let mut replaced = 0;
        for entry in &mut self.entries {
            if entry.id == updated.id {
                *entry = updated.clone();
                replaced += 1;
            }
        }
        replaced
    }

    pub fn traversal_position(&self) -> Option<usize> {
        self.position
    }

    pub fn select_at(&mut self, position: usize) -> bool {
        if position >= self.order.len() {
            return false;
        }
        self.position = Some(position);
        self.wrapped_from_end = false;
        true
    }

    /// Move an occurrence by its position in traversal order, preserving the current entry.
    pub fn move_entry(&mut self, from: usize, to: usize) -> bool {
        if from >= self.order.len() || to >= self.order.len() {
            return false;
        }
        if from == to {
            return true;
        }

        let current_entry = self.current_index();
        let entry = self.order.remove(from);
        self.order.insert(to, entry);
        if let Some(current_entry) = current_entry {
            self.position = self
                .order
                .iter()
                .position(|&entry_index| entry_index == current_entry);
        }
        self.wrapped_from_end = false;
        true
    }

    /// Reorder an owning playlist while keeping the current occurrence selected.
    pub(super) fn reorder_occurrences(&mut self, entries: Vec<Track>) -> Result<bool, ()> {
        if entries.len() != self.entries.len() {
            return Err(());
        }
        if entries == self.entries {
            return Ok(false);
        }

        let mut old_indices = HashMap::<i32, Vec<usize>>::new();
        for (index, track) in self.entries.iter().enumerate() {
            old_indices.entry(track.id).or_default().push(index);
        }

        let mut seen = HashMap::<i32, usize>::new();
        let mut old_to_new = vec![usize::MAX; entries.len()];
        for (new_index, track) in entries.iter().enumerate() {
            let rank = seen.entry(track.id).or_default();
            let Some(old_index) = old_indices
                .get(&track.id)
                .and_then(|indices| indices.get(*rank))
                .copied()
            else {
                return Err(());
            };
            old_to_new[old_index] = new_index;
            *rank += 1;
        }

        if old_indices.iter().any(|(track_id, indices)| {
            seen.get(track_id).copied().unwrap_or_default() != indices.len()
        }) {
            return Err(());
        }

        let current = self.current_index().map(|index| old_to_new[index]);
        self.entries = entries;
        self.order = (0..self.entries.len()).collect();
        if self.shuffle {
            if let Some(current) = current {
                self.order.swap(0, current);
                fastrand::shuffle(&mut self.order[1..]);
                self.position = Some(0);
            } else {
                fastrand::shuffle(&mut self.order);
                self.position = None;
            }
        } else {
            self.position = current;
        }
        self.wrapped_from_end = false;
        Ok(true)
    }

    /// Remove an occurrence; the current entry is replaced by its next, then previous neighbor.
    pub fn remove_at(&mut self, position: usize) -> Option<bool> {
        if position >= self.order.len() {
            return None;
        }
        let current_position = self.position;
        let removed_entry = self.order.remove(position);
        self.entries.remove(removed_entry);
        for entry_index in &mut self.order {
            if *entry_index > removed_entry {
                *entry_index -= 1;
            }
        }

        let removed_current = current_position == Some(position);
        self.position = match current_position {
            Some(current) if current == position => {
                (!self.order.is_empty()).then(|| position.min(self.order.len() - 1))
            }
            Some(current) if current > position => Some(current - 1),
            current => current,
        };
        self.wrapped_from_end = false;
        Some(removed_current)
    }

    /// Insert directly after the current occurrence; an empty queue starts at the new entry.
    pub fn insert_next(&mut self, track: Track) -> bool {
        let was_empty = self.entries.is_empty();
        self.entries.push(track);
        let entry_index = self.entries.len() - 1;
        if was_empty {
            self.order.push(entry_index);
            self.position = Some(0);
            self.wrapped_from_end = false;
            return true;
        }

        let insertion_position = self
            .position
            .map_or(self.order.len(), |position| position + 1);
        self.order.insert(insertion_position, entry_index);
        self.wrapped_from_end = false;
        false
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn set_repeat(&mut self, repeat_mode: RepeatMode) {
        if self.repeat_mode != repeat_mode {
            self.wrapped_from_end = false;
        }
        self.repeat_mode = repeat_mode;
    }

    pub fn repeat_mode(&self) -> RepeatMode {
        self.repeat_mode
    }

    pub fn set_shuffle(&mut self, shuffle: bool) {
        if self.shuffle == shuffle {
            return;
        }
        self.shuffle = shuffle;
        self.wrapped_from_end = false;

        let Some(position) = self.position else {
            self.order = (0..self.entries.len()).collect();
            if self.shuffle {
                fastrand::shuffle(&mut self.order);
            }
            return;
        };

        if shuffle {
            fastrand::shuffle(&mut self.order[position + 1..]);
        } else {
            let prefix_len = position + 1;
            let mut in_history = vec![false; self.entries.len()];
            for &entry_index in &self.order[..prefix_len] {
                in_history[entry_index] = true;
            }
            self.order.truncate(prefix_len);
            self.order
                .extend((0..self.entries.len()).filter(|&index| !in_history[index]));
        }
    }

    pub fn is_shuffled(&self) -> bool {
        self.shuffle
    }

    /// Manually advances; Repeat One does not affect a user-requested Next.
    pub fn advance_next(&mut self) -> Option<&Track> {
        let current_position = self.position?;
        let next_position = current_position + 1;
        if next_position < self.order.len() {
            self.position = Some(next_position);
            self.wrapped_from_end = false;
        } else if self.repeat_mode == RepeatMode::All {
            self.position = Some(0);
            self.wrapped_from_end = true;
        } else {
            return None;
        }
        self.current()
    }

    /// Advances after a track ends, repeating the current entry in Repeat One.
    pub fn advance_on_end(&mut self) -> Option<&Track> {
        if self.repeat_mode == RepeatMode::One {
            return self.current();
        }
        self.advance_next()
    }

    /// Moves back along the active traversal order. At its start, keeps the current entry.
    pub fn previous(&mut self) -> Option<&Track> {
        let current_position = self.position?;
        if current_position == 0 && self.wrapped_from_end && self.repeat_mode == RepeatMode::All {
            self.position = self.order.len().checked_sub(1);
            self.wrapped_from_end = false;
        } else {
            self.position = Some(current_position.saturating_sub(1));
        }
        self.current()
    }
}

#[cfg(test)]
mod tests {
    use super::{PlaybackQueue, RepeatMode};
    use crate::model::Track;
    use std::path::PathBuf;

    fn track(id: i32) -> Track {
        Track {
            source: crate::model::TrackSource::Local,
            id,
            path: PathBuf::from(format!("track-{id}.mp3")),
            title: format!("Track {id}"),
            format: "MP3".to_owned(),
        }
    }

    fn duplicate_track(title: &str) -> Track {
        Track {
            source: crate::model::TrackSource::Local,
            id: 7,
            path: PathBuf::from(format!("{title}.mp3")),
            title: title.to_owned(),
            format: "MP3".to_owned(),
        }
    }

    #[test]
    fn queue_steps_without_reordering_and_stops_at_end() {
        let mut queue = PlaybackQueue::default();
        assert!(queue.replace(vec![track(7), track(2), track(9)], 1));
        assert_eq!(queue.current().unwrap().id, 2);
        assert_eq!(queue.advance_next().unwrap().id, 9);
        assert!(queue.advance_on_end().is_none());
        assert_eq!(queue.previous().unwrap().id, 2);
        assert_eq!(queue.previous().unwrap().id, 7);
        assert_eq!(queue.previous().unwrap().id, 7);
    }

    #[test]
    fn replacing_track_data_updates_every_occurrence_without_changing_traversal() {
        let mut queue = PlaybackQueue::default();
        assert!(queue.replace(vec![track(1), track(2), track(1)], 2));
        let current_index = queue.current_index();
        let updated = Track {
            path: PathBuf::from("moved/track-1.mp3"),
            title: "Track one".into(),
            ..track(1)
        };

        assert_eq!(queue.replace_track_data(&updated), 2);
        assert_eq!(queue.current_index(), current_index);
        assert_eq!(queue.current().unwrap(), &updated);
        assert_eq!(
            queue
                .ordered_entries()
                .into_iter()
                .filter(|entry| entry.id == 1)
                .map(|entry| entry.path.clone())
                .collect::<Vec<_>>(),
            vec![
                PathBuf::from("moved/track-1.mp3"),
                PathBuf::from("moved/track-1.mp3")
            ]
        );
    }

    #[test]
    fn invalid_queue_selection_clears_the_previous_queue() {
        let mut queue = PlaybackQueue::default();
        assert!(queue.replace(vec![track(1)], 0));
        assert!(!queue.replace(vec![track(2)], 1));
        assert!(queue.is_empty());
        assert!(queue.current_index().is_none());
    }

    #[test]
    fn repeat_off_stops_at_the_end() {
        let mut queue = PlaybackQueue::default();
        assert_eq!(queue.repeat_mode(), RepeatMode::Off);
        assert!(queue.replace(vec![track(1), track(2)], 0));
        assert_eq!(queue.advance_on_end().unwrap().id, 2);
        assert!(queue.advance_on_end().is_none());
        assert_eq!(queue.current().unwrap().id, 2);
    }

    #[test]
    fn repeat_one_replays_only_on_end_and_manual_next_ignores_it() {
        let mut queue = PlaybackQueue::default();
        assert!(queue.replace(vec![track(1), track(2)], 0));
        queue.set_repeat(RepeatMode::One);
        assert_eq!(queue.repeat_mode(), RepeatMode::One);
        assert_eq!(queue.advance_on_end().unwrap().id, 1);
        assert_eq!(queue.current_index(), Some(0));
        assert_eq!(queue.advance_next().unwrap().id, 2);
        assert!(queue.advance_next().is_none());
    }

    #[test]
    fn repeat_all_wraps_after_end_and_manual_next() {
        let mut queue = PlaybackQueue::default();
        assert!(queue.replace(vec![track(1), track(2)], 1));
        queue.set_repeat(RepeatMode::All);
        assert_eq!(queue.advance_on_end().unwrap().id, 1);
        assert_eq!(queue.advance_next().unwrap().id, 2);
    }

    #[test]
    fn previous_after_repeat_all_wrap_returns_last_shuffled_entry() {
        let mut queue = PlaybackQueue::default();
        assert!(queue.replace(vec![track(1), track(2), track(3)], 0));
        queue.order = vec![1, 2, 0];
        queue.position = Some(2);
        queue.shuffle = true;
        queue.set_repeat(RepeatMode::All);

        assert_eq!(queue.advance_next().unwrap().id, 2);
        assert_eq!(queue.current_index(), Some(1));
        assert_eq!(queue.previous().unwrap().id, 1);
        assert_eq!(queue.current_index(), Some(0));
        assert_eq!(queue.previous().unwrap().id, 3);
        assert_eq!(queue.current_index(), Some(2));
    }

    #[test]
    fn queue_occurrence_edits_keep_current_entry_and_duplicate_identity() {
        let mut queue = PlaybackQueue::default();
        assert!(queue.replace(
            vec![
                duplicate_track("first-copy"),
                duplicate_track("second-copy"),
                track(8)
            ],
            1,
        ));
        assert_eq!(queue.current().unwrap().title, "second-copy");
        assert!(queue.move_entry(0, 2));
        assert_eq!(queue.ordered_entries()[0].title, "second-copy");
        assert_eq!(queue.current().unwrap().title, "second-copy");
        assert_eq!(queue.traversal_position(), Some(0));

        assert_eq!(queue.remove_at(2), Some(false));
        assert_eq!(queue.ordered_entries()[0].title, "second-copy");
        assert_eq!(queue.current().unwrap().title, "second-copy");
        assert_eq!(queue.remove_at(0), Some(true));
        assert_eq!(queue.current().unwrap().id, 8);
        assert_eq!(queue.remove_at(0), Some(true));
        assert!(queue.current().is_none());
        assert_eq!(queue.remove_at(0), None);
    }

    #[test]
    fn owning_playlist_reorder_keeps_the_current_duplicate_and_rejects_other_contents() {
        let mut queue = PlaybackQueue::default();
        let duplicate = track(7);
        assert!(queue.replace(vec![duplicate.clone(), track(8), duplicate.clone()], 2));

        assert_eq!(
            queue.reorder_occurrences(vec![duplicate.clone(), duplicate.clone(), track(8)]),
            Ok(true)
        );
        assert_eq!(queue.current().unwrap().id, 7);
        assert_eq!(queue.current_index(), Some(1));
        assert_eq!(
            queue
                .ordered_entries()
                .iter()
                .map(|track| track.id)
                .collect::<Vec<_>>(),
            [7, 7, 8]
        );

        let before = queue
            .ordered_entries()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            queue.reorder_occurrences(vec![track(8), track(9), duplicate]),
            Err(())
        );
        assert_eq!(
            queue
                .ordered_entries()
                .into_iter()
                .cloned()
                .collect::<Vec<_>>(),
            before
        );
    }

    #[test]
    fn queue_insert_next_preserves_current_and_starts_an_empty_queue() {
        let mut queue = PlaybackQueue::default();
        assert!(queue.insert_next(track(1)));
        assert_eq!(queue.current().unwrap().id, 1);
        assert!(!queue.insert_next(duplicate_track("same-id-next")));
        assert_eq!(queue.current().unwrap().id, 1);
        assert_eq!(queue.traversal_position(), Some(0));
        assert_eq!(
            queue
                .ordered_entries()
                .iter()
                .map(|track| track.title.as_str())
                .collect::<Vec<_>>(),
            ["Track 1", "same-id-next"]
        );
    }

    #[test]
    fn shuffle_keeps_entry_identity_and_previous_follows_the_traversal() {
        let entries = vec![
            duplicate_track("first-copy"),
            duplicate_track("second-copy"),
            track(8),
            track(9),
        ];
        let original_entries = entries.clone();
        let mut queue = PlaybackQueue::default();
        assert!(queue.replace(entries, 0));
        queue.set_shuffle(true);
        assert!(queue.is_shuffled());
        assert_eq!(queue.current_index(), Some(0));
        assert_eq!(queue.current().unwrap().title, "first-copy");

        let traversal = queue.order.clone();
        let mut sorted_entries = traversal.clone();
        sorted_entries.sort_unstable();
        assert_eq!(sorted_entries, vec![0, 1, 2, 3]);
        assert_eq!(queue.entries, original_entries);

        let first_next_index = traversal[1];
        let second_next_index = traversal[2];
        assert_eq!(
            queue.advance_next().unwrap().id,
            original_entries[first_next_index].id
        );
        assert_eq!(queue.current_index(), Some(first_next_index));
        assert_eq!(
            queue.advance_next().unwrap().id,
            original_entries[second_next_index].id
        );
        assert_eq!(queue.current_index(), Some(second_next_index));
        assert_eq!(
            queue.previous().unwrap().id,
            original_entries[first_next_index].id
        );
        assert_eq!(queue.current_index(), Some(first_next_index));
        assert_eq!(queue.previous().unwrap().title, "first-copy");
        assert_eq!(queue.current_index(), Some(0));
        assert_eq!(
            queue.advance_next().unwrap().id,
            original_entries[first_next_index].id
        );
        assert_eq!(queue.current_index(), Some(first_next_index));
    }

    #[test]
    fn checkpoint_restores_duplicate_occurrences_shuffle_order_current_and_wrap_state() {
        let entries = vec![track(1), track(2), track(1)];
        let mut queue = PlaybackQueue::default();
        assert!(queue.replace(entries.clone(), 2));
        queue.order = vec![2, 0, 1];
        queue.position = Some(0);
        queue.shuffle = true;
        queue.set_repeat(RepeatMode::All);
        queue.wrapped_from_end = true;

        let checkpoint = queue.checkpoint(12_345);
        let mut restored = PlaybackQueue::default();

        assert!(restored.restore(entries, &checkpoint));
        assert_eq!(restored.checkpoint(12_345), checkpoint);
        assert_eq!(restored.current_index(), Some(2));
        assert_eq!(restored.current().unwrap().id, 1);
        assert_eq!(restored.previous().unwrap().id, 2);
        assert_eq!(checkpoint.position_ms, 12_345);
    }

    #[test]
    fn restore_rejects_invalid_order_position_and_mismatched_entries() {
        let entries = vec![track(1), track(2)];
        let mut queue = PlaybackQueue::default();
        assert!(queue.replace(vec![track(9)], 0));
        let mut checkpoint = queue.checkpoint(0);
        checkpoint.entry_ids = vec![1, 2];
        checkpoint.order = vec![0, 0];
        checkpoint.position = Some(0);
        assert!(!queue.restore(entries.clone(), &checkpoint));
        assert_eq!(queue.current().unwrap().id, 9);

        checkpoint.order = vec![0, 1];
        checkpoint.position = Some(2);
        assert!(!queue.restore(entries.clone(), &checkpoint));
        checkpoint.position = Some(1);
        assert!(!queue.restore(vec![track(2), track(1)], &checkpoint));
        assert_eq!(queue.current().unwrap().id, 9);
    }
}
