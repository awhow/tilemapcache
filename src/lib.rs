use std::path::Path;
use std::{collections::HashMap, fs};

use chrono::{Duration, NaiveDateTime, Utc};
use reqwest::Client;
use rusqlite::{Connection, params};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TileMapError {
    #[error("database error")]
    Database(#[from] rusqlite::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("invalid TileMapBase create_time")]
    InvalidCreateTime(#[from] chrono::ParseError),

    #[error("HTTP request failed")]
    Request(#[from] reqwest::Error),

    #[error("map server returned HTTP {status} for tile {zoom}/{x}/{y}")]
    HttpStatus {
        status: reqwest::StatusCode,
        zoom: u8,
        x: u32,
        y: u32,
    },

    #[error("map server returned an empty tile")]
    EmptyTile,

    #[error("invalid tile cache size")]
    InvalidSize(#[from] std::num::TryFromIntError),

    #[error(transparent)]
    UrlFormat(#[from] strfmt::FmtError),
}

/// Defines a tile map source used to retrieve and cache map tiles.
#[derive(Debug)]
pub struct TileMapSource {
    /// Name used to identify this tile source in the `TileMapBase` cache.
    pub name: String,

    /// URL template used to request tiles.
    ///
    /// The template must contain `{x}`, `{y}`, and `{zoom}` placeholders,
    /// which are replaced with the corresponding tile coordinates.
    pub url_template: String,

    /// Width and height of each raster tile in pixels.
    pub tile_size: u32,
}

/// A SQLite-backed cache for map tiles.
///
/// Tiles are stored using the TileMapBase-compatible cache format, with
/// each tile identified by its source name and tile coordinates.
pub struct TileMapCache {
    pub db: Connection,
    pub source: TileMapSource,
    pub ttl: Duration,
}

/// Metadata describing a cached map tile.
#[derive(Debug)]
pub struct TileInfo {
    /// Name of the tile source.
    pub source: String,

    /// Tile X coordinate.
    pub x: u32,

    /// Tile Y coordinate.
    pub y: u32,

    /// Tile zoom level.
    pub zoom: u8,

    /// Size of the cached tile data in bytes.
    pub size: usize,

    /// Time at which the tile was added to the cache.
    pub create_time: String,
}

impl TileMapCache {
    /// Open or create database
    pub fn open(
        path: impl AsRef<Path>,
        source: TileMapSource,
        ttl: Duration,
    ) -> Result<Self, TileMapError> {
        let path = path.as_ref();

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let db = Connection::open(path)?;

        db.execute(
            r"
            CREATE TABLE IF NOT EXISTS cache (
            request STRING UNIQUE,
            data BLOB,
            create_time STRING
            )
            ",
            [],
        )?;

        Ok(Self { db, source, ttl })
    }

    pub async fn fetch_tile(
        &self,
        client: &Client,
        x: u32,
        y: u32,
        zoom: u8,
    ) -> Result<Vec<u8>, TileMapError> {
        if let Some(data) = self.get(x, y, zoom)? {
            return Ok(data);
        }

        let response = client.get(&self.url(x, y, zoom)?).send().await?;

        let status = response.status();
        if !status.is_success() {
            return Err(TileMapError::HttpStatus { status, zoom, x, y });
        }

        let data = response.bytes().await?.to_vec();
        if data.is_empty() {
            return Err(TileMapError::EmptyTile);
        }

        self.put(x, y, zoom, &data)?;

        Ok(data)
    }

    /// Get image from cache database
    pub fn get(&self, x: u32, y: u32, zoom: u8) -> Result<Option<Vec<u8>>, TileMapError> {
        let request = self.key(x, y, zoom);

        let mut stmt = self
            .db
            .prepare("SELECT data, create_time FROM cache WHERE request = ?1")?;

        let mut rows = stmt.query(params![request])?;

        let Some(row) = rows.next()? else {
            return Ok(None);
        };

        let data: Vec<u8> = row.get(0)?;
        let create_time: String = row.get(1)?;

        let created = NaiveDateTime::parse_from_str(&create_time, "%Y-%m-%dT%H:%M:%S")?.and_utc();

        let age = Utc::now().signed_duration_since(created);

        if age > self.ttl {
            // println!("CACHE EXPIRED {request}");
            return Ok(None);
        }

        Ok(Some(data))
    }

    /// List all tiles in cache database
    pub fn list(&self) -> Result<Vec<TileInfo>, TileMapError> {
        let mut stmt = self.db.prepare(
            r"
            SELECT request, length(data), create_time
            FROM cache
            WHERE request LIKE ?1
            ORDER BY request
            ",
        )?;

        let mut rows = stmt.query(params![format!("{}#%", self.source.name)])?;

        let mut tiles = Vec::new();

        while let Some(row) = rows.next()? {
            tiles.push(parse_tile_info(row)?);
        }

        Ok(tiles)
    }

    /// Key for cache database
    fn key(&self, x: u32, y: u32, zoom: u8) -> String {
        format!("{}#{x}#{y}#{zoom}", self.source.name)
    }

    /// Insert image into cache database
    fn put(&self, x: u32, y: u32, zoom: u8, data: &[u8]) -> Result<(), TileMapError> {
        let request = self.key(x, y, zoom);
        let create_time = Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string();

        self.db.execute(
            r"
            INSERT OR REPLACE INTO cache (request, data, create_time)
            VALUES (?1, ?2, ?3)
            ",
            params![request, data, create_time],
        )?;

        Ok(())
    }

    // Build URL for tile
    fn url(&self, x: u32, y: u32, zoom: u8) -> Result<String, TileMapError> {
        let mut vars = HashMap::new();

        vars.insert("x".to_owned(), x.to_string());
        vars.insert("y".to_owned(), y.to_string());
        vars.insert("zoom".to_owned(), zoom.to_string());

        Ok(strfmt::strfmt(&self.source.url_template, &vars)?)
    }
}

fn parse_tile_info(row: &rusqlite::Row<'_>) -> Result<TileInfo, TileMapError> {
    let request: String = row.get(0)?;
    let size: i64 = row.get(1)?;
    let create_time: String = row.get(2)?;

    let mut parts = request.split('#');

    let source = parts.next().unwrap_or_default().to_string();

    let x: u32 = parts.next().unwrap_or_default().parse().unwrap_or_default();

    let y: u32 = parts.next().unwrap_or_default().parse().unwrap_or_default();

    let zoom: u8 = parts.next().unwrap_or_default().parse().unwrap_or_default();

    Ok(TileInfo {
        source,
        x,
        y,
        zoom,
        size: usize::try_from(size)?,
        create_time,
    })
}
