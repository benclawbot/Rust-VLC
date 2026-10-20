use crate::model::{Track, TrackId};
use std::collections::HashSet;

#[derive(Default)]
pub struct TrackSelection {
    selected: HashSet<TrackId>,
}

impl TrackSelection {
    pub fn contains(&self, id: TrackId) -> bool {
        self.selected.contains(&id)
    }

    pub fn set(&mut self, id: TrackId, selected: bool) {
        if selected {
            self.selected.insert(id);
        } else {
            self.selected.remove(&id);
        }
    }

    pub fn set_visible(&mut self, tracks: &[Track], query: &str, selected: bool) {
        for track in visible_tracks(tracks, query) {
            self.set(track.id, selected);
        }
    }

    pub fn selected_visible_in_order(&self, tracks: &[Track], query: &str) -> Vec<Track> {
        visible_tracks(tracks, query)
            .into_iter()
            .filter(|track| self.contains(track.id))
            .cloned()
            .collect()
    }

    pub fn selected_in_order(&self, tracks: &[Track]) -> Vec<Track> {
        tracks
            .iter()
            .filter(|track| self.contains(track.id))
            .cloned()
            .collect()
    }

    pub fn all_visible_selected(&self, tracks: &[Track], query: &str) -> bool {
        let visible = visible_tracks(tracks, query);
        !visible.is_empty() && visible.iter().all(|track| self.contains(track.id))
    }

    pub fn selected_count(&self) -> usize {
        self.selected.len()
    }
}

pub fn visible_tracks<'a>(tracks: &'a [Track], query: &str) -> Vec<&'a Track> {
    let query = query.trim().to_lowercase();
    tracks
        .iter()
        .filter(|track| {
            query.is_empty()
                || track.title.to_lowercase().contains(&query)
                || track
                    .path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().to_lowercase().contains(&query))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{visible_tracks, TrackSelection};
    use crate::model::Track;
    use std::path::PathBuf;

    fn track(id: i32, title: &str) -> Track {
        Track {
            id,
            path: PathBuf::from(format!("{title}.mp3")),
            title: title.to_owned(),
            format: "MP3".to_owned(),
        }
    }

    #[test]
    fn select_all_affects_only_visible_tracks() {
        let tracks = vec![track(1, "Amber"), track(2, "Blue"), track(3, "Green")];
        let mut selection = TrackSelection::default();

        selection.set(1, true);
        selection.set_visible(&tracks, "bl", true);
        assert!(selection.contains(1));
        assert!(selection.contains(2));
        assert!(!selection.contains(3));
        assert!(selection.all_visible_selected(&tracks, "bl"));

        selection.set_visible(&tracks, "bl", false);
        assert!(selection.contains(1));
        assert!(!selection.contains(2));
    }

    #[test]
    fn selected_queue_uses_library_order_not_selection_order() {
        let tracks = vec![track(1, "Amber"), track(2, "Blue"), track(3, "Green")];
        let mut selection = TrackSelection::default();
        selection.set(3, true);
        selection.set(1, true);

        let queued: Vec<_> = selection
            .selected_visible_in_order(&tracks, "")
            .into_iter()
            .map(|track| track.id)
            .collect();
        assert_eq!(queued, [1, 3]);
        assert_eq!(visible_tracks(&tracks, "bl").len(), 1);
    }

    #[test]
    fn filtered_bulk_actions_keep_visible_library_order_and_hidden_checks() {
        let tracks = vec![track(1, "Amber"), track(2, "Blue"), track(3, "Aqua")];
        let mut selection = TrackSelection::default();
        selection.set(3, true);
        selection.set(2, true);
        selection.set(1, true);

        let all_selected: Vec<_> = selection
            .selected_in_order(&tracks)
            .into_iter()
            .map(|track| track.id)
            .collect();
        assert_eq!(all_selected, [1, 2, 3]);

        let filtered: Vec<_> = selection
            .selected_visible_in_order(&tracks, "a")
            .into_iter()
            .map(|track| track.id)
            .collect();
        assert_eq!(filtered, [1, 3]);
        assert!(selection.contains(2), "filtering retains the hidden check");
        assert_eq!(selection.selected_visible_in_order(&tracks, "bl")[0].id, 2);
    }
}
