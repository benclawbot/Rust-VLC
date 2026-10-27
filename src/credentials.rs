use keyring::{Entry, Error};

const SERVICE: &str = "com.benclawbot.rust-vlc";
const USER: &str = "tmdb-read-access-token";
const OPENSUBTITLES_USER: &str = "opensubtitles-api-credentials";

#[derive(Clone, Eq, PartialEq)]
pub struct OpenSubtitlesCredentials {
    pub api_key: String,
    pub bearer_token: Option<String>,
}

pub fn has_tmdb_token() -> Result<bool, String> {
    Ok(tmdb_token()?.is_some())
}

pub fn tmdb_token() -> Result<Option<String>, String> {
    let entry = match Entry::new(SERVICE, USER) {
        Ok(entry) => entry,
        Err(_) => return environment_token().map(Some).map_err(|_| store_error()),
    };
    match entry.get_password() {
        Ok(token) => normalized_token(&token)
            .map(str::to_owned)
            .map(Some)
            .ok_or_else(|| "The saved TMDB token is invalid.".to_owned()),
        Err(Error::NoEntry) => environment_token().map(Some).or(Ok(None)),
        Err(_) => environment_token().map(Some).map_err(|_| store_error()),
    }
}

pub fn save_tmdb_token(token: &str) -> Result<(), String> {
    let token = normalized_token(token)
        .ok_or_else(|| "Enter a valid TMDB read access token.".to_owned())?;
    Entry::new(SERVICE, USER)
        .and_then(|entry| entry.set_password(token))
        .map_err(|_| store_error())
}

pub fn clear_tmdb_token() -> Result<(), String> {
    match Entry::new(SERVICE, USER).and_then(|entry| entry.delete_credential()) {
        Ok(()) | Err(Error::NoEntry) => Ok(()),
        Err(_) => Err(store_error()),
    }
}

pub fn has_opensubtitles_credentials() -> Result<bool, String> {
    opensubtitles_credentials().map(|credentials| credentials.is_some())
}

pub fn opensubtitles_credentials() -> Result<Option<OpenSubtitlesCredentials>, String> {
    let entry = Entry::new(SERVICE, OPENSUBTITLES_USER).map_err(|_| store_error())?;
    let stored = match entry.get_password() {
        Ok(stored) => stored,
        Err(Error::NoEntry) => return Ok(None),
        Err(_) => return Err(store_error()),
    };
    if stored.len() > 16_384 {
        return Err("The saved OpenSubtitles credentials are invalid.".into());
    }
    let (api_key, bearer_token): (String, Option<String>) = serde_json::from_str(&stored)
        .map_err(|_| "The saved OpenSubtitles credentials are invalid.".to_owned())?;
    let api_key = normalized_secret(&api_key)
        .ok_or_else(|| "The saved OpenSubtitles credentials are invalid.".to_owned())?;
    let bearer_token = match bearer_token.as_deref() {
        Some(token) => Some(
            normalized_secret(token)
                .ok_or_else(|| "The saved OpenSubtitles credentials are invalid.".to_owned())?,
        ),
        None => None,
    };
    Ok(Some(OpenSubtitlesCredentials {
        api_key: api_key.to_owned(),
        bearer_token: bearer_token.map(str::to_owned),
    }))
}

pub fn save_opensubtitles_credentials(
    api_key: &str,
    bearer_token: Option<&str>,
) -> Result<(), String> {
    let api_key = normalized_secret(api_key)
        .ok_or_else(|| "Enter a valid OpenSubtitles API key.".to_owned())?;
    let bearer_token = bearer_token
        .filter(|token| !token.trim().is_empty())
        .map(|token| {
            normalized_secret(token).ok_or_else(|| {
                "Enter a valid OpenSubtitles bearer token or leave it blank.".to_owned()
            })
        })
        .transpose()?;
    let credentials = OpenSubtitlesCredentials {
        api_key: api_key.to_owned(),
        bearer_token: bearer_token.map(str::to_owned),
    };
    let serialized = serde_json::to_string(&(&credentials.api_key, &credentials.bearer_token))
        .map_err(|_| {
            "OpenSubtitles credentials could not be prepared for secure storage.".to_owned()
        })?;
    Entry::new(SERVICE, OPENSUBTITLES_USER)
        .and_then(|entry| entry.set_password(&serialized))
        .map_err(|_| store_error())
}

pub fn clear_opensubtitles_credentials() -> Result<(), String> {
    match Entry::new(SERVICE, OPENSUBTITLES_USER).and_then(|entry| entry.delete_credential()) {
        Ok(()) | Err(Error::NoEntry) => Ok(()),
        Err(_) => Err(store_error()),
    }
}

fn environment_token() -> Result<String, ()> {
    std::env::var("TMDB_READ_TOKEN")
        .ok()
        .and_then(|token| normalized_token(&token).map(str::to_owned))
        .ok_or(())
}

fn normalized_token(token: &str) -> Option<&str> {
    let token = token.trim();
    (!token.is_empty() && token.len() <= 8192 && !token.contains(['\r', '\n'])).then_some(token)
}

fn normalized_secret(secret: &str) -> Option<&str> {
    let secret = secret.trim();
    (!secret.is_empty() && secret.len() <= 8192 && !secret.chars().any(char::is_control))
        .then_some(secret)
}

fn store_error() -> String {
    #[cfg(target_os = "linux")]
    let message = "The Linux Secret Service is unavailable. Start a desktop keyring service such as GNOME Keyring or KWallet.";
    #[cfg(target_os = "macos")]
    let message = "macOS Keychain is unavailable.";
    #[cfg(target_os = "windows")]
    let message = "Windows Credential Manager is unavailable.";
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    let message = "The system credential store is unavailable on this platform.";
    message.to_owned()
}

#[cfg(test)]
mod tests {
    use super::{normalized_secret, normalized_token};

    #[test]
    fn tmdb_tokens_are_trimmed_and_bounded_without_logging_the_value() {
        assert_eq!(normalized_token("  token  "), Some("token"));
        assert_eq!(normalized_token(" \t "), None);
        assert_eq!(normalized_token(&"x".repeat(8193)), None);
        assert_eq!(normalized_token("token\r\nother"), None);
    }

    #[test]
    fn opensubtitles_secrets_are_trimmed_and_bounded_without_newlines() {
        assert_eq!(normalized_secret("  key  "), Some("key"));
        assert_eq!(normalized_secret("  "), None);
        assert_eq!(normalized_secret(&"x".repeat(8193)), None);
        assert_eq!(normalized_secret("secret\nsecond"), None);
        assert_eq!(normalized_secret("secret\ttab"), None);
    }
}
