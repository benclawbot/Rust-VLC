use crate::credentials::{self, OpenSubtitlesCredentials};
use reqwest::blocking::{Client, Response};
use reqwest::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE, LOCATION, USER_AGENT};
use reqwest::redirect::Policy;
use reqwest::{StatusCode, Url};
use serde_json::Value;
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const API_ROOT: &str = "https://api.opensubtitles.com/api/v1/";
const MAX_SEARCH_BYTES: u64 = 2 * 1024 * 1024;
const MAX_DOWNLOAD_RESPONSE_BYTES: u64 = 128 * 1024;
const MAX_SUBTITLE_BYTES: usize = 10 * 1024 * 1024;
const MAX_RESULTS: usize = 100;
const MAX_REDIRECTS: usize = 3;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubtitleSearchResult {
    pub file_id: i32,
    pub language: String,
    pub release: String,
    pub file_name: String,
    pub download_count: Option<u64>,
    pub hearing_impaired: bool,
}

/// Search OpenSubtitles by a title or file name and language codes. The function deliberately
/// accepts no media path, movie URL, moviehash, or external file handle.
pub fn search_subtitles(
    query: &str,
    languages: &[String],
) -> Result<Vec<SubtitleSearchResult>, String> {
    search_subtitles_inner(query, languages, None)
}

pub(crate) fn search_subtitles_with_cancel(
    query: &str,
    languages: &[String],
    stopping: &AtomicBool,
) -> Result<Vec<SubtitleSearchResult>, String> {
    search_subtitles_inner(query, languages, Some(stopping))
}

fn search_subtitles_inner(
    query: &str,
    languages: &[String],
    stopping: Option<&AtomicBool>,
) -> Result<Vec<SubtitleSearchResult>, String> {
    ensure_not_cancelled(stopping)?;
    let credentials = configured_credentials()?;
    let client = client()?;
    let base = Url::parse(API_ROOT).map_err(|_| "OpenSubtitles API configuration is invalid.")?;
    search_at(&client, &base, &credentials, query, languages, stopping)
}

/// Download a selected SRT into the application's cache. The media directory is never written.
pub fn download_subtitle(file_id: i32, cache_dir: &Path) -> Result<PathBuf, String> {
    download_subtitle_inner(file_id, cache_dir, None)
}

pub(crate) fn download_subtitle_with_cancel(
    file_id: i32,
    cache_dir: &Path,
    stopping: &AtomicBool,
) -> Result<PathBuf, String> {
    download_subtitle_inner(file_id, cache_dir, Some(stopping))
}

fn download_subtitle_inner(
    file_id: i32,
    cache_dir: &Path,
    stopping: Option<&AtomicBool>,
) -> Result<PathBuf, String> {
    ensure_not_cancelled(stopping)?;
    if file_id <= 0 {
        return Err("Select a valid subtitle result.".into());
    }
    let credentials = configured_credentials()?;
    let bearer_token = credentials.bearer_token.as_deref().ok_or_else(|| {
        "Add an OpenSubtitles bearer token in Settings to download subtitles.".to_owned()
    })?;
    let client = client()?;
    let base = Url::parse(API_ROOT).map_err(|_| "OpenSubtitles API configuration is invalid.")?;
    download_at(
        &client,
        &base,
        &credentials,
        bearer_token,
        file_id,
        cache_dir,
        stopping,
    )
}

fn configured_credentials() -> Result<OpenSubtitlesCredentials, String> {
    credentials::opensubtitles_credentials()?
        .ok_or_else(|| "Add an OpenSubtitles API key in Settings to search for subtitles.".into())
}

fn client() -> Result<Client, String> {
    Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(Policy::none())
        .build()
        .map_err(|_| "The OpenSubtitles network client could not be started.".into())
}

fn search_at(
    client: &Client,
    base: &Url,
    credentials: &OpenSubtitlesCredentials,
    query: &str,
    languages: &[String],
    stopping: Option<&AtomicBool>,
) -> Result<Vec<SubtitleSearchResult>, String> {
    let query = search_title(query)?;
    let languages = normalize_languages(languages)?;
    let language_query = languages.join(",");
    let mut params = vec![("query", query.to_lowercase())];
    if !language_query.is_empty() {
        params.push(("languages", language_query));
    }
    params.sort_by(|left, right| left.0.cmp(right.0));

    let api_key = secret_header(&credentials.api_key, "Api-Key")?;
    let mut endpoint = base
        .join("subtitles")
        .map_err(|_| "OpenSubtitles API configuration is invalid.")?;
    let mut response = None;
    for redirect_count in 0..=MAX_REDIRECTS {
        ensure_not_cancelled(stopping)?;
        let result = client
            .get(endpoint.clone())
            .query(&params)
            .header("Api-Key", api_key.clone())
            .header(USER_AGENT, user_agent())
            .send()
            .map_err(|_| "OpenSubtitles search request failed.")?;
        if is_redirect(result.status()) {
            if redirect_count == MAX_REDIRECTS {
                return Err("OpenSubtitles redirected too many times.".into());
            }
            endpoint = same_origin_redirect(&result, &endpoint)?;
            continue;
        }
        response = Some(result);
        break;
    }
    let response = response.ok_or_else(|| "OpenSubtitles search request failed.".to_owned())?;
    ensure_success(response.status(), "OpenSubtitles search failed")?;
    let body = read_bounded(response, MAX_SEARCH_BYTES)?;
    let value: Value = serde_json::from_slice(&body)
        .map_err(|_| "OpenSubtitles returned invalid search data.".to_owned())?;
    parse_search_results(&value)
}

fn download_at(
    client: &Client,
    base: &Url,
    credentials: &OpenSubtitlesCredentials,
    bearer_token: &str,
    file_id: i32,
    cache_dir: &Path,
    stopping: Option<&AtomicBool>,
) -> Result<PathBuf, String> {
    ensure_not_cancelled(stopping)?;
    if file_id <= 0 {
        return Err("Select a valid subtitle result.".into());
    }
    let api_key = secret_header(&credentials.api_key, "Api-Key")?;
    let authorization = HeaderValue::from_str(&format!("Bearer {bearer_token}"))
        .map_err(|_| "The saved OpenSubtitles credentials are invalid.".to_owned())?;
    let endpoint = base
        .join("download")
        .map_err(|_| "OpenSubtitles API configuration is invalid.")?;
    let response = client
        .post(endpoint)
        .header("Api-Key", api_key)
        .header(AUTHORIZATION, authorization)
        .header(USER_AGENT, user_agent())
        .header(CONTENT_TYPE, "application/json")
        .body(
            serde_json::to_vec(&serde_json::json!({
                "file_id": file_id,
                "sub_format": "srt"
            }))
            .map_err(|_| "The subtitle request could not be prepared.".to_owned())?,
        )
        .send()
        .map_err(|_| "OpenSubtitles subtitle request failed.")?;
    if is_redirect(response.status()) {
        return Err("OpenSubtitles redirected the subtitle request; it was not followed.".into());
    }
    ensure_success(response.status(), "OpenSubtitles subtitle request failed")?;
    let body = read_bounded(response, MAX_DOWNLOAD_RESPONSE_BYTES)?;
    ensure_not_cancelled(stopping)?;
    let value: Value = serde_json::from_slice(&body)
        .map_err(|_| "OpenSubtitles returned invalid download data.".to_owned())?;
    let link = value
        .get("link")
        .and_then(Value::as_str)
        .ok_or_else(|| "OpenSubtitles did not provide a subtitle download link.".to_owned())?;
    let download_url = Url::parse(link)
        .map_err(|_| "OpenSubtitles provided an invalid subtitle download link.".to_owned())?;
    if !valid_download_url(&download_url, base) {
        return Err("OpenSubtitles provided a download link outside its verified host.".into());
    }
    ensure_not_cancelled(stopping)?;
    download_srt(client, download_url, cache_dir, file_id, stopping)
}

fn search_title(query: &str) -> Result<String, String> {
    let query = query.trim();
    let leaf = if let Ok(url) = Url::parse(query) {
        if matches!(url.scheme(), "http" | "https") {
            url.path_segments()
                .and_then(|mut segments| segments.next_back())
                .filter(|segment| !segment.is_empty())
                .unwrap_or_else(|| url.host_str().unwrap_or_default())
                .to_owned()
        } else {
            query
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or_default()
                .to_owned()
        }
    } else {
        query
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or_default()
            .to_owned()
    };
    let leaf = leaf.split(['?', '#']).next().unwrap_or_default().trim();
    if leaf.is_empty() || leaf.len() > 512 || leaf.chars().any(char::is_control) {
        return Err("Enter a title or file name of at most 512 bytes.".into());
    }
    Ok(leaf.to_owned())
}

fn normalize_languages(languages: &[String]) -> Result<Vec<String>, String> {
    if languages.len() > 10 {
        return Err("Choose at most 10 subtitle languages.".into());
    }
    let mut normalized = Vec::with_capacity(languages.len());
    for language in languages {
        let raw = language.trim();
        if raw.len() > 5 {
            return Err("Subtitle language codes must contain 2 to 5 letters.".into());
        }
        let language = raw.to_ascii_lowercase();
        if language.is_empty() {
            continue;
        }
        if !(2..=5).contains(&language.len())
            || !language
                .bytes()
                .all(|byte| byte.is_ascii_alphabetic() || byte == b'-')
        {
            return Err("Subtitle language codes must contain 2 to 5 letters.".into());
        }
        if !normalized.contains(&language) {
            normalized.push(language);
        }
    }
    normalized.sort();
    Ok(normalized)
}

fn parse_search_results(value: &Value) -> Result<Vec<SubtitleSearchResult>, String> {
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| "OpenSubtitles returned invalid search data.".to_owned())?;
    let mut results = Vec::new();
    let mut seen = HashSet::new();
    for item in data {
        let Some(attributes) = item.get("attributes") else {
            continue;
        };
        let language = safe_text(attributes.get("language").and_then(Value::as_str), 16);
        let release = safe_text(attributes.get("release").and_then(Value::as_str), 256);
        let download_count = attributes.get("download_count").and_then(Value::as_u64);
        let hearing_impaired = attributes
            .get("hearing_impaired")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let Some(files) = attributes.get("files").and_then(Value::as_array) else {
            continue;
        };
        for file in files {
            let Some(file_id) = file
                .get("file_id")
                .and_then(Value::as_i64)
                .and_then(|id| i32::try_from(id).ok())
                .filter(|id| *id > 0)
            else {
                continue;
            };
            if !seen.insert(file_id) {
                continue;
            }
            let file_name = safe_leaf(file.get("file_name").and_then(Value::as_str), 256);
            if file_name.is_empty() {
                continue;
            }
            results.push(SubtitleSearchResult {
                file_id,
                language: language.clone(),
                release: release.clone(),
                file_name,
                download_count,
                hearing_impaired,
            });
            if results.len() == MAX_RESULTS {
                return Ok(results);
            }
        }
    }
    Ok(results)
}

fn safe_text(value: Option<&str>, limit: usize) -> String {
    let value = value.unwrap_or_default();
    let mut result = String::with_capacity(value.len().min(limit));
    for character in value.chars() {
        if !character.is_control() && result.len() + character.len_utf8() <= limit {
            result.push(character);
        }
    }
    result
}

fn safe_leaf(value: Option<&str>, limit: usize) -> String {
    let value = value
        .unwrap_or_default()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default();
    safe_text(Some(value), limit)
}

fn secret_header(value: &str, name: &str) -> Result<HeaderValue, String> {
    HeaderValue::from_str(value).map_err(|_| format!("The saved OpenSubtitles {name} is invalid."))
}

fn user_agent() -> HeaderValue {
    HeaderValue::from_static(concat!(
        env!("CARGO_PKG_NAME"),
        "/",
        env!("CARGO_PKG_VERSION")
    ))
}

fn is_redirect(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::MOVED_PERMANENTLY
            | StatusCode::FOUND
            | StatusCode::SEE_OTHER
            | StatusCode::TEMPORARY_REDIRECT
            | StatusCode::PERMANENT_REDIRECT
    )
}

fn same_origin_redirect(response: &Response, current: &Url) -> Result<Url, String> {
    let location = response
        .headers()
        .get(LOCATION)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| "OpenSubtitles returned an invalid redirect.".to_owned())?;
    let next = current
        .join(location)
        .map_err(|_| "OpenSubtitles returned an invalid redirect.".to_owned())?;
    if !same_origin(current, &next) {
        return Err("OpenSubtitles redirected to an unverified host.".into());
    }
    Ok(next)
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn valid_download_url(url: &Url, api_base: &Url) -> bool {
    #[cfg(test)]
    if api_base.host_str().is_some_and(is_loopback_host)
        && same_origin(api_base, url)
        && url.path().starts_with("/download/")
        && url.username().is_empty()
        && url.password().is_none()
    {
        return true;
    }
    if url.scheme() != "https"
        || url.username() != ""
        || url.password().is_some()
        || url.port().is_some_and(|port| port != 443)
        || !url.path().starts_with("/download/")
    {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    if host == "opensubtitles.com" || host.ends_with(".opensubtitles.com") {
        return true;
    }
    let _ = api_base;
    false
}

#[cfg(test)]
fn is_loopback_host(host: &str) -> bool {
    host == "localhost" || host == "127.0.0.1" || host == "::1"
}

fn download_srt(
    client: &Client,
    mut url: Url,
    cache_dir: &Path,
    file_id: i32,
    stopping: Option<&AtomicBool>,
) -> Result<PathBuf, String> {
    let response = get_same_origin(client, &mut url, stopping)?;
    ensure_success(response.status(), "Subtitle download failed")?;
    let bytes = read_bounded(response, MAX_SUBTITLE_BYTES as u64)?;
    ensure_not_cancelled(stopping)?;
    validate_srt(&bytes)?;

    ensure_not_cancelled(stopping)?;
    let directory = cache_dir.join("subtitles");
    fs::create_dir_all(&directory)
        .map_err(|_| "The subtitle cache could not be created.".to_owned())?;
    let destination = directory.join(format!("opensubtitles-{file_id}.srt"));
    let temporary = directory.join(format!(
        ".opensubtitles-{file_id}-{}.part",
        fastrand::u64(..)
    ));
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|_| "The subtitle cache file could not be created.")?;
        file.write_all(&bytes)
            .map_err(|_| "The subtitle cache file could not be written.")?;
        drop(file);
        ensure_not_cancelled(stopping)?;
        if destination.exists() {
            fs::remove_file(&destination)
                .map_err(|_| "The previous cached subtitle could not be replaced.")?;
        }
        fs::rename(&temporary, &destination)
            .map_err(|_| "The downloaded subtitle could not be saved to the cache.")?;
        Ok(destination.clone())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

fn get_same_origin(
    client: &Client,
    url: &mut Url,
    stopping: Option<&AtomicBool>,
) -> Result<Response, String> {
    for redirect_count in 0..=MAX_REDIRECTS {
        ensure_not_cancelled(stopping)?;
        let response = client
            .get(url.clone())
            .header(USER_AGENT, user_agent())
            .send()
            .map_err(|_| "Subtitle download failed.".to_owned())?;
        if is_redirect(response.status()) {
            if redirect_count == MAX_REDIRECTS {
                return Err("Subtitle download redirected too many times.".into());
            }
            *url = same_origin_redirect(&response, url)?;
            continue;
        }
        return Ok(response);
    }
    Err("Subtitle download failed.".into())
}

fn ensure_not_cancelled(stopping: Option<&AtomicBool>) -> Result<(), String> {
    if stopping.is_some_and(|stopping| stopping.load(Ordering::Relaxed)) {
        Err("Subtitle request was canceled.".into())
    } else {
        Ok(())
    }
}

fn validate_srt(bytes: &[u8]) -> Result<(), String> {
    if bytes.is_empty() || bytes.len() > MAX_SUBTITLE_BYTES {
        return Err("The subtitle file is empty or exceeds the 10 MiB limit.".into());
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| "The subtitle file is not valid UTF-8 text.".to_owned())?;
    if text.contains('\0')
        || !text.lines().any(|line| {
            line.split_once("-->")
                .is_some_and(|(start, end)| valid_srt_timestamp(start) && valid_srt_timestamp(end))
        })
    {
        return Err("The downloaded file is not a valid SRT text subtitle.".into());
    }
    if text.chars().any(|character| {
        character.is_control() && !matches!(character, '\r' | '\n' | '\t' | '\u{feff}')
    }) {
        return Err("The downloaded file contains invalid subtitle text.".into());
    }
    Ok(())
}

fn valid_srt_timestamp(value: &str) -> bool {
    let value = value.trim();
    let Some(separator) = value.find([',', '.']) else {
        return false;
    };
    let (clock, fraction) = value.split_at(separator);
    let fraction = &fraction[1..];
    if fraction.len() != 3 || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let parts = clock.split(':').collect::<Vec<_>>();
    if !(2..=3).contains(&parts.len()) {
        return false;
    }
    if !parts.iter().all(|part| {
        !part.is_empty() && part.len() <= 2 && part.bytes().all(|byte| byte.is_ascii_digit())
    }) {
        return false;
    }
    let minute = parts[parts.len() - 2].parse::<u8>().ok();
    let second = parts[parts.len() - 1].parse::<u8>().ok();
    minute.is_some_and(|value| value < 60) && second.is_some_and(|value| value < 60)
}

fn ensure_success(status: StatusCode, context: &str) -> Result<(), String> {
    if status.is_success() {
        Ok(())
    } else {
        Err(format!("{context} (HTTP {}).", status.as_u16()))
    }
}

fn read_bounded(mut response: Response, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "The OpenSubtitles response could not be read.".to_owned())?;
    if bytes.len() as u64 > limit {
        return Err("The OpenSubtitles response exceeded the size limit.".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread::{self, JoinHandle};

    type TestResponse = (u16, Vec<(String, String)>, Vec<u8>);

    fn credentials() -> OpenSubtitlesCredentials {
        OpenSubtitlesCredentials {
            api_key: "fixture-api-key".into(),
            bearer_token: Some("fixture-bearer".into()),
        }
    }

    fn fixture_server(responses: Vec<TestResponse>) -> (Url, JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let thread = thread::spawn(move || {
            let mut requests = Vec::new();
            for (status, headers, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    let count = stream.read(&mut chunk).unwrap();
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..count]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        let header_end = request
                            .windows(4)
                            .position(|window| window == b"\r\n\r\n")
                            .unwrap()
                            + 4;
                        let request_text =
                            String::from_utf8_lossy(&request[..header_end]).into_owned();
                        let content_length = request_text
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .and_then(|value| value.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        while request.len() < header_end + content_length {
                            let count = stream.read(&mut chunk).unwrap();
                            if count == 0 {
                                break;
                            }
                            request.extend_from_slice(&chunk[..count]);
                        }
                        break;
                    }
                }
                requests.push(String::from_utf8_lossy(&request).into_owned());
                let reason = if status < 400 { "OK" } else { "Error" };
                let mut response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
                    body.len()
                );
                for (name, value) in headers {
                    response.push_str(&format!("{name}: {value}\r\n"));
                }
                response.push_str("\r\n");
                stream.write_all(response.as_bytes()).unwrap();
                stream.write_all(&body).unwrap();
            }
            requests
        });
        (
            Url::parse(&format!("http://{address}/api/v1/")).unwrap(),
            thread,
        )
    }

    fn test_client() -> Client {
        Client::builder()
            .timeout(Duration::from_secs(3))
            .redirect(Policy::none())
            .build()
            .unwrap()
    }

    #[test]
    fn search_query_never_sends_a_local_parent_path_or_signed_url_query() {
        assert_eq!(
            search_title("C:\\Users\\Alice\\Movie.MKV").unwrap(),
            "Movie.MKV"
        );
        assert_eq!(
            search_title("https://stream.example.test/live?token=private#part").unwrap(),
            "live"
        );
        assert_eq!(
            normalize_languages(&["FR".into(), "en".into(), "en".into()]).unwrap(),
            ["en", "fr"]
        );
        assert!(normalize_languages(&["language-too-long".into()]).is_err());
    }

    fn cache_directory() -> PathBuf {
        std::env::temp_dir().join(format!(
            "rust-vlc-subtitles-{}-{}",
            std::process::id(),
            fastrand::u64(..)
        ))
    }

    #[test]
    fn search_sends_only_a_sanitized_title_and_sorted_languages_with_api_key() {
        let body = br#"{"data":[{"attributes":{"language":"en","release":"Movie.Release","download_count":4,"hearing_impaired":false,"files":[{"file_id":12,"file_name":"Movie.en.srt"}]}}]}"#;
        let (base, server) = fixture_server(vec![(200, vec![], body.to_vec())]);
        let languages = vec!["FR".to_owned(), "en".to_owned(), "en".to_owned()];
        let results = search_at(
            &test_client(),
            &base,
            &credentials(),
            "C:\\Users\\Alice\\Movie.MKV",
            &languages,
            None,
        )
        .unwrap();
        assert_eq!(results[0].file_id, 12);
        let requests = server.join().unwrap();
        let request = requests.first().unwrap().to_ascii_lowercase();
        assert!(
            request.contains("/api/v1/subtitles?languages=en%2cfr&query=movie.mkv"),
            "request was {request}"
        );
        assert!(request.contains("api-key: fixture-api-key"));
        assert!(!request.contains("alice"));
        assert!(!request.contains("authorization:"));
    }

    #[test]
    fn subtitle_download_authenticates_only_api_request_and_caches_verified_utf8_srt() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let thread = thread::spawn(move || {
            let mut requests = Vec::new();
            for response_body in [
                format!(r#"{{"link":"http://{address}/download/file"}}"#).into_bytes(),
                b"1\n00:00:01,000 --> 00:00:02,000\nHello\n".to_vec(),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut chunk = [0u8; 2048];
                loop {
                    let count = stream.read(&mut chunk).unwrap();
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..count]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        let header_end = request
                            .windows(4)
                            .position(|window| window == b"\r\n\r\n")
                            .unwrap()
                            + 4;
                        let header_text =
                            String::from_utf8_lossy(&request[..header_end]).into_owned();
                        let length = header_text
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .and_then(|value| value.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        while request.len() < header_end + length {
                            let count = stream.read(&mut chunk).unwrap();
                            if count == 0 {
                                break;
                            }
                            request.extend_from_slice(&chunk[..count]);
                        }
                        break;
                    }
                }
                requests.push(String::from_utf8_lossy(&request).into_owned());
                let content_type = if requests.len() == 1 {
                    "application/json"
                } else {
                    "text/plain; charset=utf-8"
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response_body.len()
                );
                stream.write_all(response.as_bytes()).unwrap();
                stream.write_all(&response_body).unwrap();
            }
            requests
        });
        let base = Url::parse(&format!("http://{address}/api/v1/")).unwrap();
        let directory = cache_directory();
        let path = download_at(
            &test_client(),
            &base,
            &credentials(),
            "fixture-bearer",
            4,
            &directory,
            None,
        )
        .unwrap();
        assert_eq!(path, directory.join("subtitles/opensubtitles-4.srt"));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "1\n00:00:01,000 --> 00:00:02,000\nHello\n"
        );
        let requests = thread.join().unwrap();
        let api_request = requests[0].to_ascii_lowercase();
        assert!(api_request.contains("api-key: fixture-api-key"));
        assert!(api_request.contains("authorization: bearer fixture-bearer"));
        let file_request = requests[1].to_ascii_lowercase();
        assert!(!file_request.contains("api-key:"));
        assert!(!file_request.contains("authorization:"));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn download_links_and_redirects_are_host_checked_and_payloads_are_bounded() {
        let api = Url::parse(API_ROOT).unwrap();
        assert!(valid_download_url(
            &Url::parse("https://www.opensubtitles.com/download/abc").unwrap(),
            &api
        ));
        assert!(!valid_download_url(
            &Url::parse("https://www.opensubtitles.com.attacker.test/download/abc").unwrap(),
            &api
        ));
        assert!(!valid_download_url(
            &Url::parse("https://user@www.opensubtitles.com/download/abc").unwrap(),
            &api
        ));
        assert!(validate_srt(b"1\n00:00:01,000 --> 00:00:02,000\nLine\n").is_ok());
        assert!(validate_srt(b"<html>Not subtitles</html>").is_err());
        assert!(validate_srt(&vec![b'x'; MAX_SUBTITLE_BYTES + 1]).is_err());

        let (base, server) = fixture_server(vec![(
            302,
            vec![("Location".into(), "http://attacker.test/leak".into())],
            Vec::new(),
        )]);
        let error =
            search_at(&test_client(), &base, &credentials(), "title", &[], None).unwrap_err();
        assert!(error.contains("unverified host"));
        let requests = server.join().unwrap();
        assert_eq!(
            requests.len(),
            1,
            "the redirected host must never receive credentials"
        );
    }

    #[test]
    fn cancellation_after_api_response_prevents_the_download_request() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stopping = std::sync::Arc::new(AtomicBool::new(false));
        let server_stopping = std::sync::Arc::clone(&stopping);
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 2048];
            loop {
                let count = stream.read(&mut chunk).unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..count]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let header_end = request
                        .windows(4)
                        .position(|window| window == b"\r\n\r\n")
                        .unwrap()
                        + 4;
                    let header_text = String::from_utf8_lossy(&request[..header_end]).into_owned();
                    let content_length = header_text
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    while request.len() < header_end + content_length {
                        let count = stream.read(&mut chunk).unwrap();
                        if count == 0 {
                            break;
                        }
                        request.extend_from_slice(&chunk[..count]);
                    }
                    break;
                }
            }
            let body = format!(r#"{{"link":"http://{address}/download/file"}}"#);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            server_stopping.store(true, Ordering::Relaxed);
            stream.write_all(response.as_bytes()).unwrap();
            stream.write_all(body.as_bytes()).unwrap();
            String::from_utf8_lossy(&request).into_owned()
        });
        let base = Url::parse(&format!("http://{address}/api/v1/")).unwrap();
        let cache = cache_directory();
        let error = download_at(
            &test_client(),
            &base,
            &credentials(),
            "fixture-bearer",
            51,
            &cache,
            Some(&stopping),
        )
        .unwrap_err();
        assert_eq!(error, "Subtitle request was canceled.");
        let request = server.join().unwrap().to_ascii_lowercase();
        assert!(request.contains("api-key: fixture-api-key"));
        assert!(request.contains("authorization: bearer fixture-bearer"));
        assert!(!cache.exists(), "canceled work must not write a cache file");
    }
}
