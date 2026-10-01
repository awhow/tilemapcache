use std::path::PathBuf;

use anyhow::Context;
use chrono::Duration;
use clap::{Args, Parser, Subcommand};
use directories::ProjectDirs;
use reqwest::Client;

use tilemapcache::{TileMapCache, TileMapSource};

const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

fn default_cache_path() -> PathBuf {
    ProjectDirs::from("com", "example", "tile-map-cache").map_or_else(
        || PathBuf::from("tilemapbase.db"),
        |dirs| dirs.cache_dir().join("tilemapbase.db"),
    )
}

#[derive(Debug, Parser)]
#[command(name = "tile-map-cache")]
#[command(about = "Manage a TileMapBase-compatible tile cache")]
struct Cli {
    /// Path to the `TileMapBase` SQLite cache.
    #[arg(long, default_value_os_t = default_cache_path())]
    cache: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Fetch a tile from CARTO, using the cache when available.
    Fetch(FetchArgs),

    /// List cached tiles.
    List,

    /// Show information about a cached tile.
    Show(ShowArgs),

    /// Show cache db path
    Path,
}

#[derive(Debug, Args)]
struct FetchArgs {
    /// Tile X coordinate.
    #[arg(short = 'x', long = "x")]
    x: u32,

    /// Tile Y coordinate.
    #[arg(short = 'y', long = "y")]
    y: u32,

    /// Tile zoom level.
    #[arg(short = 'z', long = "zoom")]
    z: u8,

    /// Optional path to write the tile PNG.
    #[arg(short, long)]
    output: Option<String>,
}

#[derive(Debug, Args)]
struct ShowArgs {
    /// Tile X coordinate.
    #[arg(short, long = "x")]
    x: u32,

    /// Tile Y coordinate.
    #[arg(short, long = "y")]
    y: u32,

    /// Tile zoom level.
    #[arg(short, long = "zoom")]
    z: u8,
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let cli = Cli::parse();

    let source = TileMapSource {
        name: "CARTO Light".into(),
        url_template: "https://basemaps.cartocdn.com/rastertiles/light_all/{zoom}/{x}/{y}.png"
            .into(),
        tile_size: 256,
    };

    let cache = TileMapCache::open(cli.cache, source, Duration::days(30))?;

    match cli.command {
        Command::Fetch(args) => fetch(&cache, args).await,
        Command::List => list(&cache),
        Command::Show(args) => show(&cache, &args).await,
        Command::Path => {
            path(&cache);
            Ok(())
        }
    }
}

async fn fetch(cache: &TileMapCache, args: FetchArgs) -> Result<(), anyhow::Error> {
    let client = Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .context("creating HTTP client")?;

    let data = cache.fetch_tile(&client, args.x, args.y, args.z).await?;

    if let Some(output) = args.output {
        std::fs::write(&output, &data).with_context(|| format!("writing {output}"))?;

        println!("WROTE {output}");
    }

    Ok(())
}

#[allow(clippy::cast_precision_loss, clippy::as_conversions)]
fn list(cache: &TileMapCache) -> Result<(), anyhow::Error> {
    let tiles = cache.list()?;

    for tile in tiles {
        println!(
            "{} x={:4} y={:4} zoom={:2} size={:5.1} kb, date={}",
            tile.source,
            tile.x,
            tile.y,
            tile.zoom,
            tile.size as f32 / 1_000.0,
            tile.create_time
        );
    }

    Ok(())
}

async fn show(cache: &TileMapCache, args: &ShowArgs) -> Result<(), anyhow::Error> {
    let data = if let Some(data) = cache.get(args.x, args.y, args.z)? {
        data
    } else {
        fetch(
            cache,
            FetchArgs {
                x: args.x,
                y: args.y,
                z: args.z,
                output: None,
            },
        )
        .await?;

        cache.get(args.x, args.y, args.z)?.with_context(|| {
            format!(
                "tile not found after fetch: {}/{}/{}",
                args.z, args.x, args.y
            )
        })?
    };

    let path =
        std::env::temp_dir().join(format!("tilemapcache-{}-{}-{}.png", args.z, args.x, args.y));

    std::fs::write(&path, &data).with_context(|| format!("writing {}", path.display()))?;

    open::that(&path).with_context(|| format!("opening {}", path.display()))?;

    Ok(())
}

fn path(cache: &TileMapCache) {
    if let Some(path) = cache.db.path() {
        println!("{path}");
    }
}
