use keyring::{Entry, Error};

const SERVICE: &str = "com.benclawbot.rust-vlc";
const USER: &str = "tmdb-read-access-token";

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
    use super::normalized_token;

    #[test]
    fn tmdb_tokens_are_trimmed_and_bounded_without_logging_the_value() {
        assert_eq!(normalized_token("  token  "), Some("token"));
        assert_eq!(normalized_token(" \t "), None);
        assert_eq!(normalized_token(&"x".repeat(8193)), None);
        assert_eq!(normalized_token("token\r\nother"), None);
    }
}
