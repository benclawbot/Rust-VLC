use crate::model::Track;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlaybackQueue {
    entries: Vec<Track>,
    current_index: Option<usize>,
}

impl PlaybackQueue {
    pub fn replace(&mut self, entries: Vec<Track>, start_index: usize) -> bool {
        if entries.is_empty() || start_index >= entries.len() {
            self.entries.clear();
            self.current_index = None;
            return false;
        }

        self.entries = entries;
        self.current_index = Some(start_index);
        true
    }

    pub fn current(&self) -> Option<&Track> {
        self.current_index.and_then(|index| self.entries.get(index))
    }

    pub fn current_index(&self) -> Option<usize> {
        self.current_index
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn advance_next(&mut self) -> Option<&Track> {
        let next_index = self.current_index?.checked_add(1)?;
        if next_index >= self.entries.len() {
            return None;
        }
        self.current_index = Some(next_index);
        self.current()
    }

    pub fn previous(&mut self) -> Option<&Track> {
        let current = self.current_index?;
        self.current_index = Some(current.saturating_sub(1));
        self.current()
    }
}

#[cfg(test)]
mod tests {
    use super::PlaybackQueue;
    use crate::model::Track;
    use std::path::PathBuf;

    fn track(id: i32) -> Track {
        Track {
            id,
            path: PathBuf::from(format!("track-{id}.mp3")),
            title: format!("Track {id}"),
            format: "MP3".to_owned(),
        }
    }

    #[test]
    fn queue_steps_without_reordering_and_stops_at_end() {
        let mut queue = PlaybackQueue::default();
        assert!(queue.replace(vec![track(7), track(2), track(9)], 1));
        assert_eq!(queue.current().unwrap().id, 2);
        assert_eq!(queue.advance_next().unwrap().id, 9);
        assert!(queue.advance_next().is_none());
        assert_eq!(queue.previous().unwrap().id, 2);
        assert_eq!(queue.previous().unwrap().id, 7);
        assert_eq!(queue.previous().unwrap().id, 7);
    }

    #[test]
    fn invalid_queue_selection_clears_the_previous_queue() {
        let mut queue = PlaybackQueue::default();
        assert!(queue.replace(vec![track(1)], 0));
        assert!(!queue.replace(vec![track(2)], 1));
        assert!(queue.is_empty());
        assert!(queue.current_index().is_none());
    }
}
