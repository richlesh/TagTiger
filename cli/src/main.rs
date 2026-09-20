//! Thin CLI frontend over `tagtiger-core`.
//!
//! Subcommands:
//!   tagtiger search  <file>            Parse filename, search TMDB, list matches.
//!   tagtiger inspect <file>            Print existing tags in a file.
//!   tagtiger tag     <file> --id <id>  Fetch details for a TMDB id and write tags.
//!
//! Requires a TMDB credential for network commands: set `TMDB_BEARER_TOKEN`
//! (a v4 read access token, preferred) or `TMDB_API_KEY` (a v3 API key).

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tagtiger_core::{
    artwork,
    model::{MediaKind, MediaKindMeta, ProviderId},
    naming, tag, MetadataProvider, TmdbProvider,
};

#[derive(Parser)]
#[command(
    name = "tagtiger",
    version,
    about = "Tag MP4/M4V files with movie/TV metadata"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Parse a filename and list matching titles from TMDB.
    Search { file: PathBuf },
    /// Print existing metadata found in a file.
    Inspect { file: PathBuf },
    /// Fetch details for a provider id and write tags into the file.
    Tag {
        file: PathBuf,
        /// TMDB id to fetch.
        #[arg(long)]
        id: String,
        /// Treat the id as a TV show rather than a movie.
        #[arg(long)]
        tv: bool,
        /// Skip downloading/writing artwork.
        #[arg(long)]
        no_artwork: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Search { file } => search(file).await,
        Command::Inspect { file } => inspect(file),
        Command::Tag {
            file,
            id,
            tv,
            no_artwork,
        } => tag_file(file, id, tv, no_artwork).await,
    }
}

async fn search(file: PathBuf) -> Result<()> {
    let provider = TmdbProvider::from_env().context("Set TMDB_BEARER_TOKEN or TMDB_API_KEY")?;
    let query = naming::parse(&file)?;
    println!(
        "Parsed: title={:?} year={:?} kind={:?}",
        query.title, query.year, query.kind
    );
    let results = provider.search(&query).await?;
    if results.is_empty() {
        println!("No matches.");
        return Ok(());
    }
    println!("Matches:");
    for r in results {
        println!(
            "  [{}] {} ({})  {}",
            r.id.id,
            r.title,
            r.year.map(|y| y.to_string()).unwrap_or_else(|| "?".into()),
            r.overview
                .unwrap_or_default()
                .chars()
                .take(80)
                .collect::<String>()
        );
    }
    Ok(())
}

fn inspect(file: PathBuf) -> Result<()> {
    match mp4ameta::Tag::read_from_path(&file) {
        Ok(t) => {
            println!("Title:   {:?}", t.title());
            println!("Year:    {:?}", t.year());
            println!("Genre:   {:?}", t.genre());
            println!("Media:   {:?}", t.media_type());
            println!("Artworks: {}", t.artworks().count());
        }
        Err(e) => println!("No readable tags: {e}"),
    }
    Ok(())
}

async fn tag_file(file: PathBuf, id: String, tv: bool, no_artwork: bool) -> Result<()> {
    let provider = TmdbProvider::from_env().context("Set TMDB_BEARER_TOKEN or TMDB_API_KEY")?;
    let kind = if tv {
        MediaKind::TvShow
    } else {
        MediaKind::Movie
    };
    let pid = ProviderId {
        provider: "tmdb".into(),
        id,
        kind,
    };
    let mut meta = provider.fetch_details(&pid).await?;

    // For episodes, enrich with season/episode parsed from the filename.
    if let MediaKindMeta::Episode(ref mut ep) = meta.kind {
        if let Ok(q) = naming::parse(&file) {
            ep.season = q.season.unwrap_or(ep.season);
            ep.episode = q.episode.unwrap_or(ep.episode);
        }
    }

    // Choose artwork: first candidate.
    let encoded = if no_artwork {
        None
    } else if let Some(art) = meta.artwork.first() {
        let client = reqwest::Client::new();
        let bytes = artwork::download(&client, &art.url).await?;
        Some(artwork::normalize_for_cover(&bytes)?)
    } else {
        None
    };

    tag::write_to_file(&file, &meta, encoded.as_ref())?;
    println!("Wrote tags to {}", file.display());
    Ok(())
}
