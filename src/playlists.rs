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
            if !playlist.track_ids.contains(&track.id) {
                playlist.track_ids.push(track.id);
                added += 1;
            }
        }
        Ok(added)
    }

    pub fn shuffle_selected(&mut self) -> Result<(), PlaylistError> {
        self.shuffle_selected_with_rng(&mut fastrand::Rng::new())
    }

    fn shuffle_selected_with_rng(&mut self, rng: &mut fastrand::Rng) -> Result<(), PlaylistError> {
        let id = self.selected_id.ok_or(PlaylistError::NoPlaylistSelected)?;
        let playlist = self
            .playlists
            .iter_mut()
            .find(|playlist| playlist.id == id)
            .ok_or(PlaylistError::NoPlaylistSelected)?;
        rng.shuffle(&mut playlist.track_ids);
        Ok(())
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
}
