//! Thin CLI frontend over `tagtiger-core`.
//!
//! Subcommands:
//!   tagtiger search  <file>            Parse filename, search TMDB, list matches.
//!   tagtiger search  <show> <title>    Search a TV show's episodes by title
//!                                      (Show → Season → Episode results).
//!   tagtiger inspect <file>            Print existing tags in a file.
//!   tagtiger tag     <file> --id <id>  Fetch details for a TMDB id and write tags.
//!   tagtiger tag     <file> --rating <R>  Set only the content rating in place.
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
    /// Search TMDB. With one argument, parse it as a filename and list matching
    /// titles. With two arguments, the first is a TV show name and the second
    /// is an episode title to find (results shown as Show → Season → Episode).
    #[command(
        override_usage = "tagtiger search <FILE>\n       tagtiger search <TV SHOW> <EPISODE TITLE>",
        after_help = "EXAMPLES:\n  \
            tagtiger search \"The Matrix (1999).mp4\"      Parse the filename and list movie matches\n  \
            tagtiger search \"Breaking Bad\" \"Pilot\"       List episodes titled \"Pilot\" (Show > Season > Episode)"
    )]
    Search {
        /// One filename, or two strings: <TV show> <episode title>.
        #[arg(required = true, num_args = 1..=2)]
        args: Vec<String>,
    },
    /// Print existing metadata found in a file.
    Inspect { file: PathBuf },
    /// Write tags into a file. With --id, fetch full metadata from TMDB and
    /// write it. With --rating (and no --id), set just the content rating,
    /// leaving all other tags untouched (no TMDB lookup needed).
    Tag {
        file: PathBuf,
        /// TMDB id to fetch.
        #[arg(long)]
        id: Option<String>,
        /// Set only the content rating (e.g. PG-13, R, TV-MA, Not Rated).
        /// Used without --id to update just the rating in place.
        #[arg(long)]
        rating: Option<String>,
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
        Command::Search { args } => search(args).await,
        Command::Inspect { file } => inspect(file),
        Command::Tag {
            file,
            id,
            rating,
            tv,
            no_artwork,
        } => tag_file(file, id, rating, tv, no_artwork).await,
    }
}

async fn search(args: Vec<String>) -> Result<()> {
    let provider = TmdbProvider::from_env().context("Set TMDB_BEARER_TOKEN or TMDB_API_KEY")?;
    match args.as_slice() {
        // Two strings: <TV show> <episode title>. Search the show's episodes by
        // title and print the Show → Season → Episode hierarchy.
        [show, title] => search_episode_by_title(&provider, show, title).await,
        // One string: parse it as a filename and search movies/episodes.
        [file] => search_by_filename(&provider, PathBuf::from(file)).await,
        // clap enforces 1..=2 args, so this is unreachable in practice.
        _ => {
            anyhow::bail!("search takes a filename, or a TV show name and an episode title")
        }
    }
}

/// Parse a filename and list matching titles from TMDB (movie or episode per
/// the parsed kind).
async fn search_by_filename(provider: &TmdbProvider, file: PathBuf) -> Result<()> {
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

/// Search a TV show's episodes by title across all matching shows and seasons,
/// printing the results as a Show → Season → Episode hierarchy.
async fn search_episode_by_title(provider: &TmdbProvider, show: &str, title: &str) -> Result<()> {
    println!("Searching “{show}” episodes matching “{title}”…");
    let shows = provider.search_episode_tree_by_title(show, title).await?;
    if shows.is_empty() {
        println!("No matching episodes.");
        return Ok(());
    }
    for s in shows {
        let year = s.year.map(|y| format!(" ({y})")).unwrap_or_default();
        println!("{}{}  [{}]", s.show_name, year, s.series_id);
        for season in s.seasons {
            println!("  {} (season {})", season.name, season.season_number);
            for ep in season.episodes {
                println!("    {}: {}", ep.number, ep.name);
            }
        }
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
    // TV Show fields, when the file carries episode metadata.
    if let MediaKindMeta::Episode(ep) = &meta.kind {
        println!(
            "Show:         {}",
            if ep.show_name.is_empty() {
                "(none)".into()
            } else {
                ep.show_name.clone()
            }
        );
        println!(
            "Episode ID:   {}",
            ep.episode_id
                .clone()
                .unwrap_or_else(|| format!("{}x{:02}", ep.season, ep.episode))
        );
        println!("Season:       {}", ep.season);
        println!("Episode:      {}", ep.episode);
        println!("TV Network:   {}", opt(&ep.network));
    }
    Ok(())
}

async fn tag_file(
    file: PathBuf,
    id: Option<String>,
    rating: Option<String>,
    tv: bool,
    no_artwork: bool,
) -> Result<()> {
    // Validate a supplied rating up front against the accepted labels.
    if let Some(r) = rating.as_deref() {
        let r = r.trim();
        if !tag::is_valid_content_rating(r) {
            anyhow::bail!(
                "unknown rating {r:?}; valid ratings: {}",
                tag::content_ratings().join(", ")
            );
        }
    }

    // Rating-only mode: with --rating and no --id, set just the content rating
    // in place, leaving all other tags untouched. No TMDB lookup needed.
    if id.is_none() {
        let Some(rating) = rating else {
            anyhow::bail!("tag requires --id (fetch from TMDB) or --rating (set rating only)");
        };
        return set_rating_only(file, rating.trim().to_string());
    }
    let id = id.expect("id is Some in the fetch path");

    let provider = TmdbProvider::from_env().context("Set TMDB_BEARER_TOKEN or TMDB_API_KEY")?;
    let kind = if tv {
        MediaKind::TvShow
    } else {
        MediaKind::Movie
    };
    // For TV, derive season/episode from the filename so the provider can
    // fetch the specific episode's details.
    let (fn_season, fn_episode) = if tv {
        match naming::parse(&file) {
            Ok(q) => (q.season, q.episode),
            Err(_) => (None, None),
        }
    } else {
        (None, None)
    };
    let pid = ProviderId {
        provider: "tmdb".into(),
        id,
        kind,
        season: fn_season,
        episode: fn_episode,
    };
    let mut meta = provider.fetch_details(&pid).await?;

    // For episodes, ensure season/episode are set even if the provider left
    // them at 0 (e.g. sparse episode data): fall back to the filename values.
    if let MediaKindMeta::Episode(ref mut ep) = meta.kind {
        if ep.season == 0 {
            ep.season = fn_season.unwrap_or(0);
        }
        if ep.episode == 0 {
            ep.episode = fn_episode.unwrap_or(0);
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

    // An explicit --rating overrides the fetched content rating.
    if let Some(r) = rating {
        meta.content_rating = Some(r.trim().to_string());
    }

    // Preserve the file's existing layout: keep a fast-start file fast-start,
    // and a moov-last file moov-last.
    let fast_start = tagtiger_core::mp4rewrite::is_fast_start(&file).unwrap_or(false);
    tag::write_to_file(&file, &meta, encoded.as_ref(), fast_start)?;
    println!("Wrote tags to {}", file.display());
    Ok(())
}

/// Set only the content rating in place: read the file's existing metadata,
/// replace the rating, and write it back, preserving all other tags, the cover
/// art, and the fast-start layout. No TMDB access.
fn set_rating_only(file: PathBuf, rating: String) -> Result<()> {
    let (mut meta, cover) = tag::read_from_file(&file)
        .with_context(|| format!("reading tags from {}", file.display()))?;
    meta.content_rating = Some(rating.clone());

    // Re-embed the existing cover (if any) so the write preserves it.
    let encoded = match cover {
        Some(bytes) => Some(artwork::normalize_for_cover(&bytes)?),
        None => None,
    };

    let fast_start = tagtiger_core::mp4rewrite::is_fast_start(&file).unwrap_or(false);
    tag::write_to_file(&file, &meta, encoded.as_ref(), fast_start)?;
    println!("Set rating to {rating} in {}", file.display());
    Ok(())
}
