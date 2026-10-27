use image::{DynamicImage, ImageFormat, ImageReader, Limits};
use lofty::config::ParseOptions;
use lofty::file::TaggedFileExt;
use lofty::picture::PictureType;
use lofty::probe::Probe;
use lofty::tag::{Accessor, ItemKey};
use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::header::LOCATION;
use reqwest::redirect::Policy;
use reqwest::{StatusCode, Url};
use serde_json::Value;
use std::borrow::Cow;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_ARTWORK_BYTES: usize = 12 * 1024 * 1024;
const MAX_DECODED_ARTWORK_BYTES: u64 = 32 * 1024 * 1024;
const MAX_ARTWORK_DIMENSION: u32 = 8192;
const MAX_CACHED_ARTWORK_DIMENSION: u32 = 512;
const ARTWORK_CACHE_BUDGET: u64 = 256 * 1024 * 1024;
const MAX_JSON_BYTES: usize = 1024 * 1024;
const MAX_TMDB_CANDIDATES: usize = 8;
const MAX_TMDB_PREVIEW_BYTES: usize = 2 * 1024 * 1024;
const MAX_YOUTUBE_PAGE_BYTES: usize = 2 * 1024 * 1024;
const MAX_YOUTUBE_INITIAL_DATA_BYTES: usize = MAX_JSON_BYTES;
const MAX_YOUTUBE_JSON_NODES: usize = 20_000;
const MAX_YOUTUBE_CANDIDATES: usize = 64;
const MAX_QUERY_FIELD_BYTES: usize = 512;
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const MISS_CACHE_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const YOUTUBE_REQUEST_INTERVAL: Duration = Duration::from_millis(1100);
const MAX_CAA_REDIRECTS: usize = 5;
const PROVIDER_CACHE_VERSION: u8 = 3;
const TMDB_API_BASE: &str = "https://api.themoviedb.org/3";
const TMDB_API_HOST: &str = "api.themoviedb.org";
const TMDB_IMAGE_HOST: &str = "image.tmdb.org";
const MUSICBRAINZ_SEARCH_URL: &str = "https://musicbrainz.org/ws/2/release/";
const COVER_ART_ARCHIVE_HOST: &str = "coverartarchive.org";
const YOUTUBE_IMAGE_HOST: &str = "i.ytimg.com";
const YOUTUBE_SEARCH_HOST: &str = "www.youtube.com";
static ARTWORK_CACHE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LocalMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub genre: Option<String>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub artwork: Option<PathBuf>,
}

#[derive(Default)]
struct AudioTags {
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    album_artist: Option<String>,
    genre: Option<String>,
    track_number: Option<u32>,
    disc_number: Option<u32>,
    embedded_art: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtworkRequest {
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub kind: ArtworkKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TmdbCandidate {
    pub id: u64,
    pub title: String,
    pub year: Option<u16>,
    pub poster: Option<PathBuf>,
    pub genre: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TmdbArtwork {
    pub poster: Option<PathBuf>,
    pub genre: String,
}

pub(crate) fn automatic_music_artwork_request(metadata: &LocalMetadata) -> Option<ArtworkRequest> {
    if metadata.artwork.is_some() {
        return None;
    }
    let title =
        metadata.title.as_deref().map(str::trim).filter(|title| {
            !title.is_empty() && normalized_match(title) != "unknown audio track"
        })?;
    Some(ArtworkRequest {
        title: title.to_owned(),
        artist: metadata.artist.clone(),
        album: metadata.album.clone(),
        kind: ArtworkKind::Music,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ArtworkKind {
    Movie,
    Series,
    Music,
}

/// Read audio tags and embedded art, then use a nearby named image as a fallback.
/// All cache files are written under the caller-provided app cache; media files are only read.
pub fn read_local(path: &Path, cache: &Path) -> Result<LocalMetadata, String> {
    File::open(path).map_err(|_| "Unable to read the local media file.".to_owned())?;

    let is_audio = crate::model::is_supported_audio_path(path);
    let mut metadata = LocalMetadata {
        title: Some(filename_title(path, is_audio)),
        ..LocalMetadata::default()
    };

    if is_audio {
        if let Some(tags) = read_audio_tags(path) {
            metadata.title = tags.title.or(metadata.title);
            metadata.artist = tags.artist;
            metadata.album = tags.album;
            metadata.album_artist = tags.album_artist;
            metadata.genre = tags.genre;
            metadata.track_number = tags.track_number;
            metadata.disc_number = tags.disc_number;
            if let Some(data) = tags.embedded_art {
                if validate_image(&data).is_ok() {
                    metadata.artwork = Some(cache_artwork(&data, cache)?);
                }
            }
        }
    }

    if metadata.artwork.is_none() {
        metadata.artwork = nearby_artwork(path, cache, is_video_path(path))?;
    }

    Ok(metadata)
}

/// Look up movie, TV, or music artwork. Music lookup also supports automatic metadata jobs.
pub fn lookup_artwork(
    request: &ArtworkRequest,
    cache: &Path,
    tmdb_token: Option<&str>,
) -> Result<Option<PathBuf>, String> {
    lookup_artwork_with_cancel(request, cache, tmdb_token, &AtomicBool::new(false))
}

pub(crate) fn lookup_artwork_with_cancel(
    request: &ArtworkRequest,
    cache: &Path,
    tmdb_token: Option<&str>,
    stopping: &AtomicBool,
) -> Result<Option<PathBuf>, String> {
    check_cancelled(stopping)?;
    validate_request(request)?;
    let key = request_cache_key("provider", request);
    match read_lookup_cache(cache, &key) {
        CachedLookup::Artwork(path) => return Ok(Some(path)),
        CachedLookup::NoArtwork => return Ok(None),
        CachedLookup::Miss => {}
    }

    let client = http_client()?;
    let bytes = match request.kind {
        ArtworkKind::Movie | ArtworkKind::Series => {
            let token = tmdb_token
                .filter(|token| !token.trim().is_empty())
                .ok_or_else(|| {
                    "A TMDB read token is required for movie or TV artwork.".to_owned()
                })?;
            if token.len() > 8192 || token.contains(['\r', '\n']) {
                return Err("The TMDB token is invalid.".to_owned());
            }
            lookup_tmdb(&client, request, token, stopping)?
        }
        ArtworkKind::Music => lookup_music(&client, request, stopping)?,
    };

    check_cancelled(stopping)?;
    let artwork = bytes
        .as_deref()
        .map(|bytes| cache_artwork(bytes, cache))
        .transpose()?;
    check_cancelled(stopping)?;
    store_lookup_cache(cache, &key, artwork.as_deref(), bytes.is_none())?;
    Ok(artwork)
}

pub(crate) fn search_tmdb_candidates_with_cancel(
    request: &ArtworkRequest,
    cache: &Path,
    tmdb_token: Option<&str>,
    stopping: &AtomicBool,
) -> Result<Vec<TmdbCandidate>, String> {
    check_cancelled(stopping)?;
    validate_request(request)?;
    if request.kind == ArtworkKind::Music {
        return Err("TMDB artwork is only available for video.".to_owned());
    }
    let token = validate_tmdb_token(tmdb_token)?;
    let client = http_client()?;
    let (title, year) = parse_artwork_title(&request.title);
    let search_url = tmdb_search_url(request.kind, &title, year)?;
    let response = send_tmdb_json(&client, client.get(search_url), token, stopping)?;
    check_cancelled(stopping)?;

    let genres_url = tmdb_genres_url(request.kind)?;
    let genre_names = send_tmdb_json(&client, client.get(genres_url), token, stopping)?;
    let genre_names = tmdb_genre_name_map(&genre_names)?;
    let parsed = parse_tmdb_candidates(&response, &genre_names, request.kind)?;
    if parsed.is_empty() {
        return Ok(Vec::new());
    }

    let mut candidates = parsed
        .iter()
        .map(|(candidate, _)| candidate.clone())
        .collect::<Vec<_>>();
    let previews = parsed
        .into_iter()
        .enumerate()
        .filter_map(|(index, (_, poster_path))| poster_path.map(|path| (index, path)))
        .collect::<Vec<_>>();
    if !previews.is_empty() {
        let configuration_url = tmdb_configuration_url()?;
        let configuration =
            send_tmdb_json(&client, client.get(configuration_url), token, stopping)?;
        let preview_urls = previews
            .into_iter()
            .filter_map(|(index, poster_path)| {
                tmdb_image_url_for_size(&configuration, &poster_path, Some("w185"))
                    .ok()
                    .map(|url| (index, url))
            })
            .collect::<Vec<_>>();
        download_tmdb_previews(&client, preview_urls, &mut candidates, cache, stopping)?;
    }
    check_cancelled(stopping)?;
    Ok(candidates)
}

pub(crate) fn download_tmdb_candidate_with_cancel(
    kind: ArtworkKind,
    tmdb_id: u64,
    cache: &Path,
    tmdb_token: Option<&str>,
    stopping: &AtomicBool,
) -> Result<TmdbArtwork, String> {
    check_cancelled(stopping)?;
    if kind == ArtworkKind::Music || tmdb_id == 0 {
        return Err("The TMDB candidate is invalid.".to_owned());
    }
    let token = validate_tmdb_token(tmdb_token)?;
    let client = http_client()?;
    let detail_url = tmdb_detail_url(kind, tmdb_id)?;
    let details = send_tmdb_json(&client, client.get(detail_url), token, stopping)?;
    if details.get("id").and_then(Value::as_u64) != Some(tmdb_id) {
        return Err("TMDB returned a different candidate.".to_owned());
    }
    let genre = tmdb_detail_genres(&details);
    let Some(poster_path) = details
        .get("poster_path")
        .and_then(Value::as_str)
        .filter(|path| safe_tmdb_image_path(path))
    else {
        return Ok(TmdbArtwork {
            poster: None,
            genre,
        });
    };

    let configuration_url = tmdb_configuration_url()?;
    let configuration = send_tmdb_json(&client, client.get(configuration_url), token, stopping)?;
    let image_url = tmdb_image_url(&configuration, poster_path)?;
    let poster = fetch_image(&client, image_url, is_tmdb_image_url, stopping)?
        .map(|bytes| cache_artwork(&bytes, cache))
        .transpose()?;
    check_cancelled(stopping)?;
    Ok(TmdbArtwork { poster, genre })
}

pub(crate) fn lookup_tmdb_genres_with_cancel(
    request: &ArtworkRequest,
    cache: &Path,
    tmdb_token: Option<&str>,
    stopping: &AtomicBool,
) -> Result<Option<String>, String> {
    check_cancelled(stopping)?;
    validate_request(request)?;
    if request.kind == ArtworkKind::Music {
        return Err("TMDB genres are only available for movies and TV.".to_owned());
    }
    let Some(token) = tmdb_token.filter(|token| !token.trim().is_empty()) else {
        return Ok(None);
    };
    let token = validate_tmdb_token(Some(token))?;
    let key = request_cache_key("tmdb-genres", request);
    if let Some(genre) = read_tmdb_genre_cache(cache, &key) {
        return Ok(genre);
    }

    let genre_result = (|| {
        let client = http_client()?;
        let (title, year) = parse_artwork_title(&request.title);
        let search_url = tmdb_search_url(request.kind, &title, year)?;
        let response = send_tmdb_json(&client, client.get(search_url), token, stopping)?;
        validate_tmdb_search_response(&response)?;
        check_cancelled(stopping)?;
        let (match_title, match_year, response) = if let Some((secondary_title, secondary_year)) =
            secondary_tmdb_identity(&response, &title, year, request.kind)
        {
            let secondary_url =
                tmdb_search_url(request.kind, &secondary_title, Some(secondary_year))?;
            let secondary_response =
                send_tmdb_json(&client, client.get(secondary_url), token, stopping)?;
            validate_tmdb_search_response(&secondary_response)?;
            (secondary_title, Some(secondary_year), secondary_response)
        } else {
            (title, year, response)
        };
        check_cancelled(stopping)?;
        unique_tmdb_id(&response, &match_title, match_year, request.kind)
            .map(|id| {
                let detail_url = tmdb_detail_url(request.kind, id)?;
                let details = send_tmdb_json(&client, client.get(detail_url), token, stopping)?;
                if details.get("id").and_then(Value::as_u64) != Some(id) {
                    return Err("TMDB returned a different candidate.".to_owned());
                }
                let genres = tmdb_detail_genres(&details);
                Ok((!genres.is_empty()).then_some(genres))
            })
            .transpose()
            .map(Option::flatten)
    })();
    check_cancelled(stopping)?;
    cache_tmdb_genre_result(cache, &key, genre_result)
}

fn validate_tmdb_token(tmdb_token: Option<&str>) -> Result<&str, String> {
    let token = tmdb_token
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| "A TMDB read token is required for movie or TV artwork.".to_owned())?;
    if token.len() > 8192 || token.contains(['\r', '\n']) {
        return Err("The TMDB token is invalid.".to_owned());
    }
    Ok(token)
}

/// Cache a thumbnail for a video URL or ID supplied directly by the user.
/// No YouTube search, page parsing, or media-path data is sent to the provider.
pub fn youtube_artwork(input: &str, cache: &Path) -> Result<PathBuf, String> {
    youtube_artwork_with_cancel(input, cache, &AtomicBool::new(false))
}

pub(crate) fn youtube_artwork_with_cancel(
    input: &str,
    cache: &Path,
    stopping: &AtomicBool,
) -> Result<PathBuf, String> {
    check_cancelled(stopping)?;
    let video_id = youtube_video_id(input)?;
    let key = cache_key(&("youtube", video_id.as_str()));
    match read_lookup_cache(cache, &key) {
        CachedLookup::Artwork(path) => return Ok(path),
        CachedLookup::NoArtwork => return Err("No YouTube thumbnail is cached.".to_owned()),
        CachedLookup::Miss => {}
    }

    let url = Url::parse(&format!(
        "https://{YOUTUBE_IMAGE_HOST}/vi/{video_id}/hqdefault.jpg"
    ))
    .map_err(|_| "The YouTube thumbnail URL is invalid.".to_owned())?;
    let client = http_client()?;
    let bytes = fetch_image(&client, url, is_youtube_image_url, stopping)?
        .ok_or_else(|| "YouTube did not provide a thumbnail for that video.".to_owned())?;
    check_cancelled(stopping)?;
    let artwork = cache_artwork(&bytes, cache)?;
    check_cancelled(stopping)?;
    store_lookup_cache(cache, &key, Some(&artwork), false)?;
    Ok(artwork)
}

fn read_audio_tags(path: &Path) -> Option<AudioTags> {
    let tagged_file = Probe::open(path)
        .ok()?
        .options(ParseOptions::new().read_properties(false))
        .read()
        .ok()?;
    let tag = tagged_file
        .primary_tag()
        .or_else(|| tagged_file.first_tag());
    let Some(tag) = tag else {
        return Some(AudioTags::default());
    };

    let picture = tag
        .pictures()
        .iter()
        .find(|picture| picture.pic_type() == PictureType::CoverFront)
        .or_else(|| tag.pictures().first())
        .and_then(|picture| {
            let data = picture.data();
            (data.len() <= MAX_ARTWORK_BYTES).then(|| data.to_vec())
        });

    Some(AudioTags {
        title: owned_text(tag.title()),
        artist: owned_text(tag.artist()),
        album: owned_text(tag.album()),
        album_artist: tag
            .get_string(ItemKey::AlbumArtist)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
        genre: owned_text(tag.genre()),
        track_number: tag.track(),
        disc_number: tag.disk(),
        embedded_art: picture,
    })
}

fn owned_text(value: Option<Cow<'_, str>>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn filename_title(path: &Path, is_audio: bool) -> String {
    if is_audio {
        return crate::model::readable_filename(path);
    }
    let raw = path
        .file_stem()
        .unwrap_or_else(|| path.file_name().unwrap_or_default())
        .to_string_lossy()
        .trim()
        .to_owned();
    let (title, year) = parse_artwork_title(&raw);
    if let Some(year) = year {
        format!("{title} ({year})")
    } else {
        title
    }
}

fn parse_artwork_title(value: &str) -> (String, Option<u16>) {
    let spaced = value.replace(['.', '_'], " ");
    let mut words = spaced.split_whitespace().collect::<Vec<_>>();
    if let Some(suffix_start) = words.iter().position(|word| is_video_release_suffix(word)) {
        words.truncate(suffix_start);
    }

    let title = words.join(" ");
    if let Some((title, year)) = trailing_wrapped_release_year(&title) {
        return (title.to_owned(), Some(year));
    }

    if title.is_empty() {
        (value.trim().to_owned(), None)
    } else {
        (title, None)
    }
}

fn trailing_wrapped_release_year(value: &str) -> Option<(&str, u16)> {
    let value = value.trim_end();
    let close = value.chars().last()?;
    let open = match close {
        ')' => '(',
        ']' => '[',
        _ => return None,
    };
    let open_index = value.rfind(open)?;
    let year =
        parse_release_year(&value[open_index + open.len_utf8()..value.len() - close.len_utf8()])?;
    let title = value[..open_index].trim_end();
    (!title.is_empty()).then_some((title, year))
}

fn parse_release_year(value: &str) -> Option<u16> {
    let value = value.trim_matches(|character| matches!(character, '(' | ')' | '[' | ']'));
    if value.len() != 4 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let year = value.parse::<u16>().ok()?;
    let current_year = (1970
        + SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            / 31_556_952) as u16;
    (1870..=current_year.saturating_add(1))
        .contains(&year)
        .then_some(year)
}

fn split_trailing_bare_release_year(title: &str) -> Option<(String, u16)> {
    let (title, year) = title.rsplit_once(' ')?;
    let title = title.trim_end();
    if title.is_empty() {
        return None;
    }
    Some((title.to_owned(), parse_release_year(year)?))
}

fn is_video_release_suffix(value: &str) -> bool {
    let normalized = value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    matches!(
        normalized.as_str(),
        "480p"
            | "576p"
            | "720p"
            | "1080p"
            | "1440p"
            | "2160p"
            | "4320p"
            | "4k"
            | "8k"
            | "bluray"
            | "brrip"
            | "bdrip"
            | "dvdrip"
            | "webdl"
            | "webrip"
            | "hdtv"
            | "remux"
            | "x264"
            | "x265"
            | "h264"
            | "h265"
            | "hevc"
            | "av1"
            | "hdr"
            | "hdr10"
    )
}

fn nearby_artwork(path: &Path, cache: &Path, is_video: bool) -> Result<Option<PathBuf>, String> {
    const MUSIC_NAMES: &[&str] = &["cover", "folder", "front", "album", "poster"];
    const VIDEO_NAMES: &[&str] = &["poster", "folder", "cover", "front", "fanart", "movie"];
    const EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp"];
    const MAX_DIRECTORY_ENTRIES: usize = 10_000;

    let Some(directory) = path.parent() else {
        return Ok(None);
    };
    let names = if is_video { VIDEO_NAMES } else { MUSIC_NAMES };
    let Ok(entries) = fs::read_dir(directory) else {
        return Ok(None);
    };
    let mut candidates = Vec::new();
    for entry in entries.take(MAX_DIRECTORY_ENTRIES).flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let candidate = entry.path();
        let Some(stem) = candidate.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        let Some(extension) = candidate.extension().and_then(|value| value.to_str()) else {
            continue;
        };
        let Some(name_rank) = names
            .iter()
            .position(|name| stem.eq_ignore_ascii_case(name))
        else {
            continue;
        };
        let Some(extension_rank) = EXTENSIONS
            .iter()
            .position(|allowed| extension.eq_ignore_ascii_case(allowed))
        else {
            continue;
        };
        candidates.push(((name_rank, extension_rank), candidate));
    }
    candidates.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));

    for (_, candidate) in candidates {
        let Ok(bytes) = read_bounded_file(&candidate, MAX_ARTWORK_BYTES) else {
            continue;
        };
        if validate_image(&bytes).is_err() {
            continue;
        }
        return cache_artwork(&bytes, cache).map(Some);
    }
    Ok(None)
}

fn is_video_path(path: &Path) -> bool {
    const EXTENSIONS: &[&str] = &[
        "3g2", "3gp", "asf", "avi", "divx", "dv", "flv", "m2ts", "m2v", "m4v", "mkv", "mov", "mp2",
        "mp4", "mpe", "mpeg", "mpg", "mts", "mxf", "ogv", "qt", "rm", "rmvb", "ts", "vob", "webm",
        "wmv",
    ];
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| {
            EXTENSIONS
                .iter()
                .any(|allowed| extension.eq_ignore_ascii_case(allowed))
        })
}

fn read_bounded_file(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "Unable to inspect an artwork file.".to_owned())?;
    if !metadata.file_type().is_file() || metadata.len() > limit as u64 {
        return Err("The artwork file is not a bounded regular file.".to_owned());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)
        .and_then(|file| file.take(limit as u64 + 1).read_to_end(&mut bytes))
        .map_err(|_| "Unable to read an artwork file.".to_owned())?;
    if bytes.len() > limit {
        return Err("The artwork file exceeds the byte limit.".to_owned());
    }
    Ok(bytes)
}

fn validate_image(bytes: &[u8]) -> Result<ImageFormat, String> {
    decode_image(bytes).map(|(_, format)| format)
}

fn decode_image(bytes: &[u8]) -> Result<(DynamicImage, ImageFormat), String> {
    if bytes.is_empty() || bytes.len() > MAX_ARTWORK_BYTES {
        return Err("The artwork exceeds the byte limit.".to_owned());
    }

    let mut reader = image_reader(bytes)?;
    let format = reader
        .format()
        .filter(|format| {
            matches!(
                format,
                ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::WebP
            )
        })
        .ok_or_else(|| "The artwork format is unsupported.".to_owned())?;
    reader.limits(image_limits());
    let (width, height) = reader
        .into_dimensions()
        .map_err(|_| "The artwork dimensions are invalid.".to_owned())?;
    let decoded_size = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "The artwork dimensions are too large.".to_owned())?;
    if width == 0
        || height == 0
        || width > MAX_ARTWORK_DIMENSION
        || height > MAX_ARTWORK_DIMENSION
        || decoded_size > MAX_DECODED_ARTWORK_BYTES
    {
        return Err("The artwork dimensions exceed the supported limit.".to_owned());
    }

    let mut reader = image_reader(bytes)?;
    reader.limits(image_limits());
    let decoded = reader
        .decode()
        .map_err(|_| "The artwork image is corrupt or exceeds the decode limit.".to_owned())?;
    Ok((decoded, format))
}

fn image_reader(bytes: &[u8]) -> Result<ImageReader<Cursor<&[u8]>>, String> {
    ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| "The artwork format could not be read.".to_owned())
}

fn image_limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_ARTWORK_DIMENSION);
    limits.max_image_height = Some(MAX_ARTWORK_DIMENSION);
    limits.max_alloc = Some(MAX_DECODED_ARTWORK_BYTES);
    limits
}

fn cache_artwork(bytes: &[u8], cache: &Path) -> Result<PathBuf, String> {
    let (image, _) = decode_image(bytes)?;
    let thumbnail = if image.width() > MAX_CACHED_ARTWORK_DIMENSION
        || image.height() > MAX_CACHED_ARTWORK_DIMENSION
    {
        image.thumbnail(MAX_CACHED_ARTWORK_DIMENSION, MAX_CACHED_ARTWORK_DIMENSION)
    } else {
        image
    };
    let mut encoded = Cursor::new(Vec::new());
    thumbnail
        .write_to(&mut encoded, ImageFormat::Png)
        .map_err(|_| "Unable to encode a cached artwork thumbnail.".to_owned())?;
    let bytes = encoded.into_inner();
    let key = cache_key(&bytes);
    let extension = "png";
    let file_name = format!("{key}.{extension}");
    let _cache_guard = ARTWORK_CACHE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let directory = cache.join("artwork");
    let path = write_atomic(&directory, &file_name, &bytes)?;
    enforce_artwork_budget(&directory, &path)?;
    Ok(path)
}

fn write_atomic(directory: &Path, file_name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    fs::create_dir_all(directory)
        .map_err(|_| "Unable to create the artwork cache directory.".to_owned())?;
    let destination = directory.join(file_name);
    if let Ok(existing) = read_bounded_file(&destination, MAX_ARTWORK_BYTES) {
        if existing == bytes {
            return Ok(destination);
        }
    }

    let temporary = directory.join(format!(
        ".{file_name}.{}-{}.tmp",
        std::process::id(),
        fastrand::u64(..)
    ));
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|_| "Unable to create a temporary artwork cache file.".to_owned())?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| "Unable to write the artwork cache file.".to_owned())?;
        fs::rename(&temporary, &destination)
            .map_err(|_| "Unable to finish the artwork cache write atomically.".to_owned())?;
        Ok(destination.clone())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
        if let Ok(existing) = read_bounded_file(&destination, MAX_ARTWORK_BYTES) {
            if existing == bytes {
                return Ok(destination);
            }
        }
    }
    write_result
}

fn enforce_artwork_budget(directory: &Path, keep: &Path) -> Result<(), String> {
    let entries =
        fs::read_dir(directory).map_err(|_| "Unable to inspect the artwork cache.".to_owned())?;
    let mut files = Vec::new();
    let mut total = 0u64;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !is_artwork_cache_name(name) {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.file_type().is_file() {
            continue;
        }
        total = total.saturating_add(metadata.len());
        files.push((
            metadata.modified().unwrap_or(UNIX_EPOCH),
            metadata.len(),
            path,
        ));
    }
    files.sort_by_key(|(modified, _, path)| (*modified, path.clone()));
    for (_, size, path) in files {
        if total <= ARTWORK_CACHE_BUDGET {
            break;
        }
        if path == keep {
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => total = total.saturating_sub(size),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                total = total.saturating_sub(size)
            }
            Err(_) => return Err("Unable to evict old artwork from the cache.".to_owned()),
        }
    }
    Ok(())
}

fn is_artwork_cache_name(name: &str) -> bool {
    let Some((key, extension)) = name.rsplit_once('.') else {
        return false;
    };
    key.len() == 16
        && key.bytes().all(|byte| byte.is_ascii_hexdigit())
        && matches!(extension, "jpg" | "png" | "webp")
}

fn validate_request(request: &ArtworkRequest) -> Result<(), String> {
    if request.title.trim().is_empty() || request.title.len() > MAX_QUERY_FIELD_BYTES {
        return Err("A title of at most 512 bytes is required.".to_owned());
    }
    for value in [request.artist.as_deref(), request.album.as_deref()]
        .into_iter()
        .flatten()
    {
        if value.len() > MAX_QUERY_FIELD_BYTES {
            return Err("Artwork query fields are limited to 512 bytes.".to_owned());
        }
    }
    Ok(())
}

fn http_client() -> Result<Client, String> {
    Client::builder()
        .timeout(HTTP_TIMEOUT)
        .connect_timeout(Duration::from_secs(5))
        .redirect(Policy::none())
        .user_agent(concat!("rust-vlc/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| "Unable to initialize the artwork network client.".to_owned())
}

fn check_cancelled(stopping: &AtomicBool) -> Result<(), String> {
    if stopping.load(Ordering::Relaxed) {
        Err("Artwork lookup cancelled.".to_owned())
    } else {
        Ok(())
    }
}

fn send_tmdb_json(
    client: &Client,
    request: RequestBuilder,
    token: &str,
    stopping: &AtomicBool,
) -> Result<Value, String> {
    check_cancelled(stopping)?;
    // Check the final URL before attaching the token; redirects are disabled too.
    let request = request
        .bearer_auth(token)
        .build()
        .map_err(|_| "TMDB request could not be prepared.".to_owned())?;
    if !is_tmdb_api_url(request.url()) {
        return Err("TMDB token will only be sent to its API host.".to_owned());
    }
    let response = client
        .execute(request)
        .map_err(|_| "TMDB request failed.".to_owned())?;
    check_cancelled(stopping)?;
    json_response(response, "TMDB")
}

fn json_response(mut response: Response, provider: &str) -> Result<Value, String> {
    let status = response.status();
    if !status.is_success() {
        return Err(format!("{provider} returned HTTP {}.", status.as_u16()));
    }
    let body = read_response_body(&mut response, MAX_JSON_BYTES)?;
    serde_json::from_slice(&body).map_err(|_| format!("{provider} returned an invalid response."))
}

fn read_response_body(response: &mut Response, limit: usize) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err("The provider response exceeds the byte limit.".to_owned());
    }
    let mut bytes = Vec::new();
    response
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Unable to read the provider response.".to_owned())?;
    if bytes.len() > limit {
        return Err("The provider response exceeds the byte limit.".to_owned());
    }
    Ok(bytes)
}

fn lookup_tmdb(
    client: &Client,
    request: &ArtworkRequest,
    token: &str,
    stopping: &AtomicBool,
) -> Result<Option<Vec<u8>>, String> {
    check_cancelled(stopping)?;
    let config_url = Url::parse(&format!("{TMDB_API_BASE}/configuration"))
        .map_err(|_| "TMDB configuration URL is invalid.".to_owned())?;
    if !is_tmdb_api_url(&config_url) {
        return Err("TMDB configuration host is invalid.".to_owned());
    }
    let configuration = send_tmdb_json(client, client.get(config_url), token, stopping)?;

    let (title, year) = parse_artwork_title(&request.title);
    let search_url = tmdb_search_url(request.kind, &title, year)?;
    let results = send_tmdb_json(client, client.get(search_url), token, stopping)?;
    validate_tmdb_search_response(&results)?;
    check_cancelled(stopping)?;
    let poster_path = if let Some((secondary_title, secondary_year)) =
        secondary_tmdb_identity(&results, &title, year, request.kind)
    {
        check_cancelled(stopping)?;
        let secondary_url = tmdb_search_url(request.kind, &secondary_title, Some(secondary_year))?;
        let secondary_results = send_tmdb_json(client, client.get(secondary_url), token, stopping)?;
        validate_tmdb_search_response(&secondary_results)?;
        check_cancelled(stopping)?;
        unique_tmdb_poster(
            &secondary_results,
            &secondary_title,
            Some(secondary_year),
            request.kind,
        )
    } else {
        unique_tmdb_poster(&results, &title, year, request.kind)
    };
    let Some(poster_path) = poster_path else {
        return Ok(None);
    };
    let image_url = tmdb_image_url(&configuration, &poster_path)?;
    fetch_image(client, image_url, is_tmdb_image_url, stopping)
}

fn is_tmdb_api_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some(TMDB_API_HOST)
        && matches!(url.port(), None | Some(443))
        && url.username().is_empty()
        && url.password().is_none()
}

fn is_tmdb_image_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some(TMDB_IMAGE_HOST)
        && matches!(url.port(), None | Some(443))
        && url.username().is_empty()
        && url.password().is_none()
}

fn tmdb_search_url(kind: ArtworkKind, title: &str, year: Option<u16>) -> Result<Url, String> {
    let (endpoint, year_parameter) = match kind {
        ArtworkKind::Movie => ("movie", "primary_release_year"),
        ArtworkKind::Series => ("tv", "first_air_date_year"),
        ArtworkKind::Music => return Err("TMDB artwork is only available for video.".to_owned()),
    };
    let mut url = Url::parse(&format!("{TMDB_API_BASE}/search/{endpoint}"))
        .map_err(|_| "TMDB search URL is invalid.".to_owned())?;
    if !is_tmdb_api_url(&url) {
        return Err("TMDB search host is invalid.".to_owned());
    }
    let mut query = url.query_pairs_mut();
    query.append_pair("query", title);
    if let Some(year) = year {
        query.append_pair(year_parameter, &year.to_string());
    }
    drop(query);
    Ok(url)
}

fn validate_tmdb_search_response(response: &Value) -> Result<(), String> {
    let results = response
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| "TMDB returned an invalid search response.".to_owned())?;
    if response.get("page").and_then(Value::as_u64) != Some(1) {
        return Err("TMDB returned an invalid search response.".to_owned());
    }
    let total_pages = response
        .get("total_pages")
        .and_then(Value::as_u64)
        .ok_or_else(|| "TMDB returned an invalid search response.".to_owned())?;
    if total_pages > 1 || (total_pages == 0 && !results.is_empty()) {
        return Err("TMDB returned an incomplete search response.".to_owned());
    }
    Ok(())
}

fn tmdb_genres_url(kind: ArtworkKind) -> Result<Url, String> {
    let endpoint = match kind {
        ArtworkKind::Movie => "movie",
        ArtworkKind::Series => "tv",
        ArtworkKind::Music => return Err("TMDB artwork is only available for video.".to_owned()),
    };
    let url = Url::parse(&format!(
        "{TMDB_API_BASE}/genre/{endpoint}/list?language=en-US"
    ))
    .map_err(|_| "TMDB genre URL is invalid.".to_owned())?;
    if !is_tmdb_api_url(&url) {
        return Err("TMDB genre host is invalid.".to_owned());
    }
    Ok(url)
}

fn tmdb_detail_url(kind: ArtworkKind, id: u64) -> Result<Url, String> {
    let endpoint = match kind {
        ArtworkKind::Movie => "movie",
        ArtworkKind::Series => "tv",
        ArtworkKind::Music => return Err("TMDB artwork is only available for video.".to_owned()),
    };
    if id == 0 {
        return Err("The TMDB candidate is invalid.".to_owned());
    }
    let url = Url::parse(&format!("{TMDB_API_BASE}/{endpoint}/{id}?language=en-US"))
        .map_err(|_| "TMDB detail URL is invalid.".to_owned())?;
    if !is_tmdb_api_url(&url) {
        return Err("TMDB detail host is invalid.".to_owned());
    }
    Ok(url)
}

fn tmdb_configuration_url() -> Result<Url, String> {
    let url = Url::parse(&format!("{TMDB_API_BASE}/configuration"))
        .map_err(|_| "TMDB configuration URL is invalid.".to_owned())?;
    if !is_tmdb_api_url(&url) {
        return Err("TMDB configuration host is invalid.".to_owned());
    }
    Ok(url)
}

fn tmdb_genre_name_map(response: &Value) -> Result<HashMap<u64, String>, String> {
    let genres = response
        .get("genres")
        .and_then(Value::as_array)
        .ok_or_else(|| "TMDB returned an invalid genre list.".to_owned())?;
    Ok(genres
        .iter()
        .filter_map(|genre| {
            let id = genre.get("id")?.as_u64().filter(|id| *id > 0)?;
            let name = genre.get("name")?.as_str()?.trim();
            (!name.is_empty() && name.len() <= MAX_QUERY_FIELD_BYTES).then(|| (id, name.to_owned()))
        })
        .collect())
}

fn parse_tmdb_candidates(
    response: &Value,
    genre_names: &HashMap<u64, String>,
    kind: ArtworkKind,
) -> Result<Vec<(TmdbCandidate, Option<String>)>, String> {
    let (title_keys, date_key): (&[&str], &str) = match kind {
        ArtworkKind::Movie => (&["title", "original_title"], "release_date"),
        ArtworkKind::Series => (&["name", "original_name"], "first_air_date"),
        ArtworkKind::Music => return Err("TMDB artwork is only available for video.".to_owned()),
    };
    let results = response
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| "TMDB returned an invalid candidate list.".to_owned())?;
    let mut seen = HashMap::new();
    Ok(results
        .iter()
        .filter_map(|result| {
            let id = result.get("id")?.as_u64().filter(|id| *id > 0)?;
            if seen.insert(id, ()).is_some() {
                return None;
            }
            let title = title_keys
                .iter()
                .find_map(|key| result.get(*key).and_then(Value::as_str))?
                .trim();
            if title.is_empty() || title.len() > MAX_QUERY_FIELD_BYTES {
                return None;
            }
            let year = result
                .get(date_key)
                .and_then(Value::as_str)
                .and_then(|date| date.get(..4))
                .and_then(parse_release_year);
            let genre = result
                .get("genre_ids")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_u64)
                .filter_map(|id| genre_names.get(&id))
                .fold(Vec::<String>::new(), |mut names, name| {
                    if !names.iter().any(|known| known.eq_ignore_ascii_case(name)) {
                        names.push(name.clone());
                    }
                    names
                })
                .join("; ");
            let poster_path = result
                .get("poster_path")
                .and_then(Value::as_str)
                .filter(|path| safe_tmdb_image_path(path))
                .map(str::to_owned);
            Some((
                TmdbCandidate {
                    id,
                    title: title.to_owned(),
                    year,
                    poster: None,
                    genre,
                },
                poster_path,
            ))
        })
        .take(MAX_TMDB_CANDIDATES)
        .collect())
}

fn tmdb_detail_genres(details: &Value) -> String {
    details
        .get("genres")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|genre| {
            let name = genre.get("name")?.as_str()?.trim();
            (!name.is_empty() && name.len() <= MAX_QUERY_FIELD_BYTES).then_some(name)
        })
        .fold(Vec::<String>::new(), |mut names, name| {
            if !names.iter().any(|known| known.eq_ignore_ascii_case(name)) {
                names.push(name.to_owned());
            }
            names
        })
        .join("; ")
}

fn download_tmdb_previews(
    client: &Client,
    preview_urls: Vec<(usize, Url)>,
    candidates: &mut [TmdbCandidate],
    cache: &Path,
    stopping: &AtomicBool,
) -> Result<(), String> {
    let responses = thread::scope(|scope| {
        let handles = preview_urls
            .into_iter()
            .map(|(index, url)| {
                let client = client.clone();
                let handle = scope.spawn(move || {
                    fetch_image_with_limit(
                        &client,
                        url,
                        is_tmdb_image_url,
                        stopping,
                        MAX_TMDB_PREVIEW_BYTES,
                    )
                });
                (index, handle)
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|(index, handle)| {
                let result = handle
                    .join()
                    .unwrap_or_else(|_| Err("TMDB poster preview failed.".to_owned()));
                (index, result)
            })
            .collect::<Vec<_>>()
    });
    for (index, result) in responses {
        check_cancelled(stopping)?;
        if let Ok(Some(bytes)) = result {
            if let Ok(path) = cache_artwork(&bytes, cache) {
                candidates[index].poster = Some(path);
            }
        }
    }
    Ok(())
}

fn unique_tmdb_poster(
    response: &Value,
    title: &str,
    year: Option<u16>,
    kind: ArtworkKind,
) -> Option<String> {
    let candidates = exact_tmdb_candidates(response, title, year, kind)?;
    let candidates = candidates
        .into_iter()
        .filter_map(|result| {
            let id = result.get("id")?.as_u64().filter(|id| *id > 0)?;
            let poster = result
                .get("poster_path")
                .and_then(Value::as_str)
                .filter(|path| safe_tmdb_image_path(path))
                .map(str::to_owned);
            Some((id, poster))
        })
        .collect::<Vec<_>>();
    let mut ids = candidates.iter().map(|(id, _)| *id).collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    let [unique_id] = ids.as_slice() else {
        return None;
    };

    let mut posters = candidates
        .into_iter()
        .filter(|(id, _)| id == unique_id)
        .filter_map(|(_, poster)| poster)
        .collect::<Vec<_>>();
    posters.sort();
    posters.dedup();
    (posters.len() == 1).then(|| posters.remove(0))
}

fn secondary_tmdb_identity(
    primary_response: &Value,
    title: &str,
    year: Option<u16>,
    kind: ArtworkKind,
) -> Option<(String, u16)> {
    if year.is_some() || has_exact_tmdb_candidate(primary_response, title, year, kind) {
        return None;
    }
    split_trailing_bare_release_year(title)
}

fn has_exact_tmdb_candidate(
    response: &Value,
    title: &str,
    year: Option<u16>,
    kind: ArtworkKind,
) -> bool {
    exact_tmdb_candidates(response, title, year, kind)
        .is_some_and(|candidates| !candidates.is_empty())
}

fn exact_tmdb_candidates<'a>(
    response: &'a Value,
    title: &str,
    year: Option<u16>,
    kind: ArtworkKind,
) -> Option<Vec<&'a Value>> {
    let (name_keys, date_key): (&[&str], &str) = match kind {
        ArtworkKind::Movie => (&["title", "original_title"], "release_date"),
        ArtworkKind::Series => (&["name", "original_name"], "first_air_date"),
        ArtworkKind::Music => return None,
    };
    let wanted = normalized_match(title);
    Some(
        response
            .get("results")?
            .as_array()?
            .iter()
            .filter(|result| {
                name_keys
                    .iter()
                    .copied()
                    .filter_map(|key| result.get(key).and_then(Value::as_str))
                    .any(|candidate| normalized_match(candidate) == wanted)
                    && year.is_none_or(|expected| {
                        result
                            .get(date_key)
                            .and_then(Value::as_str)
                            .and_then(|date| date.get(..4))
                            .and_then(parse_release_year)
                            == Some(expected)
                    })
            })
            .collect(),
    )
}

fn unique_tmdb_id(
    response: &Value,
    title: &str,
    year: Option<u16>,
    kind: ArtworkKind,
) -> Option<u64> {
    let mut ids = exact_tmdb_candidates(response, title, year, kind)?
        .into_iter()
        .filter_map(|result| result.get("id")?.as_u64().filter(|id| *id > 0))
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    (ids.len() == 1).then(|| ids[0])
}

fn safe_tmdb_image_path(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= 512
        && path
            .split('/')
            .skip(1)
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
        && path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.'))
}

fn tmdb_image_url(configuration: &Value, image_path: &str) -> Result<Url, String> {
    tmdb_image_url_for_size(configuration, image_path, None)
}

fn tmdb_image_url_for_size(
    configuration: &Value,
    image_path: &str,
    preferred_size: Option<&str>,
) -> Result<Url, String> {
    if !safe_tmdb_image_path(image_path) {
        return Err("TMDB returned an invalid poster path.".to_owned());
    }
    let images = configuration
        .get("images")
        .ok_or_else(|| "TMDB image configuration is missing.".to_owned())?;
    let base = images
        .get("secure_base_url")
        .and_then(Value::as_str)
        .and_then(|value| Url::parse(value).ok())
        .ok_or_else(|| "TMDB image base URL is invalid.".to_owned())?;
    if !is_tmdb_image_url(&base) {
        return Err("TMDB image host is invalid.".to_owned());
    }
    let sizes = images
        .get("poster_sizes")
        .and_then(Value::as_array)
        .ok_or_else(|| "TMDB poster sizes are missing.".to_owned())?;
    let valid_size = |size: &&str| {
        *size == "original"
            || size
                .strip_prefix('w')
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    };
    let configured = sizes.iter().filter_map(Value::as_str).collect::<Vec<_>>();
    let size = preferred_size
        .filter(|preferred| configured.contains(preferred) && valid_size(preferred))
        .or_else(|| configured.iter().copied().find(|size| *size == "w500"))
        .or_else(|| configured.iter().copied().rev().find(valid_size))
        .ok_or_else(|| "TMDB has no supported poster size.".to_owned())?;
    let relative = format!("{size}/{}", image_path.trim_start_matches('/'));
    let url = base
        .join(&relative)
        .map_err(|_| "TMDB poster URL is invalid.".to_owned())?;
    if !is_tmdb_image_url(&url) {
        return Err("TMDB poster host is invalid.".to_owned());
    }
    Ok(url)
}

fn lookup_music(
    client: &Client,
    request: &ArtworkRequest,
    stopping: &AtomicBool,
) -> Result<Option<Vec<u8>>, String> {
    check_cancelled(stopping)?;
    match lookup_youtube_music(client, request, stopping) {
        Ok(Some(bytes)) => return Ok(Some(bytes)),
        Err(_) => check_cancelled(stopping)?,
        Ok(None) => {}
    }
    let album = request
        .album
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&request.title);
    let mut query = format!("release:{}", lucene_phrase(album));
    if let Some(artist) = request
        .artist
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        query.push_str(" AND artist:");
        query.push_str(&lucene_phrase(artist));
    }

    wait_for_musicbrainz_slot();
    check_cancelled(stopping)?;
    let response = client
        .get(MUSICBRAINZ_SEARCH_URL)
        .query(&[("query", query.as_str()), ("fmt", "json"), ("limit", "5")])
        .send()
        .map_err(|_| "MusicBrainz request failed.".to_owned())?;
    check_cancelled(stopping)?;
    let response = match response.status() {
        StatusCode::NOT_FOUND => return Ok(None),
        status if status.is_success() => response,
        status => return Err(format!("MusicBrainz returned HTTP {}.", status.as_u16())),
    };
    let response = json_response(response, "MusicBrainz")?;
    let Some(release_id) = unique_musicbrainz_release(&response, album, request.artist.as_deref())
    else {
        return Ok(None);
    };
    fetch_cover_art_archive(client, &release_id, stopping)
}

fn lookup_youtube_music(
    client: &Client,
    request: &ArtworkRequest,
    stopping: &AtomicBool,
) -> Result<Option<Vec<u8>>, String> {
    check_cancelled(stopping)?;
    let url = youtube_search_url(request)?;
    wait_for_youtube_slot(stopping)?;
    let page = fetch_youtube_search_page(client, url, is_youtube_search_url, stopping)?;
    let Some(page) = page else {
        return Ok(None);
    };
    let Some(data) = youtube_initial_data(&page) else {
        return Ok(None);
    };
    let Some(video_id) = best_youtube_music_video(&data, request) else {
        return Ok(None);
    };
    let thumbnail = Url::parse(&format!(
        "https://{YOUTUBE_IMAGE_HOST}/vi/{video_id}/hqdefault.jpg"
    ))
    .map_err(|_| "The YouTube thumbnail URL is invalid.".to_owned())?;
    fetch_image(client, thumbnail, is_youtube_image_url, stopping)
}

fn lucene_phrase(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn unique_musicbrainz_release(
    response: &Value,
    album: &str,
    artist: Option<&str>,
) -> Option<String> {
    let wanted_album = normalized_match(album);
    let wanted_artist = artist.map(normalized_match);
    let mut matches = response
        .get("releases")?
        .as_array()?
        .iter()
        .filter_map(|release| {
            let title = release.get("title")?.as_str()?;
            if normalized_match(title) != wanted_album {
                return None;
            }
            if let Some(wanted_artist) = wanted_artist.as_deref() {
                let credited_artist = artist_credit_name(release.get("artist-credit")?)?;
                if normalized_match(&credited_artist) != wanted_artist {
                    return None;
                }
            }
            let score = release.get("score")?.as_u64()?;
            let id = release.get("id")?.as_str()?;
            (score >= 95 && is_uuid(id)).then(|| (score, id.to_owned()))
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    let (best_score, best_id) = matches.first().cloned()?;
    if matches
        .iter()
        .filter(|(score, _)| *score == best_score)
        .count()
        == 1
    {
        Some(best_id)
    } else {
        None
    }
}

fn artist_credit_name(credit: &Value) -> Option<String> {
    if let Some(name) = credit.as_str() {
        return Some(name.to_owned());
    }
    let parts = credit.as_array()?;
    let mut name = String::new();
    for part in parts {
        name.push_str(part.get("name")?.as_str()?);
        if let Some(joinphrase) = part.get("joinphrase").and_then(Value::as_str) {
            name.push_str(joinphrase);
        }
    }
    Some(name)
}

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn fetch_cover_art_archive(
    client: &Client,
    release_id: &str,
    stopping: &AtomicBool,
) -> Result<Option<Vec<u8>>, String> {
    if !is_uuid(release_id) {
        return Err("MusicBrainz returned an invalid release identifier.".to_owned());
    }
    let url = Url::parse(&format!(
        "https://{COVER_ART_ARCHIVE_HOST}/release/{release_id}/front-500"
    ))
    .map_err(|_| "Cover Art Archive URL is invalid.".to_owned())?;
    follow_cover_art_redirects(client, url, is_archive_image_url, true, stopping)
}

fn follow_cover_art_redirects(
    client: &Client,
    mut url: Url,
    is_allowed_url: fn(&Url) -> bool,
    upgrade_http: bool,
    stopping: &AtomicBool,
) -> Result<Option<Vec<u8>>, String> {
    for redirects_followed in 0..=MAX_CAA_REDIRECTS {
        check_cancelled(stopping)?;
        if !is_allowed_url(&url) {
            return Err("Cover Art Archive redirected to an unsupported host.".to_owned());
        }
        // Build a fresh unauthenticated request for every hop. No headers or credentials from
        // a prior request are copied across hosts.
        let response = client
            .get(url)
            .send()
            .map_err(|_| "Cover Art Archive request failed.".to_owned())?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if response.status().is_redirection() {
            if redirects_followed == MAX_CAA_REDIRECTS {
                return Err("Cover Art Archive exceeded the redirect limit.".to_owned());
            }
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| "Cover Art Archive returned an invalid redirect.".to_owned())?;
            let mut next_url = response
                .url()
                .join(location)
                .map_err(|_| "Cover Art Archive returned an invalid redirect.".to_owned())?;
            if next_url.scheme() == "http"
                && upgrade_http
                && is_archive_download_host(next_url.host_str().unwrap_or_default())
            {
                next_url
                    .set_scheme("https")
                    .map_err(|_| "Cover Art Archive redirect is not secure.".to_owned())?;
            }
            if !is_allowed_url(&next_url) {
                return Err("Cover Art Archive redirected to an unsupported host.".to_owned());
            }
            url = next_url;
            continue;
        }
        if response.status().is_success() {
            return read_image_response(response).map(Some);
        }
        return Err(format!(
            "Cover Art Archive returned HTTP {}.",
            response.status().as_u16()
        ));
    }

    Err("Cover Art Archive exceeded the redirect limit.".to_owned())
}

fn is_archive_host(host: &str) -> bool {
    host == "archive.org" || host.ends_with(".archive.org")
}

fn is_archive_download_host(host: &str) -> bool {
    host == COVER_ART_ARCHIVE_HOST || is_archive_host(host)
}

fn is_archive_image_url(url: &Url) -> bool {
    url.scheme() == "https"
        && matches!(url.port(), None | Some(443))
        && url.username().is_empty()
        && url.password().is_none()
        && is_archive_download_host(url.host_str().unwrap_or_default())
}

fn fetch_image(
    client: &Client,
    url: Url,
    allowed_host: fn(&Url) -> bool,
    stopping: &AtomicBool,
) -> Result<Option<Vec<u8>>, String> {
    fetch_image_with_limit(client, url, allowed_host, stopping, MAX_ARTWORK_BYTES)
}

fn fetch_image_with_limit(
    client: &Client,
    url: Url,
    allowed_host: fn(&Url) -> bool,
    stopping: &AtomicBool,
    max_bytes: usize,
) -> Result<Option<Vec<u8>>, String> {
    check_cancelled(stopping)?;
    if !allowed_host(&url) {
        return Err("Artwork URL is outside the provider's approved host.".to_owned());
    }
    let response = client
        .get(url)
        .send()
        .map_err(|_| "Artwork download failed.".to_owned())?;
    check_cancelled(stopping)?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(format!(
            "Artwork host returned HTTP {}.",
            response.status().as_u16()
        ));
    }
    let bytes = read_image_response_with_limit(response, max_bytes)?;
    Ok(Some(bytes))
}

fn read_image_response(response: Response) -> Result<Vec<u8>, String> {
    read_image_response_with_limit(response, MAX_ARTWORK_BYTES)
}

fn read_image_response_with_limit(
    mut response: Response,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    let bytes = read_response_body(&mut response, max_bytes)?;
    validate_image(&bytes)?;
    Ok(bytes)
}

fn wait_for_musicbrainz_slot() {
    static LAST_REQUEST: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    let last = LAST_REQUEST.get_or_init(|| Mutex::new(None));
    let mut last = last
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(previous) = *last {
        let delay = Duration::from_secs(1).saturating_sub(previous.elapsed());
        if !delay.is_zero() {
            thread::sleep(delay);
        }
    }
    *last = Some(Instant::now());
}

fn wait_for_youtube_slot(stopping: &AtomicBool) -> Result<(), String> {
    static LAST_REQUEST: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    let last = LAST_REQUEST.get_or_init(|| Mutex::new(None));
    let mut last = last
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(previous) = *last {
        let mut delay = YOUTUBE_REQUEST_INTERVAL.saturating_sub(previous.elapsed());
        while !delay.is_zero() {
            check_cancelled(stopping)?;
            thread::sleep(delay.min(Duration::from_millis(50)));
            delay = YOUTUBE_REQUEST_INTERVAL.saturating_sub(previous.elapsed());
        }
    }
    check_cancelled(stopping)?;
    *last = Some(Instant::now());
    Ok(())
}

fn youtube_video_id(input: &str) -> Result<String, String> {
    let input = input.trim();
    if valid_video_id(input) {
        return Ok(input.to_owned());
    }
    let url = Url::parse(input)
        .map_err(|_| "Enter a YouTube video URL or 11-character ID.".to_owned())?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return Err("Only secure YouTube video URLs are accepted.".to_owned());
    }
    let host = url.host_str().unwrap_or_default();
    let id = if matches!(host, "youtu.be" | "www.youtu.be") {
        let mut segments = url.path_segments().into_iter().flatten();
        let id = segments.next().unwrap_or_default().to_owned();
        if segments.next().is_some() {
            return Err("The YouTube URL does not identify one video.".to_owned());
        }
        id
    } else if matches!(
        host,
        "youtube.com" | "www.youtube.com" | "m.youtube.com" | "music.youtube.com"
    ) {
        let path = url
            .path_segments()
            .into_iter()
            .flatten()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        match path.as_slice() {
            [path] if path == "watch" => {
                let ids = url
                    .query_pairs()
                    .filter(|(key, _)| key == "v")
                    .map(|(_, value)| value.into_owned())
                    .collect::<Vec<_>>();
                if ids.len() != 1 {
                    return Err("The YouTube URL must contain one video ID.".to_owned());
                }
                return validate_video_id_owned(ids.into_iter().next().unwrap_or_default());
            }
            [kind, id] if matches!(kind.as_str(), "embed" | "shorts" | "live" | "v") => id.clone(),
            _ => return Err("The YouTube URL does not identify a supported video page.".to_owned()),
        }
    } else {
        return Err("Only YouTube video URLs are accepted.".to_owned());
    };
    validate_video_id_owned(id)
}

fn validate_video_id_owned(id: String) -> Result<String, String> {
    if valid_video_id(&id) {
        Ok(id)
    } else {
        Err("The YouTube video ID is invalid.".to_owned())
    }
}

fn valid_video_id(id: &str) -> bool {
    id.len() == 11
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn youtube_search_url(request: &ArtworkRequest) -> Result<Url, String> {
    let title = clean_music_search_title(&request.title);
    let artist = request.artist.as_deref().map(str::trim).unwrap_or_default();
    let query = if artist.is_empty() {
        title
    } else {
        format!("{artist} {title}")
    };
    if query.trim().is_empty() || query.len() > MAX_QUERY_FIELD_BYTES * 2 {
        return Err("The YouTube music search query is invalid.".to_owned());
    }
    let mut url = Url::parse(&format!("https://{YOUTUBE_SEARCH_HOST}/results"))
        .map_err(|_| "The YouTube music search URL is invalid.".to_owned())?;
    if !is_youtube_search_url(&url) {
        return Err("The YouTube music search host is invalid.".to_owned());
    }
    url.query_pairs_mut().append_pair("search_query", &query);
    Ok(url)
}

fn clean_music_search_title(title: &str) -> String {
    let title = title.trim();
    let digits = title.bytes().take_while(u8::is_ascii_digit).count();
    if !(1..=3).contains(&digits) || digits == title.len() {
        return title.to_owned();
    }
    let rest = &title[digits..];
    let rest = if let Some(rest) = rest.strip_prefix(" - ") {
        rest
    } else if let Some(rest) = rest.strip_prefix(['-', '–', '—', '_']) {
        rest
    } else if let Some(rest) = rest.strip_prefix(". ") {
        rest
    } else {
        return title.to_owned();
    };
    rest.trim().to_owned()
}

fn fetch_youtube_search_page(
    client: &Client,
    url: Url,
    allowed_url: fn(&Url) -> bool,
    stopping: &AtomicBool,
) -> Result<Option<Vec<u8>>, String> {
    check_cancelled(stopping)?;
    if !allowed_url(&url) {
        return Err("YouTube search URL is outside the approved host.".to_owned());
    }
    let response = client
        .get(url)
        .send()
        .map_err(|_| "YouTube music search failed.".to_owned())?;
    check_cancelled(stopping)?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(format!(
            "YouTube music search returned HTTP {}.",
            response.status().as_u16()
        ));
    }
    let mut response = response;
    read_response_body(&mut response, MAX_YOUTUBE_PAGE_BYTES).map(Some)
}

fn youtube_initial_data(page: &[u8]) -> Option<Value> {
    const MARKER: &[u8] = b"ytInitialData";
    let mut search_from = 0;
    while search_from < page.len() {
        let marker = page[search_from..]
            .windows(MARKER.len())
            .position(|window| window == MARKER)?
            + search_from;
        let object_start = marker + MARKER.len();
        let search_end = object_start.saturating_add(128).min(page.len());
        let relative_start = page[object_start..search_end]
            .iter()
            .position(|byte| *byte == b'{');
        if let Some(relative_start) = relative_start {
            let start = object_start + relative_start;
            if let Some(end) = balanced_json_object_end(page, start) {
                if end - start <= MAX_YOUTUBE_INITIAL_DATA_BYTES {
                    if let Ok(data) = serde_json::from_slice(&page[start..end]) {
                        return Some(data);
                    }
                }
            }
        }
        search_from = object_start;
    }
    None
}

fn balanced_json_object_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, byte) in bytes.iter().copied().enumerate().skip(start) {
        if offset - start >= MAX_YOUTUBE_INITIAL_DATA_BYTES {
            return None;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
        } else {
            match byte {
                b'"' => in_string = true,
                b'{' => depth += 1,
                b'}' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(offset + 1);
                    }
                }
                _ => {}
            }
        }
    }
    None
}

struct YoutubeVideoCandidate {
    id: String,
    title: String,
    channel: String,
}

fn best_youtube_music_video(data: &Value, request: &ArtworkRequest) -> Option<String> {
    let wanted_title = music_match_tokens(&clean_music_search_title(&request.title));
    if wanted_title.is_empty() {
        return None;
    }
    let wanted_artist = request
        .artist
        .as_deref()
        .map(music_match_tokens)
        .unwrap_or_default();
    let mut candidates = Vec::new();
    let mut pending = vec![data];
    let mut visited = 0;
    while let Some(value) = pending.pop() {
        visited += 1;
        if visited > MAX_YOUTUBE_JSON_NODES {
            break;
        }
        if let Some(object) = value.as_object() {
            for (key, child) in object {
                if key == "videoRenderer" {
                    if let Some(candidate) = youtube_video_candidate(child) {
                        if !candidates
                            .iter()
                            .any(|existing: &YoutubeVideoCandidate| existing.id == candidate.id)
                        {
                            candidates.push(candidate);
                            if candidates.len() == MAX_YOUTUBE_CANDIDATES {
                                break;
                            }
                        }
                    }
                } else {
                    pending.push(child);
                }
            }
        } else if let Some(array) = value.as_array() {
            pending.extend(array.iter());
        }
        if candidates.len() == MAX_YOUTUBE_CANDIDATES {
            break;
        }
    }

    let mut ranked = candidates
        .into_iter()
        .filter_map(|candidate| {
            youtube_music_match_score(
                &wanted_title,
                &wanted_artist,
                &candidate.title,
                &candidate.channel,
            )
            .map(|score| (score, candidate.id))
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| right.0.total_cmp(&left.0));
    let (best_score, best_id) = ranked.first()?.clone();
    let minimum_score = if wanted_artist.is_empty() { 0.82 } else { 0.80 };
    if best_score < minimum_score
        || ranked
            .get(1)
            .is_some_and(|(score, _)| *score >= minimum_score - 0.04 && best_score - score < 0.08)
    {
        return None;
    }
    Some(best_id)
}

fn youtube_video_candidate(value: &Value) -> Option<YoutubeVideoCandidate> {
    let id = value.get("videoId")?.as_str()?;
    if !valid_video_id(id) {
        return None;
    }
    let title = youtube_renderer_text(value.get("title")?)?;
    let channel = ["ownerText", "longBylineText", "shortBylineText"]
        .into_iter()
        .filter_map(|key| value.get(key).and_then(youtube_renderer_text))
        .next()
        .unwrap_or_default();
    Some(YoutubeVideoCandidate {
        id: id.to_owned(),
        title,
        channel,
    })
}

fn youtube_renderer_text(value: &Value) -> Option<String> {
    if let Some(text) = value.get("simpleText").and_then(Value::as_str) {
        return (text.len() <= MAX_QUERY_FIELD_BYTES).then(|| text.to_owned());
    }
    let mut text = String::new();
    for run in value.get("runs")?.as_array()? {
        let part = run.get("text")?.as_str()?;
        if text.len().saturating_add(part.len()) > MAX_QUERY_FIELD_BYTES {
            return None;
        }
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(part);
    }
    (!text.trim().is_empty()).then_some(text)
}

fn youtube_music_match_score(
    wanted_title: &[String],
    wanted_artist: &[String],
    candidate_title: &str,
    candidate_channel: &str,
) -> Option<f64> {
    let candidate_tokens = music_match_tokens(candidate_title);
    const UNSOLICITED_VARIANTS: &[&str] = &[
        "acoustic",
        "cover",
        "covers",
        "live",
        "karaoke",
        "instrumental",
        "remix",
        "tribute",
        "nightcore",
        "slowed",
        "sped",
        "8d",
    ];
    if candidate_tokens.iter().any(|token| {
        UNSOLICITED_VARIANTS.contains(&token.as_str()) && !wanted_title.contains(token)
    }) {
        return None;
    }
    let artist_removed_title = candidate_tokens
        .iter()
        .filter(|token| !wanted_artist.contains(token))
        .cloned()
        .collect::<Vec<_>>();
    if has_conflicting_numbers(wanted_title, &artist_removed_title) {
        return None;
    }
    let title_score = token_similarity(wanted_title, &artist_removed_title);
    if title_score < 0.72 {
        return None;
    }
    if wanted_artist.is_empty() {
        return Some(title_score);
    }
    let channel_tokens = music_match_tokens(candidate_channel);
    if !wanted_artist
        .iter()
        .all(|token| candidate_tokens.contains(token) || channel_tokens.contains(token))
    {
        return None;
    }
    let artist_score = token_similarity(wanted_artist, &candidate_tokens)
        .max(token_similarity(wanted_artist, &channel_tokens));
    (artist_score >= 0.6).then_some(title_score * 0.75 + artist_score * 0.25)
}

fn music_match_tokens(value: &str) -> Vec<String> {
    const NOISE: &[&str] = &[
        "official",
        "audio",
        "video",
        "lyrics",
        "lyric",
        "music",
        "hd",
        "visualizer",
    ];
    normalized_match(value)
        .split_whitespace()
        .filter(|token| !NOISE.contains(token))
        .map(str::to_owned)
        .collect()
}

fn token_similarity(left: &[String], right: &[String]) -> f64 {
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let intersection = left.iter().filter(|token| right.contains(token)).count();
    let recall = intersection as f64 / left.len() as f64;
    let precision = intersection as f64 / right.len() as f64;
    0.65 * recall + 0.35 * precision
}

fn has_conflicting_numbers(left: &[String], right: &[String]) -> bool {
    fn numbers(tokens: &[String]) -> Vec<&str> {
        tokens
            .iter()
            .filter(|token| token.bytes().all(|byte| byte.is_ascii_digit()))
            .map(String::as_str)
            .collect::<Vec<_>>()
    }
    let left = numbers(left);
    let right = numbers(right);
    !left.is_empty() && !right.is_empty() && left != right
}

fn normalized_match(value: &str) -> String {
    value
        .chars()
        .flat_map(char::to_lowercase)
        .map(|character| {
            if character.is_alphanumeric() {
                character
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn request_cache_key(provider: &str, request: &ArtworkRequest) -> String {
    let (title, year) = match request.kind {
        ArtworkKind::Movie | ArtworkKind::Series => parse_artwork_title(&request.title),
        ArtworkKind::Music => (request.title.trim().to_owned(), None),
    };
    let artist = request.artist.as_deref().map(normalized_match);
    let album = request.album.as_deref().map(normalized_match);
    cache_key(&(
        provider,
        PROVIDER_CACHE_VERSION,
        normalized_match(&title),
        year,
        artist,
        album,
        request.kind,
    ))
}

fn cache_key<T: Hash>(value: &T) -> String {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

enum CachedLookup {
    Miss,
    NoArtwork,
    Artwork(PathBuf),
}

fn read_lookup_cache(cache: &Path, key: &str) -> CachedLookup {
    let _cache_guard = ARTWORK_CACHE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let marker = cache.join("requests").join(format!("{key}.json"));
    let Ok(bytes) = read_bounded_file(&marker, 4096) else {
        return CachedLookup::Miss;
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        let _ = fs::remove_file(marker);
        return CachedLookup::Miss;
    };
    let expires_at = value.get("expires_at").and_then(Value::as_u64).unwrap_or(0);
    let Some(artwork) = value.get("artwork") else {
        let _ = fs::remove_file(marker);
        return CachedLookup::Miss;
    };
    if artwork.is_null() {
        if expires_at <= now_unix_seconds() {
            let _ = fs::remove_file(marker);
            return CachedLookup::Miss;
        }
        return CachedLookup::NoArtwork;
    }
    let Some(file_name) = artwork.as_str() else {
        let _ = fs::remove_file(marker);
        return CachedLookup::Miss;
    };
    if !is_artwork_cache_name(file_name) {
        let _ = fs::remove_file(marker);
        return CachedLookup::Miss;
    }
    let path = cache.join("artwork").join(file_name);
    if read_bounded_file(&path, MAX_ARTWORK_BYTES).is_ok_and(|bytes| validate_image(&bytes).is_ok())
    {
        CachedLookup::Artwork(path)
    } else {
        let _ = fs::remove_file(marker);
        CachedLookup::Miss
    }
}

fn store_lookup_cache(
    cache: &Path,
    key: &str,
    artwork: Option<&Path>,
    is_miss: bool,
) -> Result<(), String> {
    let file_name = artwork
        .map(|path| {
            path.file_name()
                .and_then(|value| value.to_str())
                .filter(|name| is_artwork_cache_name(name))
                .ok_or_else(|| "The artwork cache path is invalid.".to_owned())
        })
        .transpose()?;
    let expires_at = is_miss.then(|| now_unix_seconds().saturating_add(MISS_CACHE_TTL.as_secs()));
    let record = serde_json::json!({
        "expires_at": expires_at,
        "artwork": file_name,
    });
    let bytes = serde_json::to_vec(&record)
        .map_err(|_| "Unable to encode the artwork cache record.".to_owned())?;
    let _cache_guard = ARTWORK_CACHE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    write_atomic(&cache.join("requests"), &format!("{key}.json"), &bytes)?;
    Ok(())
}

fn read_tmdb_genre_cache(cache: &Path, key: &str) -> Option<Option<String>> {
    let _cache_guard = ARTWORK_CACHE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let marker = cache.join("tmdb-genres").join(format!("{key}.json"));
    let bytes = match read_bounded_file(&marker, 4096) {
        Ok(bytes) => bytes,
        Err(_) => return None,
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        let _ = fs::remove_file(marker);
        return None;
    };
    if let Some(genre) = value.get("genre").and_then(Value::as_str) {
        let genre = genre.trim();
        if !genre.is_empty() && genre.len() <= MAX_QUERY_FIELD_BYTES * 8 {
            return Some(Some(genre.to_owned()));
        }
    } else if value.get("genre").is_some_and(Value::is_null) {
        let expires_at = value.get("expires_at").and_then(Value::as_u64).unwrap_or(0);
        if expires_at > now_unix_seconds() {
            return Some(None);
        }
    }
    let _ = fs::remove_file(marker);
    None
}

fn store_tmdb_genre_cache(cache: &Path, key: &str, genre: Option<&str>) -> Result<(), String> {
    let value = match genre {
        Some(genre) => serde_json::json!({"genre": genre}),
        None => serde_json::json!({
            "genre": null,
            "expires_at": now_unix_seconds().saturating_add(MISS_CACHE_TTL.as_secs())
        }),
    };
    let bytes =
        serde_json::to_vec(&value).map_err(|_| "Unable to save TMDB genre metadata.".to_owned())?;
    let _cache_guard = ARTWORK_CACHE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    write_atomic(&cache.join("tmdb-genres"), &format!("{key}.json"), &bytes)?;
    Ok(())
}

fn cache_tmdb_genre_result(
    cache: &Path,
    key: &str,
    result: Result<Option<String>, String>,
) -> Result<Option<String>, String> {
    let genre = result?;
    store_tmdb_genre_cache(cache, key, genre.as_deref())?;
    Ok(genre)
}

fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn validate_youtube_image_host(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some(YOUTUBE_IMAGE_HOST)
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
}

fn is_youtube_search_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some(YOUTUBE_SEARCH_HOST)
        && url.path() == "/results"
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
}

fn is_youtube_image_url(url: &Url) -> bool {
    validate_youtube_image_host(url)
}

#[cfg(test)]
mod tests {
    use super::{
        cache_artwork, cache_tmdb_genre_result, filename_title, http_client, is_archive_image_url,
        is_tmdb_api_url, is_tmdb_image_url, is_uuid, json_response, lookup_artwork,
        normalized_match, parse_artwork_title, parse_tmdb_candidates, read_local,
        read_tmdb_genre_cache, request_cache_key, secondary_tmdb_identity, send_tmdb_json,
        split_trailing_bare_release_year, store_tmdb_genre_cache, tmdb_detail_genres,
        tmdb_detail_url, tmdb_genre_name_map, tmdb_genres_url, tmdb_image_url,
        tmdb_image_url_for_size, tmdb_search_url, unique_musicbrainz_release, unique_tmdb_id,
        unique_tmdb_poster, validate_image, youtube_artwork, youtube_video_id, ArtworkKind,
        ArtworkRequest, LocalMetadata, MAX_ARTWORK_BYTES,
    };
    use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
    use reqwest::StatusCode;
    use reqwest::Url;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc};
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    fn temporary_directory() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after the Unix epoch")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("rust-vlc-metadata-{}-{nonce}", std::process::id()));
        fs::create_dir(&root).expect("create isolated metadata fixture");
        root
    }

    fn png(red: u8) -> Vec<u8> {
        let image = RgbaImage::from_pixel(2, 2, Rgba([red, 20, 30, 255]));
        let mut output = std::io::Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image)
            .write_to(&mut output, ImageFormat::Png)
            .expect("encode PNG fixture");
        output.into_inner()
    }

    fn synchsafe(value: usize) -> [u8; 4] {
        [
            ((value >> 21) & 0x7f) as u8,
            ((value >> 14) & 0x7f) as u8,
            ((value >> 7) & 0x7f) as u8,
            (value & 0x7f) as u8,
        ]
    }

    fn id3_frame(name: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut frame = name.to_vec();
        frame.extend_from_slice(&synchsafe(payload.len()));
        frame.extend_from_slice(&[0, 0]);
        frame.extend_from_slice(payload);
        frame
    }

    fn id3_text_frame(name: &[u8; 4], value: &str) -> Vec<u8> {
        let mut payload = vec![3];
        payload.extend_from_slice(value.as_bytes());
        id3_frame(name, &payload)
    }

    fn mp3_with_tags_and_art(art: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend(id3_text_frame(b"TIT2", "夜の雨"));
        body.extend(id3_text_frame(b"TPE1", "Étoile"));
        body.extend(id3_text_frame(b"TPE2", "Various Artists"));
        body.extend(id3_text_frame(b"TALB", "Collection"));
        body.extend(id3_text_frame(b"TCON", "Jazz"));
        body.extend(id3_text_frame(b"TRCK", "7"));
        body.extend(id3_text_frame(b"TPOS", "2"));
        let mut picture = vec![0];
        picture.extend_from_slice(b"image/png\0");
        picture.extend_from_slice(&[3, 0]);
        picture.extend_from_slice(art);
        body.extend(id3_frame(b"APIC", &picture));

        let mut file = b"ID3\x04\0\0".to_vec();
        file.extend_from_slice(&synchsafe(body.len()));
        file.extend(body);
        file
    }

    fn local_redirect_server() -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local HTTP fixture");
        let address = listener.local_addr().expect("read fixture address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept local request");
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            let response = b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/redirected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            stream.write_all(response).expect("write redirect fixture");
        });
        (format!("http://{address}/start"), server)
    }

    fn local_json_server(body: Vec<u8>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local JSON fixture");
        let address = listener.local_addr().expect("read fixture address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept local JSON request");
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .expect("write local JSON headers");
            stream.write_all(&body).expect("write local JSON body");
        });
        (format!("http://{address}/fixture"), server)
    }

    fn local_archive_redirect_server(image: Vec<u8>) -> (Url, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local archive fixture");
        let address = listener.local_addr().expect("read fixture address");
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for (status, location) in [(307, Some("/archive")), (302, Some("/image")), (200, None)]
            {
                let (mut stream, _) = listener.accept().expect("accept archive request");
                let mut request = [0u8; 2048];
                let length = stream.read(&mut request).expect("read archive request");
                requests.push(String::from_utf8_lossy(&request[..length]).into_owned());
                if let Some(location) = location {
                    write!(
                        stream,
                        "HTTP/1.1 {status} Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .expect("write redirect response");
                } else {
                    write!(
                        stream,
                        "HTTP/1.1 {status} OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        image.len()
                    )
                    .expect("write image response headers");
                    stream.write_all(&image).expect("write image response body");
                }
            }
            requests
        });
        (
            Url::parse(&format!("http://{address}/start")).expect("parse local fixture URL"),
            server,
        )
    }

    fn local_archive_redirect_to(location: &'static str) -> (Url, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local archive fixture");
        let address = listener.local_addr().expect("read fixture address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept archive request");
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            write!(
                stream,
                "HTTP/1.1 302 Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .expect("write archive redirect response");
        });
        (
            Url::parse(&format!("http://{address}/start")).expect("parse local fixture URL"),
            server,
        )
    }

    fn local_archive_redirect_server_waiting() -> (
        Url,
        mpsc::Receiver<()>,
        mpsc::Sender<()>,
        thread::JoinHandle<usize>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local archive fixture");
        let address = listener.local_addr().expect("read fixture address");
        let (received, first_request) = mpsc::channel();
        let (release_response, response_released) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept first archive request");
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            received.send(()).expect("notify first archive request");
            response_released
                .recv()
                .expect("wait to release archive response");
            stream
                .write_all(b"HTTP/1.1 302 Redirect\r\nLocation: /next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .expect("write archive redirect response");
            drop(stream);

            listener
                .set_nonblocking(true)
                .expect("make local listener nonblocking");
            let mut requests = 1;
            let deadline = Instant::now() + Duration::from_millis(250);
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut request = [0u8; 2048];
                        let _ = stream.read(&mut request);
                        requests += 1;
                        let _ = stream.write_all(
                            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
            requests
        });
        (
            Url::parse(&format!("http://{address}/start")).expect("parse local fixture URL"),
            first_request,
            release_response,
            server,
        )
    }

    fn is_local_archive_fixture_url(url: &Url) -> bool {
        url.scheme() == "http"
            && url.host_str() == Some("127.0.0.1")
            && url.port().is_some()
            && url.username().is_empty()
            && url.password().is_none()
    }

    fn local_http_response(body: Vec<u8>) -> (Url, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local search fixture");
        let address = listener.local_addr().expect("read fixture address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept local search request");
            let mut request = [0u8; 4096];
            let length = stream.read(&mut request).expect("read search request");
            let request = String::from_utf8_lossy(&request[..length]).into_owned();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .expect("write search response headers");
            stream.write_all(&body).expect("write search response body");
            request
        });
        (
            Url::parse(&format!("http://{address}/results")).expect("parse fixture URL"),
            server,
        )
    }

    fn local_http_response_waiting(
        body: Vec<u8>,
    ) -> (
        Url,
        mpsc::Receiver<()>,
        mpsc::Sender<()>,
        thread::JoinHandle<usize>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local search fixture");
        let address = listener.local_addr().expect("read fixture address");
        let (received, first_request) = mpsc::channel();
        let (release_response, response_released) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept search request");
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            received.send(()).expect("notify search request");
            response_released
                .recv()
                .expect("wait to release search response");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .expect("write search response headers");
            stream.write_all(&body).expect("write search response body");
            1
        });
        (
            Url::parse(&format!("http://{address}/results")).expect("parse fixture URL"),
            first_request,
            release_response,
            server,
        )
    }

    #[test]
    fn reads_real_id3_tags_and_embedded_art_without_changing_source() {
        let root = temporary_directory();
        let source = root.join("track.mp3");
        let embedded = png(200);
        let source_bytes = mp3_with_tags_and_art(&embedded);
        fs::write(&source, &source_bytes).expect("write ID3 fixture");

        let metadata = read_local(&source, &root.join("cache")).expect("read local metadata");

        assert_eq!(metadata.title.as_deref(), Some("夜の雨"));
        assert_eq!(metadata.artist.as_deref(), Some("Étoile"));
        assert_eq!(metadata.album_artist.as_deref(), Some("Various Artists"));
        assert_eq!(metadata.album.as_deref(), Some("Collection"));
        assert_eq!(metadata.genre.as_deref(), Some("Jazz"));
        assert_eq!(metadata.track_number, Some(7));
        assert_eq!(metadata.disc_number, Some(2));
        let artwork = metadata.artwork.expect("cache embedded art");
        let cached_image = image::ImageReader::open(artwork)
            .expect("open cached art")
            .decode()
            .expect("decode cached art")
            .to_rgba8();
        assert_eq!(cached_image.dimensions(), (2, 2));
        assert_eq!(cached_image.get_pixel(0, 0).0[0], 200);
        assert_eq!(
            fs::read(&source).expect("source remains readable"),
            source_bytes
        );
        fs::remove_dir_all(root).expect("remove isolated fixture");
    }

    #[test]
    fn uses_video_folder_poster_and_ignores_corrupt_candidate() {
        let root = temporary_directory();
        fs::write(root.join("film.mkv"), []).expect("create movie fixture");
        fs::write(root.join("poster.jpg"), b"corrupt").expect("create corrupt poster");
        let expected = png(100);
        fs::write(root.join("folder.png"), &expected).expect("create folder artwork");

        let metadata = read_local(&root.join("film.mkv"), &root.join("cache"))
            .expect("read local movie metadata");

        assert_eq!(metadata.title.as_deref(), Some("film"));
        let artwork = metadata.artwork.expect("folder artwork");
        let cached_image = image::ImageReader::open(artwork)
            .expect("open cached folder art")
            .decode()
            .expect("decode cached folder art")
            .to_rgba8();
        assert_eq!(cached_image.get_pixel(0, 0).0[0], 100);
        fs::remove_dir_all(root).expect("remove isolated fixture");
    }

    #[test]
    fn cached_artwork_longest_edge_is_capped_at_512_pixels() {
        let root = temporary_directory();
        let large = RgbaImage::from_pixel(1200, 600, Rgba([20, 30, 40, 255]));
        let mut encoded = std::io::Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(large)
            .write_to(&mut encoded, ImageFormat::Png)
            .expect("encode source art fixture");

        let cached = cache_artwork(encoded.get_ref(), &root).unwrap();
        let (width, height) = image::ImageReader::open(cached)
            .expect("open cached thumbnail")
            .into_dimensions()
            .expect("read cached thumbnail dimensions");

        assert_eq!(width, 512);
        assert!(height <= 512);
        fs::remove_dir_all(root).expect("remove isolated fixture");
    }

    #[test]
    fn rejects_oversized_corrupt_and_excessively_large_images() {
        assert!(validate_image(b"not an image").is_err());
        assert!(validate_image(&vec![0; MAX_ARTWORK_BYTES + 1]).is_err());
        let large = RgbaImage::from_pixel(3000, 3000, Rgba([1, 2, 3, 255]));
        let mut encoded = std::io::Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(large)
            .write_to(&mut encoded, ImageFormat::Png)
            .expect("encode large PNG fixture");
        assert!(validate_image(encoded.get_ref()).is_err());
    }

    #[test]
    fn explicit_youtube_id_and_urls_are_strictly_validated_and_cached() {
        assert_eq!(youtube_video_id("dQw4w9WgXcQ").unwrap(), "dQw4w9WgXcQ");
        assert_eq!(
            youtube_video_id("https://youtu.be/dQw4w9WgXcQ").unwrap(),
            "dQw4w9WgXcQ"
        );
        assert_eq!(
            youtube_video_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ").unwrap(),
            "dQw4w9WgXcQ"
        );
        assert!(youtube_video_id("http://youtu.be/dQw4w9WgXcQ").is_err());
        assert!(youtube_video_id("https://evil.example/watch?v=dQw4w9WgXcQ").is_err());
        assert!(youtube_video_id("https://www.youtube.com/watch?v=bad").is_err());

        let root = temporary_directory();
        let cached = cache_artwork(&png(123), &root).unwrap();
        let id = "dQw4w9WgXcQ";
        let key = super::cache_key(&("youtube", id));
        super::store_lookup_cache(&root, &key, Some(&cached), false).unwrap();
        assert_eq!(youtube_artwork(id, &root).unwrap(), cached);
        fs::remove_dir_all(root).expect("remove isolated fixture");
    }

    #[test]
    fn automatic_music_artwork_uses_metadata_only_and_respects_local_art() {
        let metadata = LocalMetadata {
            title: Some("Organ Donor".to_owned()),
            artist: Some("DJ Shadow".to_owned()),
            album: Some("Endtroducing".to_owned()),
            ..LocalMetadata::default()
        };
        let request = super::automatic_music_artwork_request(&metadata)
            .expect("tagged tracks without local art are eligible");
        assert_eq!(request.title, "Organ Donor");
        assert_eq!(request.artist.as_deref(), Some("DJ Shadow"));
        assert_eq!(request.album.as_deref(), Some("Endtroducing"));
        assert_eq!(request.kind, ArtworkKind::Music);
        assert!(super::automatic_music_artwork_request(&LocalMetadata {
            artwork: Some(PathBuf::from("cached.png")),
            ..metadata.clone()
        })
        .is_none());
        assert!(super::automatic_music_artwork_request(&LocalMetadata {
            title: Some("Unknown audio track".to_owned()),
            ..LocalMetadata::default()
        })
        .is_none());
    }

    #[test]
    fn youtube_music_search_uses_only_title_and_artist() {
        let request = ArtworkRequest {
            title: "001 - Organ Donor".to_owned(),
            artist: Some("DJ Shadow".to_owned()),
            album: Some("Private Album Name".to_owned()),
            kind: ArtworkKind::Music,
        };
        let url = super::youtube_search_url(&request).unwrap();
        assert!(super::is_youtube_search_url(&url));
        assert_eq!(
            url.query_pairs()
                .find(|(key, _)| key == "search_query")
                .unwrap()
                .1,
            "DJ Shadow Organ Donor"
        );
        assert!(!url.as_str().contains("Private Album Name"));
        assert!(!url.as_str().contains("C%3A%5C"));
    }

    #[test]
    fn yt_initial_data_parser_balances_json_strings_and_ranks_conservatively() {
        let page = br#"<script>var ytInitialData = {"note":"a } brace and \"quote\"","contents":[
            {"videoRenderer":{"videoId":"a1b2c3d4e5F","title":{"simpleText":"DJ Shadow - Organ Donor (Official Audio)"},"ownerText":{"simpleText":"DJ Shadow - Topic"}}},
            {"videoRenderer":{"videoId":"z9y8x7w6v5U","title":{"simpleText":"Other Song"},"ownerText":{"simpleText":"Other Artist"}}}
        ]};</script>"#;
        let data = super::youtube_initial_data(page).expect("parse bounded initial data");
        let request = ArtworkRequest {
            title: "Organ Donor".to_owned(),
            artist: Some("DJ Shadow".to_owned()),
            album: None,
            kind: ArtworkKind::Music,
        };
        assert_eq!(
            super::best_youtube_music_video(&data, &request).as_deref(),
            Some("a1b2c3d4e5F")
        );

        let ambiguous = serde_json::json!({
            "contents": [
                {"videoRenderer":{"videoId":"a1b2c3d4e5F","title":{"simpleText":"Organ Donor (Official Audio)"},"ownerText":{"simpleText":"DJ Shadow - Topic"}}},
                {"videoRenderer":{"videoId":"z9y8x7w6v5U","title":{"simpleText":"Organ Donor (Official Audio)"},"ownerText":{"simpleText":"DJ Shadow - Topic"}}}
            ]
        });
        assert!(super::best_youtube_music_video(&ambiguous, &request).is_none());
        let sequel = serde_json::json!({
            "contents": [
                {"videoRenderer":{"videoId":"a1b2c3d4e5F","title":{"simpleText":"Track 2"},"ownerText":{"simpleText":"Artist"}}}
            ]
        });
        assert!(super::best_youtube_music_video(
            &sequel,
            &ArtworkRequest {
                title: "Track 1".to_owned(),
                artist: Some("Artist".to_owned()),
                album: None,
                kind: ArtworkKind::Music
            }
        )
        .is_none());

        let pink_floyd = ArtworkRequest {
            title: "Time".to_owned(),
            artist: Some("Pink Floyd".to_owned()),
            album: None,
            kind: ArtworkKind::Music,
        };
        let cover = serde_json::json!({
            "contents": [{
                "videoRenderer": {
                    "videoId": "a1b2c3d4e5F",
                    "title": {"simpleText": "Pink Floyd - Time (Cover)"},
                    "ownerText": {"simpleText": "Pink Floyd"}
                }
            }]
        });
        assert!(super::best_youtube_music_video(&cover, &pink_floyd).is_none());

        let live = serde_json::json!({
            "contents": [{
                "videoRenderer": {
                    "videoId": "a1b2c3d4e5F",
                    "title": {"simpleText": "Pink Floyd - Time (Live)"},
                    "ownerText": {"simpleText": "Pink Floyd"}
                }
            }]
        });
        assert!(super::best_youtube_music_video(&live, &pink_floyd).is_none());

        let tom_petty = ArtworkRequest {
            title: "Free Fallin'".to_owned(),
            artist: Some("Tom Petty".to_owned()),
            album: None,
            kind: ArtworkKind::Music,
        };
        let partial_artist = serde_json::json!({
            "contents": [{
                "videoRenderer": {
                    "videoId": "a1b2c3d4e5F",
                    "title": {"simpleText": "Free Fallin'"},
                    "ownerText": {"simpleText": "Tom"}
                }
            }]
        });
        assert!(super::best_youtube_music_video(&partial_artist, &tom_petty).is_none());

        let original_artist = serde_json::json!({
            "contents": [{
                "videoRenderer": {
                    "videoId": "a1b2c3d4e5F",
                    "title": {"simpleText": "Tom Petty - Free Fallin' (Official Audio)"},
                    "ownerText": {"simpleText": "Tom Petty - Topic"}
                }
            }]
        });
        assert_eq!(
            super::best_youtube_music_video(&original_artist, &tom_petty).as_deref(),
            Some("a1b2c3d4e5F")
        );
    }

    #[test]
    fn youtube_search_parser_uses_bounded_local_http_fixture() {
        let body = br#"var ytInitialData = {"contents":[]};"#.to_vec();
        let (url, server) = local_http_response(body);
        assert!(!super::is_youtube_search_url(&url));
        let page = super::fetch_youtube_search_page(
            &http_client().unwrap(),
            url,
            is_local_archive_fixture_url,
            &AtomicBool::new(false),
        )
        .unwrap()
        .unwrap();
        assert!(super::youtube_initial_data(&page).is_some());
        assert!(server.join().unwrap().starts_with("GET /results "));
    }

    #[test]
    fn youtube_search_cancellation_stops_before_parsing_or_followup_requests() {
        let (url, first_request, release_response, server) =
            local_http_response_waiting(br#"var ytInitialData = {"contents":[]};"#.to_vec());
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_stopping = Arc::clone(&stopping);
        let request = thread::spawn(move || {
            super::fetch_youtube_search_page(
                &http_client().unwrap(),
                url,
                is_local_archive_fixture_url,
                &worker_stopping,
            )
        });
        first_request
            .recv_timeout(Duration::from_secs(2))
            .expect("search reaches the local fixture");
        stopping.store(true, Ordering::Relaxed);
        release_response.send(()).unwrap();
        assert_eq!(
            request.join().unwrap().unwrap_err(),
            "Artwork lookup cancelled."
        );
        assert_eq!(server.join().unwrap(), 1);
    }

    #[test]
    fn provider_matchers_reject_ambiguous_or_non_exact_results() {
        let tmdb = serde_json::json!({
            "results": [
                {"id": 1917, "title":"1917", "poster_path":"/1917.jpg"},
                {"id": 2020, "title":"1917 Again", "poster_path":"/other.jpg"}
            ]
        });
        assert_eq!(
            unique_tmdb_poster(&tmdb, "1917", None, ArtworkKind::Movie).as_deref(),
            Some("/1917.jpg")
        );
        let ambiguous = serde_json::json!({
            "results": [
                {"id": 1, "title":"Dune", "poster_path":"/one.jpg"},
                {"id": 2, "title":"Dune", "poster_path":"/two.jpg"}
            ]
        });
        assert!(unique_tmdb_poster(&ambiguous, "Dune", None, ArtworkKind::Movie).is_none());

        let ambiguous_missing_poster = serde_json::json!({
            "results": [
                {"id": 1, "title":"Dune", "poster_path":null},
                {"id": 2, "title":"Dune", "poster_path":"/one.jpg"}
            ]
        });
        assert!(
            unique_tmdb_poster(&ambiguous_missing_poster, "Dune", None, ArtworkKind::Movie)
                .is_none()
        );

        let same_candidate_id = serde_json::json!({
            "results": [
                {"id": 1, "title":"Dune", "poster_path":null},
                {"id": 1, "title":"Dune", "poster_path":"/one.jpg"}
            ]
        });
        assert_eq!(
            unique_tmdb_poster(&same_candidate_id, "Dune", None, ArtworkKind::Movie).as_deref(),
            Some("/one.jpg")
        );

        let music = serde_json::json!({
            "releases": [{
                "id":"76df3287-6cda-33eb-8e9a-044b5e15ffdd",
                "title":"Collection", "score":100,
                "artist-credit":[{"name":"Étoile", "joinphrase":""}]
            }]
        });
        assert_eq!(
            unique_musicbrainz_release(&music, "Collection", Some("Étoile")).as_deref(),
            Some("76df3287-6cda-33eb-8e9a-044b5e15ffdd")
        );
        assert!(is_uuid("76df3287-6cda-33eb-8e9a-044b5e15ffdd"));
        assert!(!is_uuid("../../etc/passwd"));
    }

    #[test]
    fn video_filenames_keep_clean_titles_years_numeric_titles_and_unicode() {
        assert_eq!(
            parse_artwork_title("Dune (2021)"),
            ("Dune".to_owned(), Some(2021))
        );
        assert_eq!(
            parse_artwork_title("Dune(2021)"),
            ("Dune".to_owned(), Some(2021))
        );
        assert_eq!(
            parse_artwork_title("Dune [2021]"),
            ("Dune".to_owned(), Some(2021))
        );
        assert_eq!(
            parse_artwork_title("Dune[2021].1080p"),
            ("Dune".to_owned(), Some(2021))
        );
        assert_eq!(
            parse_artwork_title("The.Matrix.1999.1080p.BluRay.x265"),
            ("The Matrix 1999".to_owned(), None)
        );
        assert_eq!(
            parse_artwork_title("Amélie.2001.1080p"),
            ("Amélie 2001".to_owned(), None)
        );
        for title in ["1917", "2001", "Blade Runner 2049"] {
            assert_eq!(parse_artwork_title(title), (title.to_owned(), None));
        }
        assert_eq!(
            parse_artwork_title("Class of 1984"),
            ("Class of 1984".to_owned(), None)
        );
        assert_eq!(
            parse_artwork_title("Class of 1984 (1982)"),
            ("Class of 1984".to_owned(), Some(1982))
        );
        assert_eq!(
            parse_artwork_title("Dracula2000"),
            ("Dracula2000".to_owned(), None)
        );
        assert_eq!(
            parse_artwork_title("Dracula.2000.1080p"),
            ("Dracula 2000".to_owned(), None)
        );
        assert_eq!(
            filename_title(&PathBuf::from("The.Matrix.1999.1080p.mkv"), false),
            "The Matrix 1999"
        );
        assert_eq!(
            split_trailing_bare_release_year("The Matrix 1999"),
            Some(("The Matrix".to_owned(), 1999))
        );
        assert_eq!(split_trailing_bare_release_year("Blade Runner 2049"), None);
        assert_eq!(
            normalized_match("The.Matrix"),
            normalized_match("The Matrix")
        );
        assert_eq!(normalized_match("Amélie"), normalized_match("AMÉLIE"));
    }

    #[test]
    fn tmdb_search_urls_apply_movie_and_tv_year_filters() {
        let movie = tmdb_search_url(ArtworkKind::Movie, "The Matrix", Some(1999)).unwrap();
        assert_eq!(movie.path(), "/3/search/movie");
        assert_eq!(
            movie.query_pairs().into_owned().collect::<Vec<_>>(),
            vec![
                ("query".to_owned(), "The Matrix".to_owned()),
                ("primary_release_year".to_owned(), "1999".to_owned()),
            ]
        );

        let series = tmdb_search_url(ArtworkKind::Series, "Dune", Some(2000)).unwrap();
        assert_eq!(series.path(), "/3/search/tv");
        assert_eq!(
            series.query_pairs().into_owned().collect::<Vec<_>>(),
            vec![
                ("query".to_owned(), "Dune".to_owned()),
                ("first_air_date_year".to_owned(), "2000".to_owned()),
            ]
        );
        assert!(tmdb_search_url(ArtworkKind::Music, "Album", None).is_err());
    }

    #[test]
    fn automatic_tmdb_matching_rejects_incomplete_and_malformed_searches() {
        let exact_match = serde_json::json!({
            "id": 603,
            "title": "The Matrix",
            "release_date": "1999-03-31"
        });
        let complete = serde_json::json!({
            "page": 1,
            "total_pages": 1,
            "total_results": 1,
            "results": [exact_match]
        });
        assert!(super::validate_tmdb_search_response(&complete).is_ok());
        assert_eq!(
            unique_tmdb_id(&complete, "The Matrix", Some(1999), ArtworkKind::Movie),
            Some(603)
        );

        let empty = serde_json::json!({
            "page": 1,
            "total_pages": 0,
            "total_results": 0,
            "results": []
        });
        assert!(super::validate_tmdb_search_response(&empty).is_ok());

        let paginated = serde_json::json!({
            "page": 1,
            "total_pages": 2,
            "total_results": 21,
            "results": [{
                "id": 603,
                "title": "The Matrix",
                "release_date": "1999-03-31"
            }]
        });
        assert_eq!(
            unique_tmdb_id(&paginated, "The Matrix", Some(1999), ArtworkKind::Movie),
            Some(603)
        );
        assert!(super::validate_tmdb_search_response(&paginated).is_err());

        for malformed in [
            serde_json::json!({}),
            serde_json::json!({"page": 1, "total_pages": 1, "results": null}),
            serde_json::json!({"page": 2, "total_pages": 2, "results": []}),
        ] {
            assert!(super::validate_tmdb_search_response(&malformed).is_err());
        }
    }

    #[test]
    fn tmdb_candidate_json_maps_movie_and_tv_genres_and_caps_results() {
        let genres = tmdb_genre_name_map(&serde_json::json!({
            "genres": [
                {"id": 28, "name": "Action"},
                {"id": 878, "name": "Science Fiction"}
            ]
        }))
        .unwrap();
        let movies = serde_json::json!({
            "results": [
                {"id": 603, "title": "The Matrix", "release_date": "1999-03-31", "poster_path": "/matrix.jpg", "genre_ids": [28, 878]},
                {"id": 603, "title": "Duplicate", "release_date": "2000-01-01", "poster_path": "/duplicate.jpg", "genre_ids": []},
                {"id": 604, "title": "No poster", "release_date": "", "poster_path": "https://evil.example/poster.jpg", "genre_ids": [999]},
                {"id": 0, "title": "Invalid id", "poster_path": "/invalid.jpg", "genre_ids": []}
            ]
        });
        let parsed = parse_tmdb_candidates(&movies, &genres, ArtworkKind::Movie).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].0.id, 603);
        assert_eq!(parsed[0].0.title, "The Matrix");
        assert_eq!(parsed[0].0.year, Some(1999));
        assert_eq!(parsed[0].0.genre, "Action; Science Fiction");
        assert_eq!(parsed[0].1.as_deref(), Some("/matrix.jpg"));
        assert_eq!(parsed[1].0.year, None);
        assert!(parsed[1].1.is_none());

        let tv = serde_json::json!({
            "results": [
                {"id": 1396, "name": "Breaking Bad", "first_air_date": "2008-01-20", "genre_ids": [18]}
            ]
        });
        let tv_genres = tmdb_genre_name_map(&serde_json::json!({
            "genres": [{"id": 18, "name": "Drama"}]
        }))
        .unwrap();
        let series = parse_tmdb_candidates(&tv, &tv_genres, ArtworkKind::Series).unwrap();
        assert_eq!(series[0].0.title, "Breaking Bad");
        assert_eq!(series[0].0.year, Some(2008));
        assert_eq!(series[0].0.genre, "Drama");
        assert!(parse_tmdb_candidates(&tv, &genres, ArtworkKind::Music).is_err());

        let many = serde_json::json!({
            "results": (1..=20).map(|id| serde_json::json!({"id": id, "title": format!("Movie {id}")})).collect::<Vec<_>>()
        });
        assert_eq!(
            parse_tmdb_candidates(&many, &genres, ArtworkKind::Movie)
                .unwrap()
                .len(),
            super::MAX_TMDB_CANDIDATES
        );
    }

    #[test]
    fn tmdb_candidates_parse_local_fake_json_without_credentials() {
        let body = serde_json::json!({
            "results": [
                {"id": 11, "title": "Local Fixture", "release_date": "2024-02-01", "genre_ids": [35], "poster_path": "/fixture.jpg"}
            ]
        })
        .to_string()
        .into_bytes();
        let (url, server) = local_json_server(body);
        let response = http_client().unwrap().get(url).send().unwrap();
        let fixture = json_response(response, "TMDB").unwrap();
        server.join().expect("finish local JSON fixture");
        let genres = tmdb_genre_name_map(&serde_json::json!({
            "genres": [{"id": 35, "name": "Comedy"}]
        }))
        .unwrap();
        let candidates = parse_tmdb_candidates(&fixture, &genres, ArtworkKind::Movie).unwrap();
        assert_eq!(candidates[0].0.id, 11);
        assert_eq!(candidates[0].0.genre, "Comedy");
    }

    #[test]
    fn tmdb_candidate_urls_are_type_specific_and_host_bound() {
        assert_eq!(
            tmdb_genres_url(ArtworkKind::Movie).unwrap().path(),
            "/3/genre/movie/list"
        );
        assert_eq!(
            tmdb_genres_url(ArtworkKind::Series).unwrap().path(),
            "/3/genre/tv/list"
        );
        assert_eq!(
            tmdb_detail_url(ArtworkKind::Movie, 603).unwrap().path(),
            "/3/movie/603"
        );
        assert_eq!(
            tmdb_detail_url(ArtworkKind::Series, 1396).unwrap().path(),
            "/3/tv/1396"
        );
        assert!(tmdb_detail_url(ArtworkKind::Movie, 0).is_err());
        assert!(tmdb_detail_url(ArtworkKind::Music, 603).is_err());
        assert_eq!(
            tmdb_detail_genres(&serde_json::json!({
                "genres": [{"id": 28, "name": "Action"}, {"id": 28, "name": "Action"}, {"id": 878, "name": "Science Fiction"}]
            })),
            "Action; Science Fiction"
        );

        for value in [
            "https://api.themoviedb.org:444/3/movie/1",
            "https://user:secret@api.themoviedb.org/3/movie/1",
            "http://api.themoviedb.org/3/movie/1",
            "https://api.themoviedb.org.example.com/3/movie/1",
        ] {
            assert!(!is_tmdb_api_url(&Url::parse(value).unwrap()));
        }
        for value in [
            "https://image.tmdb.org:444/t/p/w185/poster.jpg",
            "https://user:secret@image.tmdb.org/t/p/w185/poster.jpg",
            "http://image.tmdb.org/t/p/w185/poster.jpg",
            "https://image.tmdb.org.example.com/t/p/w185/poster.jpg",
        ] {
            assert!(!is_tmdb_image_url(&Url::parse(value).unwrap()));
        }

        let config = serde_json::json!({
            "images": {
                "secure_base_url":"https://image.tmdb.org/t/p/",
                "poster_sizes":["w185", "w500", "original"]
            }
        });
        assert_eq!(
            tmdb_image_url_for_size(&config, "/poster.jpg", Some("w185"))
                .unwrap()
                .as_str(),
            "https://image.tmdb.org/t/p/w185/poster.jpg"
        );
        let unsafe_config = serde_json::json!({
            "images": {
                "secure_base_url":"https://image.tmdb.org:444/t/p/",
                "poster_sizes":["w185", "w500"]
            }
        });
        assert!(tmdb_image_url_for_size(&unsafe_config, "/poster.jpg", Some("w185")).is_err());
    }

    #[test]
    fn automatic_tmdb_genres_require_one_exact_title_and_matching_year() {
        let remakes = serde_json::json!({
            "results": [
                {"id": 1, "title": "The Thing", "release_date": "1982-06-25"},
                {"id": 2, "title": "The Thing", "release_date": "2011-09-14"},
                {"id": 3, "title": "The Thing Returns", "release_date": "2020-01-01"}
            ]
        });
        assert_eq!(
            unique_tmdb_id(&remakes, "The Thing", Some(1982), ArtworkKind::Movie),
            Some(1)
        );
        assert_eq!(
            unique_tmdb_id(&remakes, "The Thing", None, ArtworkKind::Movie),
            None
        );
        assert_eq!(
            unique_tmdb_id(&remakes, "The Thing", Some(1990), ArtworkKind::Movie),
            None
        );

        let duplicate_id = serde_json::json!({
            "results": [
                {"id": 8, "title": "Dune", "release_date": "2021-10-22"},
                {"id": 8, "title": "Dune", "release_date": "2021-10-22"}
            ]
        });
        assert_eq!(
            unique_tmdb_id(&duplicate_id, "Dune", Some(2021), ArtworkKind::Movie),
            Some(8)
        );
        assert_eq!(
            unique_tmdb_id(&duplicate_id, "Dune", Some(2021), ArtworkKind::Series),
            None
        );

        let tv = serde_json::json!({
            "results": [
                {"id": 13, "name": "The Office", "first_air_date": "2005-03-24"},
                {"id": 14, "name": "The Office", "first_air_date": "2001-03-09"}
            ]
        });
        assert_eq!(
            unique_tmdb_id(&tv, "The Office", Some(2005), ArtworkKind::Series),
            Some(13)
        );
        assert_eq!(
            unique_tmdb_id(&tv, "The Office", Some(2005), ArtworkKind::Movie),
            None
        );
    }

    #[test]
    fn genre_cache_skips_missing_credentials_and_never_caches_errors() {
        let root = temporary_directory();
        let request = ArtworkRequest {
            title: "The Matrix (1999)".into(),
            artist: None,
            album: None,
            kind: ArtworkKind::Movie,
        };
        let key = request_cache_key("tmdb-genres", &request);
        assert_eq!(
            super::lookup_tmdb_genres_with_cancel(&request, &root, None, &AtomicBool::new(false))
                .unwrap(),
            None
        );
        assert!(read_tmdb_genre_cache(&root, &key).is_none());

        let invalid_token_result = super::lookup_tmdb_genres_with_cancel(
            &request,
            &root,
            Some("bad\r\ntoken"),
            &AtomicBool::new(false),
        );
        assert!(invalid_token_result.is_err());
        assert!(read_tmdb_genre_cache(&root, &key).is_none());

        assert_eq!(
            cache_tmdb_genre_result(&root, &key, Err("network failed".into())).unwrap_err(),
            "network failed"
        );
        assert!(read_tmdb_genre_cache(&root, &key).is_none());

        for response in [
            serde_json::json!({}),
            serde_json::json!({
                "page": 1,
                "total_pages": 2,
                "total_results": 21,
                "results": []
            }),
        ] {
            let invalid_search = super::validate_tmdb_search_response(&response).map(|()| None);
            assert!(cache_tmdb_genre_result(&root, &key, invalid_search).is_err());
            assert!(read_tmdb_genre_cache(&root, &key).is_none());
        }

        cache_tmdb_genre_result(&root, &key, Ok(Some("Action; Science Fiction".into()))).unwrap();
        assert_eq!(
            read_tmdb_genre_cache(&root, &key),
            Some(Some("Action; Science Fiction".into()))
        );
        store_tmdb_genre_cache(&root, &key, None).unwrap();
        assert_eq!(read_tmdb_genre_cache(&root, &key), Some(None));

        fs::write(
            root.join("tmdb-genres").join(format!("{key}.json")),
            br#"{"genre":null,"expires_at":0}"#,
        )
        .unwrap();
        assert!(read_tmdb_genre_cache(&root, &key).is_none());
        assert!(!root
            .join("tmdb-genres")
            .join(format!("{key}.json"))
            .exists());
        fs::remove_dir_all(root).expect("remove isolated genre cache fixture");
    }

    #[test]
    fn secondary_tmdb_identity_requires_no_exact_primary_title_candidate() {
        let no_exact_title = serde_json::json!({
            "results": [
                {"id": 1, "title": "The Matrix", "release_date": "1999-03-31", "poster_path": "/matrix.jpg"}
            ]
        });
        assert_eq!(
            secondary_tmdb_identity(&no_exact_title, "The Matrix 1999", None, ArtworkKind::Movie),
            Some(("The Matrix".to_owned(), 1999))
        );

        let numeric_title = serde_json::json!({
            "results": [
                {"id": 2, "title": "Class of 1984", "release_date": "1982-10-15", "poster_path": "/class-of-1984.jpg"}
            ]
        });
        assert_eq!(
            unique_tmdb_poster(&numeric_title, "Class of 1984", None, ArtworkKind::Movie)
                .as_deref(),
            Some("/class-of-1984.jpg")
        );
        assert_eq!(
            secondary_tmdb_identity(&numeric_title, "Class of 1984", None, ArtworkKind::Movie),
            None
        );

        let ambiguous_primary = serde_json::json!({
            "results": [
                {"id": 3, "title": "The Matrix 1999", "poster_path": "/one.jpg"},
                {"id": 4, "title": "The Matrix 1999", "poster_path": "/two.jpg"}
            ]
        });
        assert!(unique_tmdb_poster(
            &ambiguous_primary,
            "The Matrix 1999",
            None,
            ArtworkKind::Movie
        )
        .is_none());
        assert_eq!(
            secondary_tmdb_identity(
                &ambiguous_primary,
                "The Matrix 1999",
                None,
                ArtworkKind::Movie
            ),
            None
        );

        let missing_poster_primary = serde_json::json!({
            "results": [
                {"id": 5, "title": "The Matrix 1999", "release_date": "1999-03-31"}
            ]
        });
        assert!(unique_tmdb_poster(
            &missing_poster_primary,
            "The Matrix 1999",
            None,
            ArtworkKind::Movie
        )
        .is_none());
        assert_eq!(
            secondary_tmdb_identity(
                &missing_poster_primary,
                "The Matrix 1999",
                None,
                ArtworkKind::Movie
            ),
            None
        );
    }

    #[test]
    fn tmdb_candidate_years_disambiguate_remakes_and_tv_series() {
        let remakes = serde_json::json!({
            "results": [
                {"id": 1, "title": "The Thing", "release_date": "1982-06-25", "poster_path": "/1982.jpg"},
                {"id": 2, "title": "The Thing", "release_date": "2011-09-14", "poster_path": "/2011.jpg"}
            ]
        });
        assert_eq!(
            unique_tmdb_poster(&remakes, "The Thing", Some(1982), ArtworkKind::Movie).as_deref(),
            Some("/1982.jpg")
        );
        assert!(unique_tmdb_poster(&remakes, "The Thing", None, ArtworkKind::Movie).is_none());
        assert!(
            unique_tmdb_poster(&remakes, "The Thing", Some(1990), ArtworkKind::Movie).is_none()
        );

        let series = serde_json::json!({
            "results": [
                {"id": 1, "name": "The Office", "original_name": "The Office", "first_air_date": "2001-03-09", "poster_path": "/uk.jpg"},
                {"id": 2, "name": "The Office", "original_name": "The Office", "first_air_date": "2005-03-24", "poster_path": "/us.jpg"}
            ]
        });
        assert_eq!(
            unique_tmdb_poster(&series, "The.Office", Some(2005), ArtworkKind::Series).as_deref(),
            Some("/us.jpg")
        );
        assert!(unique_tmdb_poster(&series, "The Office", None, ArtworkKind::Series).is_none());
    }

    #[test]
    fn provider_cache_identity_tracks_clean_title_and_year_in_a_new_namespace() {
        let dotted = ArtworkRequest {
            title: "Dune.2021.1080p".to_owned(),
            artist: None,
            album: None,
            kind: ArtworkKind::Movie,
        };
        let clean = ArtworkRequest {
            title: "Dune 2021".to_owned(),
            ..dotted.clone()
        };
        let explicit_year = ArtworkRequest {
            title: "Dune (2021)".to_owned(),
            ..dotted.clone()
        };
        let other_year = ArtworkRequest {
            title: "Dune (1984)".to_owned(),
            ..dotted.clone()
        };

        assert_eq!(
            super::request_cache_key("provider", &dotted),
            super::request_cache_key("provider", &clean)
        );
        assert_ne!(
            super::request_cache_key("provider", &clean),
            super::request_cache_key("provider", &explicit_year)
        );
        assert_ne!(
            super::request_cache_key("provider", &explicit_year),
            super::request_cache_key("provider", &other_year)
        );
        assert_ne!(
            super::request_cache_key("provider", &clean),
            super::cache_key(&(
                "provider",
                &clean.title,
                &clean.artist,
                &clean.album,
                clean.kind
            ))
        );
    }

    #[test]
    fn tmdb_image_configuration_and_hosts_are_checked() {
        let config = serde_json::json!({
            "images": {
                "secure_base_url":"https://image.tmdb.org/t/p/",
                "poster_sizes":["w185", "w500", "original"]
            }
        });
        let url = tmdb_image_url(&config, "/poster.jpg").unwrap();
        assert_eq!(url.as_str(), "https://image.tmdb.org/t/p/w500/poster.jpg");
        assert!(tmdb_image_url(&config, "/../secret").is_err());
        let client = http_client().unwrap();
        let other_host = reqwest::Url::parse("https://example.com/api").unwrap();
        assert!(send_tmdb_json(
            &client,
            client.get(other_host),
            "test-token",
            &AtomicBool::new(false),
        )
        .is_err());
        let archive = reqwest::Url::parse("https://archive.org/image.jpg").unwrap();
        assert!(is_archive_image_url(&archive));
        let archive_cdn = reqwest::Url::parse("https://ia800.us.archive.org/image.jpg").unwrap();
        assert!(is_archive_image_url(&archive_cdn));
        let insecure = reqwest::Url::parse("http://archive.org/image.jpg").unwrap();
        assert!(!is_archive_image_url(&insecure));
        let deceptive = reqwest::Url::parse("https://archive.org.example.com/image.jpg").unwrap();
        assert!(!is_archive_image_url(&deceptive));
        let alternate_port = reqwest::Url::parse("https://archive.org:444/image.jpg").unwrap();
        assert!(!is_archive_image_url(&alternate_port));
        let user_info = reqwest::Url::parse("https://user:secret@archive.org/image.jpg").unwrap();
        assert!(!is_archive_image_url(&user_info));
    }

    #[test]
    fn provider_client_does_not_follow_redirects() {
        let (url, server) = local_redirect_server();
        let response = http_client()
            .unwrap()
            .get(url)
            .send()
            .expect("receive redirect without following it");
        server.join().expect("finish local HTTP fixture");
        assert_eq!(response.status(), StatusCode::FOUND);
    }

    #[test]
    fn cover_art_archive_follows_307_then_302_to_image_without_auth() {
        let expected_image = png(77);
        let (url, server) = local_archive_redirect_server(expected_image.clone());
        let fetched = super::follow_cover_art_redirects(
            &http_client().unwrap(),
            url,
            is_local_archive_fixture_url,
            false,
            &AtomicBool::new(false),
        )
        .expect("follow local archive redirects")
        .expect("receive archive image");
        let requests = server.join().expect("finish local archive fixture");

        assert_eq!(fetched, expected_image);
        assert_eq!(requests.len(), 3);
        assert!(requests
            .iter()
            .all(|request| !request.to_ascii_lowercase().contains("authorization:")));
        assert!(requests[0].starts_with("GET /start "));
        assert!(requests[1].starts_with("GET /archive "));
        assert!(requests[2].starts_with("GET /image "));
    }

    #[test]
    fn cover_art_archive_rejects_unapproved_redirect_hosts_before_requesting_them() {
        let (url, server) = local_archive_redirect_to("https://evil.example/image.png");

        let result = super::follow_cover_art_redirects(
            &http_client().unwrap(),
            url,
            is_local_archive_fixture_url,
            false,
            &AtomicBool::new(false),
        );

        server.join().expect("finish local archive fixture");
        assert!(result.is_err());
    }

    #[test]
    #[ignore = "requires network access to Cover Art Archive"]
    fn live_cover_art_archive_redirect_example_returns_an_image() {
        let bytes = super::fetch_cover_art_archive(
            &http_client().unwrap(),
            "76df3287-6cda-33eb-8e9a-044b5e15ffdd",
            &AtomicBool::new(false),
        )
        .expect("download Cover Art Archive example")
        .expect("example release has front artwork");
        assert!(validate_image(&bytes).is_ok());
    }

    #[test]
    fn cached_artwork_is_available_without_network_or_tmdb_token() {
        let root = temporary_directory();
        let artwork = cache_artwork(&png(99), &root).unwrap();
        let request = ArtworkRequest {
            title: "Local Film".to_owned(),
            artist: None,
            album: None,
            kind: ArtworkKind::Movie,
        };
        let key = super::request_cache_key("provider", &request);
        super::store_lookup_cache(&root, &key, Some(&artwork), false).unwrap();
        let marker = root.join("requests").join(format!("{key}.json"));
        let expired_record = serde_json::json!({
            "expires_at": 1,
            "artwork": artwork.file_name().unwrap().to_string_lossy(),
        });
        fs::write(&marker, serde_json::to_vec(&expired_record).unwrap()).unwrap();

        assert_eq!(
            lookup_artwork(&request, &root, None).unwrap(),
            Some(artwork)
        );
        let _default = LocalMetadata::default();
        fs::remove_dir_all(root).expect("remove isolated fixture");
    }

    #[test]
    fn cancellation_during_archive_request_stops_before_the_next_redirect() {
        let (url, first_request, release_response, server) =
            local_archive_redirect_server_waiting();
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_stopping = Arc::clone(&stopping);
        let request = thread::spawn(move || {
            super::follow_cover_art_redirects(
                &http_client().unwrap(),
                url,
                is_local_archive_fixture_url,
                false,
                &worker_stopping,
            )
        });

        first_request
            .recv_timeout(Duration::from_secs(2))
            .expect("first archive request reaches local server");
        let started = Instant::now();
        stopping.store(true, Ordering::Relaxed);
        release_response
            .send(())
            .expect("release in-flight archive request");
        let result = request.join().expect("finish cancelled lookup");

        assert_eq!(result.unwrap_err(), "Artwork lookup cancelled.");
        assert_eq!(server.join().expect("finish local archive fixture"), 1);
        assert!(started.elapsed() < super::HTTP_TIMEOUT + Duration::from_secs(1));
    }

    #[test]
    fn cancelled_artwork_lookup_does_not_write_a_negative_cache_entry() {
        let root = temporary_directory();
        let request = ArtworkRequest {
            title: "Cancelled Album".to_owned(),
            artist: Some("Test Artist".to_owned()),
            album: Some("Cancelled Album".to_owned()),
            kind: ArtworkKind::Music,
        };
        let stopping = AtomicBool::new(true);
        let result = super::lookup_artwork_with_cancel(&request, &root, None, &stopping);
        let key = super::request_cache_key("provider", &request);

        assert_eq!(result.unwrap_err(), "Artwork lookup cancelled.");
        assert!(!root.join("requests").join(format!("{key}.json")).exists());
        fs::remove_dir_all(root).expect("remove isolated fixture");
    }

    #[test]
    fn request_queries_escape_provider_syntax() {
        assert_eq!(super::lucene_phrase("A \"quote\""), "\"A \\\"quote\\\"\"");
    }
}
