#!/usr/bin/env python3
"""Seed and verify a synthetic Rust-VLC v4 library database for installer CI."""

from __future__ import annotations

import hashlib
import os
import sqlite3
import sys
from pathlib import Path


def native_path(path: Path) -> tuple[bytes, str]:
    if os.name == "nt":
        return str(path).encode("utf-16le"), "windows"
    return os.fsencode(path), "unix"


def digest(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as database:
        for block in iter(lambda: database.read(1024 * 1024), b""):
            hasher.update(block)
    return hasher.hexdigest()


def create(path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        raise SystemExit(f"Refusing to overwrite existing library database: {path}")

    movie_path, encoding = native_path(path.parent / "retention-fixture" / "sample-movie.mp4")
    track_path, track_encoding = native_path(path.parent / "retention-fixture" / "sample-track.flac")
    if encoding != track_encoding:
        raise SystemExit("Synthetic native paths use inconsistent encodings")

    connection = sqlite3.connect(path)
    try:
        connection.execute("PRAGMA foreign_keys=ON")
        connection.executescript(
            """
            CREATE TABLE media (
                id INTEGER PRIMARY KEY,
                kind TEXT NOT NULL CHECK(kind IN ('audio','video')),
                path BLOB NOT NULL,
                encoding TEXT NOT NULL,
                title TEXT NOT NULL,
                format TEXT NOT NULL,
                UNIQUE(kind,encoding,path)
            );
            CREATE TABLE playlists (id INTEGER PRIMARY KEY, name TEXT NOT NULL COLLATE NOCASE UNIQUE);
            CREATE TABLE playlist_items (
                playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
                position INTEGER NOT NULL,
                media_id INTEGER NOT NULL REFERENCES media(id),
                PRIMARY KEY(playlist_id,position)
            );
            CREATE TABLE progress (
                media_id INTEGER PRIMARY KEY REFERENCES media(id),
                position_ms INTEGER NOT NULL,
                duration_ms INTEGER NOT NULL,
                completed INTEGER NOT NULL
            );
            CREATE TABLE details (
                media_id INTEGER PRIMARY KEY REFERENCES media(id),
                artist TEXT NOT NULL,
                album TEXT NOT NULL,
                album_artist TEXT NOT NULL,
                genre TEXT NOT NULL,
                artwork BLOB,
                art_encoding TEXT,
                art_override INTEGER NOT NULL,
                disc_number INTEGER,
                track_number INTEGER
            );
            CREATE TABLE preferences (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE watched_roots (
                position INTEGER PRIMARY KEY,
                path BLOB NOT NULL,
                encoding TEXT NOT NULL,
                UNIQUE(encoding,path)
            );
            CREATE TABLE bookmarks (
                id INTEGER PRIMARY KEY,
                media_id INTEGER NOT NULL REFERENCES media(id),
                name TEXT NOT NULL,
                position_ms INTEGER NOT NULL CHECK(position_ms>=0)
            );
            CREATE INDEX bookmarks_media_idx ON bookmarks(media_id);
            CREATE TABLE network_history (
                position INTEGER PRIMARY KEY,
                url TEXT NOT NULL UNIQUE,
                title TEXT NOT NULL,
                last_played_at INTEGER NOT NULL CHECK(last_played_at>=0)
            );
            PRAGMA user_version=4;
            """
        )
        connection.executemany(
            "INSERT INTO media(id,kind,path,encoding,title,format) VALUES(?,?,?,?,?,?)",
            (
                (11, "video", movie_path, encoding, "Retention Test Movie", "mp4"),
                (12, "audio", track_path, encoding, "Retention Test Track", "flac"),
            ),
        )
        connection.execute("INSERT INTO playlists(id,name) VALUES(7,'Retention Test Playlist')")
        connection.executemany(
            "INSERT INTO playlist_items(playlist_id,position,media_id) VALUES(?,?,?)",
            ((7, 0, 12), (7, 1, 12)),
        )
        connection.execute("INSERT INTO progress VALUES(11,45000,120000,0)")
        connection.execute(
            "INSERT INTO details VALUES(12,'Retention Artist','Retention Album','Retention Artist','Synthetic',NULL,NULL,0,1,1)"
        )
        connection.executemany(
            "INSERT INTO preferences(key,value) VALUES(?,?)",
            (
                ("view", "music"),
                ("selected_playlist", "7"),
                ("volume", "63"),
                ("rate", "1.25"),
                ("saved_movie_ids", "[11]"),
            ),
        )
        connection.execute("INSERT INTO bookmarks VALUES(9,11,'Retention bookmark',45000)")
        connection.execute(
            "INSERT INTO network_history VALUES(0,'https://example.invalid/retention','Retained network entry',1700000000)"
        )
        connection.commit()
    finally:
        connection.close()


def verify(path: Path, expected_digest: str) -> None:
    actual_digest = digest(path)
    if actual_digest != expected_digest:
        raise SystemExit(f"Library database hash changed: expected {expected_digest}, got {actual_digest}")

    uri = path.resolve().as_uri() + "?mode=ro"
    connection = sqlite3.connect(uri, uri=True)
    try:
        if connection.execute("PRAGMA integrity_check").fetchone() != ("ok",):
            raise SystemExit("Library database failed SQLite integrity_check")
        if connection.execute("PRAGMA user_version").fetchone() != (4,):
            raise SystemExit("Library database schema version changed")

        movie_path, encoding = native_path(path.parent / "retention-fixture" / "sample-movie.mp4")
        track_path, track_encoding = native_path(path.parent / "retention-fixture" / "sample-track.flac")
        if encoding != track_encoding:
            raise SystemExit("Synthetic native paths use inconsistent encodings")
        expected = {
            "media": [
                (11, "video", movie_path, encoding, "Retention Test Movie", "mp4"),
                (12, "audio", track_path, encoding, "Retention Test Track", "flac"),
            ],
            "playlist": [(7, "Retention Test Playlist")],
            "playlist_items": [(7, 0, 12), (7, 1, 12)],
            "progress": [(11, 45000, 120000, 0)],
            "details": [(12, "Retention Artist", "Retention Album", "Retention Artist", "Synthetic", None, None, 0, 1, 1)],
            "preferences": [
                ("rate", "1.25"),
                ("saved_movie_ids", "[11]"),
                ("selected_playlist", "7"),
                ("view", "music"),
                ("volume", "63"),
            ],
            "bookmarks": [(9, 11, "Retention bookmark", 45000)],
            "network_history": [(0, "https://example.invalid/retention", "Retained network entry", 1700000000)],
        }
        queries = {
            "media": "SELECT id,kind,path,encoding,title,format FROM media ORDER BY id",
            "playlist": "SELECT id,name FROM playlists ORDER BY id",
            "playlist_items": "SELECT playlist_id,position,media_id FROM playlist_items ORDER BY playlist_id,position",
            "progress": "SELECT media_id,position_ms,duration_ms,completed FROM progress ORDER BY media_id",
            "details": "SELECT media_id,artist,album,album_artist,genre,artwork,art_encoding,art_override,disc_number,track_number FROM details ORDER BY media_id",
            "preferences": "SELECT key,value FROM preferences ORDER BY key",
            "bookmarks": "SELECT id,media_id,name,position_ms FROM bookmarks ORDER BY id",
            "network_history": "SELECT position,url,title,last_played_at FROM network_history ORDER BY position",
        }
        for table, query in queries.items():
            rows = connection.execute(query).fetchall()
            if rows != expected[table]:
                raise SystemExit(f"Library database {table} contents changed: {rows!r}")
    finally:
        connection.close()

    print(actual_digest)


def main() -> None:
    if len(sys.argv) not in (3, 4):
        raise SystemExit("usage: validate-install-database.py create|verify PATH [EXPECTED_SHA256]")
    operation, raw_path = sys.argv[1:3]
    path = Path(raw_path)
    if operation == "create" and len(sys.argv) == 3:
        create(path)
        verify(path, digest(path))
    elif operation == "verify" and len(sys.argv) == 4:
        verify(path, sys.argv[3])
    else:
        raise SystemExit("usage: validate-install-database.py create|verify PATH [EXPECTED_SHA256]")


if __name__ == "__main__":
    main()
