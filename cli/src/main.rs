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
    // Install the rustls `ring` crypto provider process-wide (reqwest is built
    // with `rustls-no-provider`, so no provider is auto-installed). Ignore an
    // error, which only means one is already installed.
    let _ = rustls::crypto::ring::default_provider().install_default();

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
    // Read the full metadata via core (parses the iTunMOVI plist, hdvd/derived
    // definition, iTunEXTC rating, etc.), plus the raw cover bytes.
    let (meta, cover) = tag::read_from_file(&file)
        .with_context(|| format!("reading tags from {}", file.display()))?;

    let names = |people: &[tagtiger_core::model::Person]| {
        if people.is_empty() {
            "(none)".to_string()
        } else {
            people
                .iter()
                .map(|p| p.name.clone())
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    let opt = |o: &Option<String>| o.clone().unwrap_or_else(|| "(none)".into());

    println!(
        "Title:        {}",
        if meta.title.is_empty() {
            "(none)".into()
        } else {
            meta.title.clone()
        }
    );
    println!(
        "Release date: {}",
        meta.release_date
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "(none)".into())
    );
    println!(
        "Media kind:   {}",
        meta.video_kind.map(|k| k.label()).unwrap_or("(none)")
    );
    println!(
        "Definition:   {}",
        meta.definition.map(|d| d.label()).unwrap_or("(none)")
    );
    println!("Rating:       {}", opt(&meta.content_rating));
    println!(
        "Genres:       {}",
        if meta.genres.is_empty() {
            "(none)".to_string()
        } else {
            meta.genres.join(", ")
        }
    );
    println!("Studio:       {}", opt(&meta.studio));
    println!("Directors:    {}", names(&meta.directors));
    println!("Cast:         {}", names(&meta.cast));
    println!("Producers:    {}", names(&meta.producers));
    println!("Screenwriters:{}", names(&meta.writers));
    println!("Summary:      {}", opt(&meta.summary));
    println!("Long desc.:   {}", opt(&meta.overview));
    println!(
        "Artwork:      {}",
        match &cover {
            Some(bytes) => format!("yes ({} bytes)", bytes.len()),
            None => "(none)".into(),
        }
    );
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

    // Deduce the video Definition (hdvd) from the file's actual track
    // dimensions when the provider didn't supply one (TMDB never does). This
    // mirrors what the GUI does when a file is opened.
    if meta.definition.is_none() {
        if let Some((w, h)) = tagtiger_core::mp4dim::video_dimensions(&file) {
            meta.definition = Some(tagtiger_core::model::Definition::from_dimensions(w, h));
        }
    }

    // Preserve the file's existing layout: keep a fast-start file fast-start,
    // and a moov-last file moov-last.
    let fast_start = tagtiger_core::mp4rewrite::is_fast_start(&file).unwrap_or(false);
    tag::write_to_file(&file, &meta, encoded.as_ref(), fast_start)?;
    println!("Wrote tags to {}", file.display());
    Ok(())
}
