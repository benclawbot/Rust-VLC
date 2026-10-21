use std::path::Path;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EpisodeInfo {
    pub series: String,
    pub season: u32,
    pub episode: u32,
}

pub fn episode_info(path: &Path) -> Option<EpisodeInfo> {
    let stem = path.file_stem()?.to_str()?;
    let bytes = stem.as_bytes();
    for start in 0..bytes.len() {
        if start > 0 && bytes[start - 1].is_ascii_alphanumeric() {
            continue;
        }
        let (season_start, separator) = if bytes[start].eq_ignore_ascii_case(&b's') {
            (start + 1, b'e')
        } else if bytes[start].is_ascii_digit() {
            (start, b'x')
        } else {
            continue;
        };
        let season_end = digit_end(bytes, season_start);
        if season_end == season_start
            || season_end - season_start > 2
            || bytes
                .get(season_end)
                .is_none_or(|byte| !byte.eq_ignore_ascii_case(&separator))
        {
            continue;
        }
        let episode_start = season_end + 1;
        let end = digit_end(bytes, episode_start);
        if end == episode_start
            || end - episode_start > 3
            || bytes.get(end).is_some_and(u8::is_ascii_alphanumeric)
            || (bytes.get(end) == Some(&b'-') && bytes.get(end + 1).is_some_and(u8::is_ascii_digit))
        {
            continue;
        }
        let series = stem[..start]
            .trim_matches(|character: char| {
                character.is_whitespace() || matches!(character, '.' | '_' | '-')
            })
            .replace(['.', '_'], " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if series.is_empty() {
            continue;
        }
        return Some(EpisodeInfo {
            series,
            season: stem[season_start..season_end].parse().ok()?,
            episode: stem[episode_start..end].parse().ok()?,
        });
    }
    None
}

fn digit_end(bytes: &[u8], start: usize) -> usize {
    let mut end = start;
    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_seasons_specials_and_unicode_without_inventing_movie_episodes() {
        let episode = episode_info(Path::new("Série_du_soir.S00E02.1080p.mkv")).unwrap();
        assert_eq!(
            episode,
            EpisodeInfo {
                series: "Série du soir".into(),
                season: 0,
                episode: 2
            }
        );
        assert_eq!(
            episode_info(Path::new("Show 1x10.mkv")).unwrap().episode,
            10
        );
        for filename in [
            "1917 (2019).mp4",
            "2001 A Space Odyssey.mkv",
            "Show S01E02E03.mkv",
            "Show S01E02-03.mkv",
            "Season01E02.mkv",
            "Show 1080x1920.mkv",
        ] {
            assert!(episode_info(Path::new(filename)).is_none(), "{filename}");
        }
    }
}
