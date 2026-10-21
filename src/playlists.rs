use crate::model::{Track, TrackId};
use std::fmt;

pub type PlaylistId = i32;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Playlist {
    pub id: PlaylistId,
    pub name: String,
    pub track_ids: Vec<TrackId>,
}

#[derive(Debug, Default)]
pub struct PlaylistCollection {
    playlists: Vec<Playlist>,
    selected_id: Option<PlaylistId>,
    next_id: PlaylistId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaylistError {
    EmptyName,
    DuplicateName,
    NoPlaylistSelected,
}

impl fmt::Display for PlaylistError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyName => write!(formatter, "Enter a name for the new playlist."),
            Self::DuplicateName => write!(formatter, "A playlist with that name already exists."),
            Self::NoPlaylistSelected => write!(formatter, "Choose a playlist first."),
        }
    }
}

impl std::error::Error for PlaylistError {}

impl PlaylistCollection {
    pub fn restore(playlists: Vec<Playlist>, selected_id: Option<PlaylistId>) -> Self {
        let next_id = playlists
            .iter()
            .map(|playlist| playlist.id)
            .max()
            .unwrap_or(0);
        let selected_id =
            selected_id.filter(|id| playlists.iter().any(|playlist| playlist.id == *id));
        Self {
            playlists,
            selected_id,
            next_id,
        }
    }

    pub fn playlists(&self) -> &[Playlist] {
        &self.playlists
    }

    pub fn selected_id(&self) -> Option<PlaylistId> {
        self.selected_id
    }

    pub fn has_selected_playlist(&self) -> bool {
        self.selected_id.is_some()
    }

    pub fn can_shuffle_selected(&self) -> bool {
        self.selected_playlist()
            .is_some_and(|playlist| playlist.track_ids.len() > 1)
    }

    pub fn selected_playlist(&self) -> Option<&Playlist> {
        let id = self.selected_id?;
        self.playlists.iter().find(|playlist| playlist.id == id)
    }

    pub fn create(&mut self, name: &str) -> Result<PlaylistId, PlaylistError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(PlaylistError::EmptyName);
        }
        if self
            .playlists
            .iter()
            .any(|playlist| playlist.name.eq_ignore_ascii_case(name))
        {
            return Err(PlaylistError::DuplicateName);
        }

        self.next_id = self.next_id.saturating_add(1);
        let id = self.next_id;
        self.playlists.push(Playlist {
            id,
            name: name.to_owned(),
            track_ids: Vec::new(),
        });
        self.selected_id = Some(id);
        Ok(id)
    }

    pub fn select(&mut self, id: PlaylistId) -> bool {
        if self.playlists.iter().any(|playlist| playlist.id == id) {
            self.selected_id = Some(id);
            true
        } else {
            false
        }
    }

    pub fn add_tracks(&mut self, tracks: &[Track]) -> Result<usize, PlaylistError> {
        self.add_tracks_with_duplicates(tracks, false)
    }

    pub fn add_tracks_with_duplicates(
        &mut self,
        tracks: &[Track],
        allow_duplicates: bool,
    ) -> Result<usize, PlaylistError> {
        let Some(id) = self.selected_id else {
            return Err(PlaylistError::NoPlaylistSelected);
        };
        let playlist = self
            .playlists
            .iter_mut()
            .find(|playlist| playlist.id == id)
            .ok_or(PlaylistError::NoPlaylistSelected)?;

        let mut added = 0;
        for track in tracks {
            if allow_duplicates || !playlist.track_ids.contains(&track.id) {
                playlist.track_ids.push(track.id);
                added += 1;
            }
        }
        Ok(added)
    }

    pub fn shuffle_selected(&mut self) -> Result<(), PlaylistError> {
        let id = self.selected_id.ok_or(PlaylistError::NoPlaylistSelected)?;
        self.shuffle_playlist(id)
    }

    pub fn shuffle_playlist(&mut self, id: PlaylistId) -> Result<(), PlaylistError> {
        self.shuffle_playlist_with_rng(id, &mut fastrand::Rng::new())
    }

    #[cfg(test)]
    fn shuffle_selected_with_rng(&mut self, rng: &mut fastrand::Rng) -> Result<(), PlaylistError> {
        let id = self.selected_id.ok_or(PlaylistError::NoPlaylistSelected)?;
        self.shuffle_playlist_with_rng(id, rng)
    }

    fn shuffle_playlist_with_rng(
        &mut self,
        id: PlaylistId,
        rng: &mut fastrand::Rng,
    ) -> Result<(), PlaylistError> {
        let playlist = self
            .playlists
            .iter_mut()
            .find(|playlist| playlist.id == id)
            .ok_or(PlaylistError::NoPlaylistSelected)?;
        rng.shuffle(&mut playlist.track_ids);
        Ok(())
    }

    pub fn rename(&mut self, id: PlaylistId, name: &str) -> Result<(), PlaylistError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(PlaylistError::EmptyName);
        }
        if self
            .playlists
            .iter()
            .any(|playlist| playlist.id != id && playlist.name.eq_ignore_ascii_case(name))
        {
            return Err(PlaylistError::DuplicateName);
        }
        let playlist = self
            .playlists
            .iter_mut()
            .find(|playlist| playlist.id == id)
            .ok_or(PlaylistError::NoPlaylistSelected)?;
        playlist.name = name.to_owned();
        Ok(())
    }

    pub fn remove(&mut self, id: PlaylistId) -> bool {
        let Some(position) = self.playlists.iter().position(|playlist| playlist.id == id) else {
            return false;
        };
        self.playlists.remove(position);
        if self.selected_id == Some(id) {
            self.selected_id = None;
        }
        true
    }

    pub fn remove_selected_entry(&mut self, position: usize) -> bool {
        let Some(id) = self.selected_id else {
            return false;
        };
        self.remove_entry(id, position)
    }

    pub fn remove_entry(&mut self, id: PlaylistId, position: usize) -> bool {
        let Some(playlist) = self.playlists.iter_mut().find(|playlist| playlist.id == id) else {
            return false;
        };
        if position >= playlist.track_ids.len() {
            return false;
        }
        playlist.track_ids.remove(position);
        true
    }

    pub fn move_selected_entry(&mut self, from: usize, to: usize) -> bool {
        let Some(id) = self.selected_id else {
            return false;
        };
        self.move_entry(id, from, to)
    }

    pub fn move_entry(&mut self, id: PlaylistId, from: usize, to: usize) -> bool {
        let Some(playlist) = self.playlists.iter_mut().find(|playlist| playlist.id == id) else {
            return false;
        };
        if from >= playlist.track_ids.len() || to >= playlist.track_ids.len() {
            return false;
        }
        let entry = playlist.track_ids.remove(from);
        playlist.track_ids.insert(to, entry);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{PlaylistCollection, PlaylistError};
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
    fn playlists_require_a_unique_nonempty_name() {
        let mut playlists = PlaylistCollection::default();
        assert_eq!(playlists.create("  "), Err(PlaylistError::EmptyName));
        assert_eq!(playlists.create("  Road Mix  "), Ok(1));
        assert_eq!(playlists.playlists()[0].name, "Road Mix");
        assert_eq!(
            playlists.create("road mix"),
            Err(PlaylistError::DuplicateName)
        );
    }

    #[test]
    fn adding_selected_tracks_keeps_order_and_does_not_duplicate() {
        let mut playlists = PlaylistCollection::default();
        playlists.create("Road Mix").unwrap();

        assert_eq!(playlists.add_tracks(&[track(3), track(1)]), Ok(2));
        assert_eq!(playlists.add_tracks(&[track(1), track(2)]), Ok(1));
        assert_eq!(playlists.playlists()[0].track_ids, [3, 1, 2]);
    }

    #[test]
    fn adding_duplicate_occurrences_is_an_explicit_choice() {
        let mut skipped = PlaylistCollection::default();
        skipped.create("Skip duplicates").unwrap();
        assert_eq!(
            skipped.add_tracks_with_duplicates(&[track(1), track(1)], false),
            Ok(1)
        );
        assert_eq!(skipped.playlists()[0].track_ids, [1]);

        let mut kept = PlaylistCollection::default();
        kept.create("Keep duplicates").unwrap();
        assert_eq!(
            kept.add_tracks_with_duplicates(&[track(1), track(1)], true),
            Ok(2)
        );
        assert_eq!(kept.playlists()[0].track_ids, [1, 1]);
    }

    #[test]
    fn tracks_cannot_be_added_without_a_selected_playlist() {
        let mut playlists = PlaylistCollection::default();
        assert_eq!(
            playlists.add_tracks(&[track(1)]),
            Err(PlaylistError::NoPlaylistSelected)
        );
    }

    #[test]
    fn shuffling_selected_playlist_changes_order_and_preserves_each_track_once() {
        let mut playlists = PlaylistCollection::default();
        playlists.create("Road Mix").unwrap();
        playlists
            .add_tracks(&[track(1), track(2), track(3), track(4), track(5)])
            .unwrap();
        let original = playlists.selected_playlist().unwrap().track_ids.clone();
        assert!(playlists.can_shuffle_selected());

        playlists
            .shuffle_selected_with_rng(&mut fastrand::Rng::with_seed(0x5eed))
            .unwrap();

        let shuffled = &playlists.selected_playlist().unwrap().track_ids;
        assert_ne!(shuffled, &original);
        let mut sorted_shuffled = shuffled.clone();
        sorted_shuffled.sort_unstable();
        assert_eq!(sorted_shuffled, original);
    }

    #[test]
    fn playlist_occurrence_edits_preserve_duplicates_and_order() {
        let mut playlists = PlaylistCollection::default();
        playlists.create("Duplicates").unwrap();
        playlists
            .add_tracks_with_duplicates(&[track(1), track(1), track(2)], true)
            .unwrap();

        assert!(playlists.move_selected_entry(0, 2));
        assert_eq!(playlists.playlists()[0].track_ids, [1, 2, 1]);
        assert!(playlists.remove_selected_entry(1));
        assert_eq!(playlists.playlists()[0].track_ids, [1, 1]);
        assert!(!playlists.remove_selected_entry(2));
        assert!(!playlists.move_selected_entry(0, 2));
    }

    #[test]
    fn shuffle_by_id_does_not_change_the_selected_playlist() {
        let mut playlists = PlaylistCollection::default();
        let first = playlists.create("First").unwrap();
        playlists
            .add_tracks(&[track(1), track(2), track(3), track(4)])
            .unwrap();
        let selected = playlists.create("Selected").unwrap();
        let original = playlists.playlists()[0].track_ids.clone();

        playlists.shuffle_playlist(first).unwrap();

        assert_eq!(playlists.selected_id(), Some(selected));
        let shuffled = &playlists.playlists()[0].track_ids;
        let mut sorted = shuffled.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, original);
    }

    #[test]
    fn editing_an_open_playlist_does_not_change_the_add_destination() {
        let mut playlists = PlaylistCollection::default();
        let opened = playlists.create("Opened").unwrap();
        playlists
            .add_tracks_with_duplicates(&[track(1), track(2), track(1)], true)
            .unwrap();
        let target = playlists.create("Destination").unwrap();
        assert_eq!(
            playlists.rename(opened, " Destination "),
            Err(PlaylistError::DuplicateName)
        );
        assert_eq!(
            playlists.rename(opened, "  "),
            Err(PlaylistError::EmptyName)
        );
        assert_eq!(playlists.rename(opened, " Renamed "), Ok(()));
        assert!(playlists.move_entry(opened, 2, 0));
        assert!(playlists.remove_entry(opened, 1));
        assert_eq!(playlists.playlists()[0].name, "Renamed");
        assert_eq!(playlists.playlists()[0].track_ids, [1, 2]);
        assert_eq!(playlists.selected_id(), Some(target));
        assert!(playlists.remove(opened));
        assert_eq!(playlists.selected_id(), Some(target));
        assert!(!playlists.remove(opened));
        assert!(playlists.remove(target));
        assert_eq!(playlists.selected_id(), None);
    }
}
