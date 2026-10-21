use crate::model::{MediaDetails, Track};
use crate::playlists::Playlist;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct MusicGroup {
    pub id: i32,
    pub title: String,
    pub detail: String,
    pub artwork: Option<PathBuf>,
    pub count: usize,
}

fn group_key(track: &Track, details: &HashMap<i32, MediaDetails>, view: &str) -> String {
    let details = details.get(&track.id).cloned().unwrap_or_default();
    match view {
        "artists" => nonempty(&details.artist, "Unknown artist").into(),
        "genres" => nonempty(&details.genre, "Unknown genre").into(),
        _ => format!(
            "{}\u{1f}{}",
            album_name(&details),
            album_performer(&details)
        ),
    }
}

fn album_name(details: &MediaDetails) -> &str {
    nonempty(&details.album, "Unknown album")
}

fn album_performer(details: &MediaDetails) -> &str {
    nonempty(
        &details.album_artist,
        nonempty(&details.artist, "Unknown artist"),
    )
}

fn nonempty<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.trim().is_empty() {
        fallback
    } else {
        value
    }
}

pub fn groups(
    tracks: &[Track],
    details: &HashMap<i32, MediaDetails>,
    view: &str,
    query: &str,
) -> Vec<MusicGroup> {
    let mut groups = BTreeMap::<String, MusicGroup>::new();
    for track in tracks
        .iter()
        .filter(|track| matches_query(track, details.get(&track.id), query))
    {
        let key = group_key(track, details, view);
        let tags = details.get(&track.id).cloned().unwrap_or_default();
        let (title, detail) = match view {
            "artists" => (
                nonempty(&tags.artist, "Unknown artist").to_owned(),
                "Artist".into(),
            ),
            "genres" => (
                nonempty(&tags.genre, "Unknown genre").to_owned(),
                "Genre".into(),
            ),
            _ => (
                nonempty(&tags.album, "Unknown album").to_owned(),
                album_performer(&tags).to_owned(),
            ),
        };
        let group = groups.entry(key).or_insert(MusicGroup {
            id: track.id,
            title,
            detail,
            artwork: None,
            count: 0,
        });
        group.count += 1;
        if group.artwork.is_none() {
            group.artwork = tags.artwork;
        }
    }
    groups.into_values().collect()
}

pub fn collection(
    tracks: &[Track],
    details: &HashMap<i32, MediaDetails>,
    view: &str,
    group_id: Option<i32>,
    playlist: Option<&Playlist>,
    query: &str,
) -> Vec<Track> {
    let selected_key = group_id
        .and_then(|id| tracks.iter().find(|track| track.id == id))
        .map(|track| group_key(track, details, view));
    let candidates: Vec<&Track> = if view == "playlists" {
        playlist
            .map(|playlist| {
                playlist
                    .track_ids
                    .iter()
                    .filter_map(|id| tracks.iter().find(|track| track.id == *id))
                    .collect()
            })
            .unwrap_or_default()
    } else if view == "tracks" {
        tracks.iter().collect()
    } else if let Some(key) = selected_key {
        tracks
            .iter()
            .filter(|track| group_key(track, details, view) == key)
            .collect()
    } else {
        Vec::new()
    };
    let mut result: Vec<Track> = candidates
        .into_iter()
        .filter(|track| matches_query(track, details.get(&track.id), query))
        .cloned()
        .collect();
    if view == "albums" {
        result.sort_by_key(|track| {
            let tags = details.get(&track.id).cloned().unwrap_or_default();
            (
                tags.disc_number.unwrap_or(1),
                tags.track_number.unwrap_or(u32::MAX),
                track.title.to_lowercase(),
            )
        });
    }
    result
}

pub fn matches_query(track: &Track, details: Option<&MediaDetails>, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty()
        || track.title.to_lowercase().contains(&query)
        || track
            .path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().to_lowercase().contains(&query))
        || details.is_some_and(|tags| {
            [&tags.artist, &tags.album, &tags.album_artist, &tags.genre]
                .iter()
                .any(|value| value.to_lowercase().contains(&query))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn album_order_and_playlist_duplicates_define_the_actual_play_collection() {
        let tracks: Vec<Track> = (1..=3)
            .map(|id| Track {
                id,
                path: PathBuf::from(format!("{id}.mp3")),
                title: format!("Track{id}"),
                format: "MP3".into(),
            })
            .collect();
        let details: HashMap<_, _> = [(1, 10), (2, 2), (3, 1)]
            .into_iter()
            .map(|(id, number)| {
                (
                    id,
                    MediaDetails {
                        album: "Evening".into(),
                        artist: "Artist".into(),
                        track_number: Some(number),
                        ..MediaDetails::default()
                    },
                )
            })
            .collect();
        assert_eq!(
            collection(&tracks, &details, "albums", Some(1), None, "")
                .iter()
                .map(|track| track.id)
                .collect::<Vec<_>>(),
            [3, 2, 1]
        );
        let playlist = Playlist {
            id: 1,
            name: "Mix".into(),
            track_ids: vec![3, 1, 3],
        };
        assert_eq!(
            collection(&tracks, &details, "playlists", None, Some(&playlist), "")
                .iter()
                .map(|track| track.id)
                .collect::<Vec<_>>(),
            [3, 1, 3]
        );
        assert_eq!(groups(&tracks, &details, "artists", "Artist").len(), 1);
        assert!(matches_query(&tracks[0], details.get(&1), "evening"));
    }

    #[test]
    fn compilation_tracks_group_by_album_artist_then_fall_back_to_track_artist() {
        let tracks = vec![
            Track {
                id: 1,
                path: PathBuf::from("one.mp3"),
                title: "One".into(),
                format: "MP3".into(),
            },
            Track {
                id: 2,
                path: PathBuf::from("two.mp3"),
                title: "Two".into(),
                format: "MP3".into(),
            },
            Track {
                id: 3,
                path: PathBuf::from("solo.mp3"),
                title: "Solo".into(),
                format: "MP3".into(),
            },
        ];
        let details = HashMap::from([
            (
                1,
                MediaDetails {
                    album: "Compilation".into(),
                    artist: "Singer One".into(),
                    album_artist: "Various Artists".into(),
                    ..MediaDetails::default()
                },
            ),
            (
                2,
                MediaDetails {
                    album: "Compilation".into(),
                    artist: "Singer Two".into(),
                    album_artist: "Various Artists".into(),
                    ..MediaDetails::default()
                },
            ),
            (
                3,
                MediaDetails {
                    album: "Solo Album".into(),
                    artist: "Solo Artist".into(),
                    ..MediaDetails::default()
                },
            ),
        ]);

        let groups = groups(&tracks, &details, "albums", "");

        assert_eq!(groups.len(), 2);
        let compilation = groups
            .iter()
            .find(|group| group.title == "Compilation")
            .unwrap();
        assert_eq!(compilation.detail, "Various Artists");
        assert_eq!(compilation.count, 2);
        let solo = groups
            .iter()
            .find(|group| group.title == "Solo Album")
            .unwrap();
        assert_eq!(solo.detail, "Solo Artist");
        assert_eq!(solo.count, 1);
    }
}
