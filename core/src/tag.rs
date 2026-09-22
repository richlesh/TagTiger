//! Maps the provider-agnostic [`MediaMetadata`] onto MP4 iTunes-style atoms and
//! writes them into an MP4/M4V file.
//!
//! This is the only place that knows about atom codes. It uses the pure-Rust
//! `mp4ameta` crate (compiled into our binary — not an external tool) for the
//! ISO-BMFF box handling, `ilst` entries, `covr` artwork, and freeform `----`
//! atoms, and handles the `moov` growth / sample-offset rewrite internally.
//!
//! Apple stores cast/directors/producers/screenwriters as an Apple **plist XML**
//! blob inside a freeform atom (`mean = com.apple.iTunes`, `name = iTunMOVI`).
//! We serialize that plist here.

use crate::artwork::{ArtworkFormat, EncodedArtwork};
use crate::error::Result;
use crate::model::{MediaKindMeta, MediaMetadata, Person};
use mp4ameta::{Data, DataIdent, Img, ImgFmt, MediaType, Tag};
use std::path::Path;

/// iTunes TV-specific freeform/atom identifiers.
const ITUNMOVI_MEAN: &str = "com.apple.iTunes";
const ITUNMOVI_NAME: &str = "iTunMOVI";

/// Standard 4-character codes for TV atoms (written via freeform-independent
/// fourcc data atoms).
const ATOM_TV_SHOW: [u8; 4] = *b"tvsh";
const ATOM_TV_NETWORK: [u8; 4] = *b"tvnn";
const ATOM_TV_EPISODE_ID: [u8; 4] = *b"tven";
// tvsn (season) and tves (episode) are BE-signed integers.
const ATOM_TV_SEASON: [u8; 4] = *b"tvsn";
const ATOM_TV_EPISODE: [u8; 4] = *b"tves";

/// Build a `Tag` in memory from metadata plus an optional chosen artwork.
///
/// The artwork is passed separately (already downloaded/normalized) because the
/// user picks it in the GUI from the candidate list.
pub fn build_tag(meta: &MediaMetadata, artwork: Option<&EncodedArtwork>) -> Tag {
    let mut tag = Tag::default();

    tag.set_title(meta.title.clone());

    if let Some(date) = meta.release_date {
        // `©day` accepts an ISO-8601 date/timestamp; full date is well
        // supported by players.
        tag.set_year(date.format("%Y-%m-%d").to_string());
    }

    if let Some(summary) = &meta.summary {
        // Short description (`desc`), limited to 255 characters.
        let s: String = summary.chars().take(255).collect();
        tag.set_description(s);
    }
    if let Some(overview) = &meta.overview {
        // Long description (`ldes`).
        set_long_description(&mut tag, overview);
    }

    if let Some(rating) = &meta.content_rating {
        if !rating.trim().is_empty() {
            // iTunes stores the content rating in the iTunEXTC freeform atom,
            // encoded like `mpaa|PG-13|300|` or `us-tv|TV-14|500|`.
            if let Some(encoded) = encode_itunextc(rating) {
                tag.set_data(
                    DataIdent::freeform(ITUNMOVI_MEAN, "iTunEXTC"),
                    Data::Utf8(encoded),
                );
            }
        }
    }

    if !meta.genres.is_empty() {
        tag.set_genre(meta.genres.join(", "));
    }

    if let Some(studio) = &meta.studio {
        set_fourcc_utf8(&mut tag, *b"\xa9pub", studio); // `©pub` studio/publisher
    }

    // Media kind -> `stik`. An explicit user choice takes precedence.
    if let Some(vk) = meta.video_kind {
        tag.set_media_type(media_type_for_stik(vk.stik()));
    } else {
        match meta.kind.kind() {
            crate::model::MediaKind::Movie => tag.set_media_type(MediaType::ShortFilm),
            crate::model::MediaKind::TvShow => tag.set_media_type(MediaType::TvShow),
        }
    }

    // Definition -> `hdvd` (BE-signed integer).
    if let Some(def) = meta.definition {
        set_fourcc_be_signed(&mut tag, *b"hdvd", def.hdvd() as i32);
    }

    // Cast / crew -> iTunMOVI plist.
    let plist = build_itunmovi_plist(meta);
    if let Some(xml) = plist {
        tag.set_data(
            DataIdent::freeform(ITUNMOVI_MEAN, ITUNMOVI_NAME),
            Data::Utf8(xml),
        );
    }

    // TV-episode atoms.
    if let MediaKindMeta::Episode(ep) = &meta.kind {
        set_fourcc_utf8(&mut tag, ATOM_TV_SHOW, &ep.show_name);
        if let Some(net) = &ep.network {
            set_fourcc_utf8(&mut tag, ATOM_TV_NETWORK, net);
        }
        set_fourcc_be_signed(&mut tag, ATOM_TV_SEASON, ep.season as i32);
        set_fourcc_be_signed(&mut tag, ATOM_TV_EPISODE, ep.episode as i32);
        let episode_id = format!("{}x{:02}", ep.season, ep.episode);
        set_fourcc_utf8(&mut tag, ATOM_TV_EPISODE_ID, &episode_id);
    }

    // Artwork -> `covr`.
    if let Some(art) = artwork {
        let img = match art.format {
            ArtworkFormat::Jpeg => Img::new(ImgFmt::Jpeg, art.bytes.clone()),
            ArtworkFormat::Png => Img::new(ImgFmt::Png, art.bytes.clone()),
        };
        tag.set_artwork(img);
    }

    tag
}

/// Write metadata into the file at `path`, preserving existing media data.
/// Read existing iTunes-style tags from an MP4/M4V file into a
/// [`MediaMetadata`], plus the raw cover-art bytes if present.
///
/// Best-effort: fields absent from the file are left empty/`None`. Cast and
/// directors are parsed from the `iTunMOVI` plist when present.
pub fn read_from_file(path: impl AsRef<Path>) -> Result<(MediaMetadata, Option<Vec<u8>>)> {
    let path = path.as_ref();
    let tag = Tag::read_from_path(path)?;

    let mut meta = MediaMetadata {
        title: tag.title().unwrap_or_default().to_string(),
        ..Default::default()
    };

    if let Some(year) = tag.year() {
        // `©day` may be a full date or just a year; try both.
        meta.release_date = chrono::NaiveDate::parse_from_str(year, "%Y-%m-%d")
            .ok()
            .or_else(|| {
                year.get(0..4)
                    .and_then(|y| y.parse::<i32>().ok())
                    .and_then(|y| chrono::NaiveDate::from_ymd_opt(y, 1, 1))
            });
    }

    if let Some(desc) = tag.description() {
        // `desc` = short summary.
        meta.summary = Some(desc.to_string());
    }
    // `ldes` = long description.
    if let Some(Data::Utf8(ldes)) = tag.data_of(&DataIdent::fourcc(*b"ldes")).next() {
        meta.overview = Some(ldes.clone());
    }
    // Content rating from the iTunEXTC freeform atom.
    if let Some(Data::Utf8(rating)) = tag
        .data_of(&DataIdent::freeform(ITUNMOVI_MEAN, "iTunEXTC"))
        .next()
    {
        meta.content_rating = decode_itunextc(rating);
    }
    // Media kind (`stik`) -> VideoKind.
    if let Some(mt) = tag.media_type() {
        meta.video_kind = crate::model::VideoKind::from_stik(mt as u8);
    }
    // Definition: prefer an existing `hdvd` atom, else deduce from the video
    // track dimensions.
    if let Some(Data::BeSigned(bytes)) = tag.data_of(&DataIdent::fourcc(*b"hdvd")).next() {
        if let Some(&b) = bytes.last() {
            meta.definition = crate::model::Definition::from_hdvd(b);
        }
    }
    if meta.definition.is_none() {
        if let Some((w, h)) = crate::mp4dim::video_dimensions(path) {
            meta.definition = Some(crate::model::Definition::from_dimensions(w, h));
        }
    }
    if let Some(genre) = tag.genre() {
        meta.genres = genre
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }

    // Cast / crew from the iTunMOVI plist, if present.
    if let Some(Data::Utf8(xml)) = tag
        .data_of(&DataIdent::freeform(ITUNMOVI_MEAN, ITUNMOVI_NAME))
        .next()
    {
        let people = parse_itunmovi_people(xml);
        meta.cast = people.cast;
        meta.directors = people.directors;
        meta.producers = people.producers;
        meta.writers = people.writers;
        if meta.studio.is_none() {
            meta.studio = people.studio;
        }
    }

    let cover = tag.artwork().map(|img| img.data.to_vec());

    Ok((meta, cover))
}

/// The people/credits parsed back out of an `iTunMOVI` plist.
#[derive(Default)]
struct ItunmoviPeople {
    cast: Vec<Person>,
    directors: Vec<Person>,
    producers: Vec<Person>,
    writers: Vec<Person>,
    studio: Option<String>,
}

/// Extract the cast/directors/producers/screenwriters name arrays and the
/// studio string from an `iTunMOVI` plist. Intentionally lightweight (no full
/// plist parser): scans for each key's `<array>` (or `<string>`) block and
/// pulls the `<string>` values.
fn parse_itunmovi_people(xml: &str) -> ItunmoviPeople {
    // Names inside the `<array>` that follows `<key>{key}</key>`.
    fn names_after_key(xml: &str, key: &str) -> Vec<Person> {
        let key_tag = format!("<key>{key}</key>");
        let Some(kpos) = xml.find(&key_tag) else {
            return Vec::new();
        };
        let rest = &xml[kpos + key_tag.len()..];
        let Some(astart) = rest.find("<array>") else {
            return Vec::new();
        };
        let aend = rest.find("</array>").unwrap_or(rest.len());
        let block = &rest[astart..aend];
        let mut people = Vec::new();
        let mut cursor = 0;
        while let Some(s) = block[cursor..].find("<string>") {
            let start = cursor + s + "<string>".len();
            if let Some(e) = block[start..].find("</string>") {
                let name = xml_unescape(&block[start..start + e]);
                if !name.trim().is_empty() {
                    people.push(Person::new(name.trim().to_string()));
                }
                cursor = start + e + "</string>".len();
            } else {
                break;
            }
        }
        people
    }

    // The single `<string>` value that follows `<key>{key}</key>`.
    fn string_after_key(xml: &str, key: &str) -> Option<String> {
        let key_tag = format!("<key>{key}</key>");
        let kpos = xml.find(&key_tag)?;
        let rest = &xml[kpos + key_tag.len()..];
        let sstart = rest.find("<string>")? + "<string>".len();
        let send = rest[sstart..].find("</string>")?;
        let val = xml_unescape(rest[sstart..sstart + send].trim());
        if val.is_empty() {
            None
        } else {
            Some(val)
        }
    }

    ItunmoviPeople {
        cast: names_after_key(xml, "cast"),
        directors: names_after_key(xml, "directors"),
        producers: names_after_key(xml, "producers"),
        writers: names_after_key(xml, "screenwriters"),
        studio: string_after_key(xml, "studio"),
    }
}

fn xml_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

/// Which phase of a layout-changing save is currently streaming, so a UI can
/// label its progress. An in-place edit (no layout change) reports no progress
/// at all, so callers never see these for the cheap path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WritePhase {
    /// Pass 1: copying the original to a working temp file.
    Copying,
    /// Pass 2: rewriting into the requested (fast-start / moov-last) layout.
    Optimizing,
}

impl WritePhase {
    /// A short human-readable label for status text.
    pub fn label(self) -> &'static str {
        match self {
            WritePhase::Copying => "Copying",
            WritePhase::Optimizing => "Optimizing layout",
        }
    }
}

pub fn write_to_file(
    path: impl AsRef<Path>,
    meta: &MediaMetadata,
    artwork: Option<&EncodedArtwork>,
    fast_start: bool,
) -> Result<()> {
    write_to_file_with_progress(path, meta, artwork, fast_start, &mut |_, _, _| {})
}

/// Write metadata, producing the layout requested by `fast_start`:
///
/// - `fast_start == false` (moov-last): if the file is already moov-last, edit
///   **in place** (mp4ameta) — a cheap tail edit that never moves the media.
///   Otherwise rewrite the media on a temp copy so `moov` ends up **after**
///   `mdat`, then atomic-swap.
/// - `fast_start == true` (web-optimized / moov-first): rewrite on a temp copy
///   so the final layout is `[ftyp][moov][free padding][mdat]`, patching the
///   `stco`/`co64` chunk-offset tables for the moved `mdat`, then atomic-swap.
///
/// In both rewrite cases the work is done on a sibling temp file and the
/// requested layout normalization is applied as a post-pass, so the on-disk
/// result matches the requested layout regardless of how mp4ameta placed the
/// boxes during its edit. The original is never modified until the finished
/// temp is swapped in, so a crash can't corrupt it. Progress is reported as
/// `(bytes_done, bytes_total)` during the streaming copies.
pub fn write_to_file_with_progress(
    path: impl AsRef<Path>,
    meta: &MediaMetadata,
    artwork: Option<&EncodedArtwork>,
    fast_start: bool,
    progress: &mut dyn FnMut(WritePhase, u64, u64),
) -> Result<()> {
    let path = path.as_ref();

    let currently_fast = crate::mp4rewrite::is_fast_start(path).unwrap_or(false);

    // Cheap path: the file is already in the layout the caller wants. mp4ameta
    // edits `moov` in place without ever reordering top-level boxes, so the
    // layout (moov-first or moov-last) is preserved:
    //   - moov-last stays moov-last: a trailing edit that doesn't move media.
    //   - moov-first stays moov-first: mp4ameta fixes up chunk offsets itself,
    //     and when the metadata is unchanged the tag size is identical so
    //     nothing shifts at all.
    // Either way, no full rewrite is needed.
    if fast_start == currently_fast {
        apply_tag_in_place(path, meta, artwork)?;
        return Ok(());
    }

    // Layout change requested (fast-start toggled). Rewrite on a sibling temp
    // copy, then atomic-swap. We run the requested layout normalization as a
    // post-pass so the on-disk result matches the requested layout regardless
    // of how mp4ameta placed the boxes during its edit.
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("tagtiger");
    let tmp = dir.join(format!(".{stem}.tagtiger.tmp"));
    let tmp2 = dir.join(format!(".{stem}.tagtiger.tmp2"));

    let result = (|| -> Result<()> {
        // 1) Copy original -> temp so mp4ameta never touches the original.
        crate::mp4rewrite::copy_file_with_progress(path, &tmp, &mut |d, t| {
            progress(WritePhase::Copying, d, t)
        })?;
        // 2) Let mp4ameta perform the tag edit on the temp.
        apply_tag_in_place(&tmp, meta, artwork)?;
        // 3) Normalize the temp into the requested layout (tmp -> tmp2), then
        //    swap tmp2 in as the finished temp.
        let normalized = if fast_start {
            crate::mp4rewrite::normalize_moov_first(&tmp, &tmp2, &mut |d, t| {
                progress(WritePhase::Optimizing, d, t)
            })?
        } else {
            crate::mp4rewrite::normalize_moov_last(&tmp, &tmp2, &mut |d, t| {
                progress(WritePhase::Optimizing, d, t)
            })?
        };
        if normalized {
            std::fs::rename(&tmp2, &tmp)?;
        }
        // 4) Atomically replace the original with the finished temp file.
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&tmp2);
    }
    result
}

/// Read the existing tag, overlay our managed atoms, and write in place.
fn apply_tag_in_place(
    path: &Path,
    meta: &MediaMetadata,
    artwork: Option<&EncodedArtwork>,
) -> Result<()> {
    let mut tag = Tag::read_from_path(path).unwrap_or_default();
    let built = build_tag(meta, artwork);
    merge(&mut tag, built);
    tag.write_to_path(path)?;
    Ok(())
}

/// Overlay the fields we set from `src` onto `dst`.
fn merge(dst: &mut Tag, src: Tag) {
    // mp4ameta doesn't expose a bulk merge; simplest correct approach is to
    // replace `dst` with `src` for the atoms we manage. Since `build_tag`
    // always regenerates the full managed set, replacing wholesale is correct.
    *dst = src;
}

/// Map a `stik` integer to the corresponding `mp4ameta::MediaType` variant.
fn media_type_for_stik(stik: u8) -> MediaType {
    match stik {
        0 => MediaType::Movie,
        1 => MediaType::Normal,
        2 => MediaType::AudioBook,
        6 => MediaType::MusicVideo,
        9 => MediaType::ShortFilm,
        10 => MediaType::TvShow,
        11 => MediaType::Booklet,
        _ => MediaType::Movie,
    }
}

fn set_long_description(tag: &mut Tag, text: &str) {
    // `ldes` long description.
    set_fourcc_utf8(tag, *b"ldes", text);
}

fn set_fourcc_utf8(tag: &mut Tag, code: [u8; 4], value: &str) {
    tag.set_data(DataIdent::fourcc(code), Data::Utf8(value.to_string()));
}

fn set_fourcc_be_signed(tag: &mut Tag, code: [u8; 4], value: i32) {
    tag.set_data(
        DataIdent::fourcc(code),
        Data::BeSigned(value.to_be_bytes().to_vec()),
    );
}

/// Serialize cast/directors/producers/screenwriters into the Apple `iTunMOVI`
/// plist XML. Returns `None` if there is nothing to write.
fn build_itunmovi_plist(meta: &MediaMetadata) -> Option<String> {
    if meta.cast.is_empty()
        && meta.directors.is_empty()
        && meta.producers.is_empty()
        && meta.writers.is_empty()
        && meta.studio.is_none()
    {
        return None;
    }

    let mut body = String::new();
    push_person_array(&mut body, "cast", &meta.cast);
    push_person_array(&mut body, "directors", &meta.directors);
    push_person_array(&mut body, "producers", &meta.producers);
    push_person_array(&mut body, "screenwriters", &meta.writers);
    if let Some(studio) = &meta.studio {
        body.push_str(&format!(
            "\t<key>studio</key>\n\t<string>{}</string>\n",
            xml_escape(studio)
        ));
    }

    Some(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n<dict>\n{body}</dict>\n</plist>\n"
    ))
}

/// Append a `<key>name</key><array><dict><key>name</key><string>..</string></dict>...</array>`.
fn push_person_array(out: &mut String, key: &str, people: &[Person]) {
    if people.is_empty() {
        return;
    }
    out.push_str(&format!("\t<key>{key}</key>\n\t<array>\n"));
    for p in people {
        out.push_str("\t\t<dict>\n\t\t\t<key>name</key>\n\t\t\t<string>");
        out.push_str(&xml_escape(&p.name));
        out.push_str("</string>\n\t\t</dict>\n");
    }
    out.push_str("\t</array>\n");
}
/// iTunes rating tables mapping the plain menu value to its numeric score.
/// Written as `system|label|score|`.
const ITUNEXTC_MOVIE: &[(&str, &str)] = &[
    ("G", "100"),
    ("PG", "200"),
    ("PG-13", "300"),
    ("R", "400"),
    ("NC-17", "500"),
    ("Not Rated", "0"),
    ("Unrated", "0"),
];
const ITUNEXTC_TV: &[(&str, &str)] = &[
    ("TV-Y", "100"),
    ("TV-Y7", "200"),
    ("TV-G", "300"),
    ("TV-PG", "400"),
    ("TV-14", "500"),
    ("TV-MA", "600"),
];

/// Encode a plain rating value (as shown in the menu) into the Apple
/// `iTunEXTC` string, e.g. `PG-13` -> `mpaa|PG-13|300|`. Returns `None` for an
/// unrecognized value.
fn encode_itunextc(rating: &str) -> Option<String> {
    let r = rating.trim();
    if let Some((label, score)) = ITUNEXTC_MOVIE.iter().find(|(l, _)| *l == r) {
        return Some(format!("mpaa|{label}|{score}|"));
    }
    if let Some((label, score)) = ITUNEXTC_TV.iter().find(|(l, _)| *l == r) {
        return Some(format!("us-tv|{label}|{score}|"));
    }
    None
}

/// Decode an `iTunEXTC` string back to the plain menu value, e.g.
/// `mpaa|PG-13|300|` -> `PG-13`. Returns the label field when present.
fn decode_itunextc(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    // The label is the second pipe-delimited field: `system|label|score|...`.
    let label = raw
        .split('|')
        .nth(1)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match label {
        Some(l) => Some(l.to_string()),
        None => Some(raw.to_string()),
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EpisodeInfo, MediaKindMeta, Person};

    fn sample_movie() -> MediaMetadata {
        MediaMetadata {
            title: "The Matrix".into(),
            release_date: chrono::NaiveDate::from_ymd_opt(1999, 3, 31),
            overview: Some("A hacker learns the truth.".into()),
            summary: Some("A hacker learns the truth.".into()),
            genres: vec!["Action".into(), "Sci-Fi".into()],
            cast: vec![Person::new("Keanu Reeves"), Person::new("Carrie-Anne Moss")],
            directors: vec![Person::new("Lana Wachowski")],
            producers: vec![Person::new("Joel Silver")],
            writers: vec![Person::new("Lilly Wachowski")],
            content_rating: Some("R".into()),
            studio: Some("Warner Bros.".into()),
            video_kind: Some(crate::model::VideoKind::Movie),
            definition: Some(crate::model::Definition::Hd1080),
            artwork: vec![],
            kind: MediaKindMeta::Movie,
        }
    }

    #[test]
    fn builds_movie_tag_fields() {
        let meta = sample_movie();
        let tag = build_tag(&meta, None);
        assert_eq!(tag.title(), Some("The Matrix"));
        assert_eq!(tag.year(), Some("1999-03-31"));
        // video_kind Movie -> stik 9 (mp4ameta names this variant ShortFilm).
        assert_eq!(tag.media_type(), Some(MediaType::ShortFilm));
        assert!(tag.genre().is_some());
    }

    #[test]
    fn itunmovi_contains_people() {
        let meta = sample_movie();
        let xml = build_itunmovi_plist(&meta).unwrap();
        assert!(xml.contains("<key>cast</key>"));
        assert!(xml.contains("Keanu Reeves"));
        assert!(xml.contains("<key>directors</key>"));
        assert!(xml.contains("Lana Wachowski"));
        assert!(xml.contains("<key>screenwriters</key>"));
    }

    #[test]
    fn parses_itunmovi_people_roundtrip() {
        let meta = sample_movie();
        let xml = build_itunmovi_plist(&meta).unwrap();
        let people = parse_itunmovi_people(&xml);
        let names = |v: &[Person]| v.iter().map(|p| p.name.clone()).collect::<Vec<_>>();
        assert_eq!(
            names(&people.cast),
            vec!["Keanu Reeves", "Carrie-Anne Moss"]
        );
        assert_eq!(names(&people.directors), vec!["Lana Wachowski"]);
        // These two were previously dropped on read — the bug this fixes.
        assert_eq!(names(&people.producers), vec!["Joel Silver"]);
        assert_eq!(names(&people.writers), vec!["Lilly Wachowski"]);
        assert_eq!(people.studio.as_deref(), Some("Warner Bros."));
    }

    #[test]
    fn unescapes_xml_people() {
        let mut meta = sample_movie();
        meta.cast = vec![Person::new("A & B <tag>")];
        meta.directors = vec![];
        let xml = build_itunmovi_plist(&meta).unwrap();
        let people = parse_itunmovi_people(&xml);
        assert_eq!(people.cast[0].name, "A & B <tag>");
    }

    #[test]
    fn deduces_definition_from_dimensions() {
        use crate::model::Definition;
        assert_eq!(Definition::from_dimensions(640, 480), Definition::Sd);
        assert_eq!(Definition::from_dimensions(1280, 720), Definition::Hd720);
        assert_eq!(Definition::from_dimensions(1920, 1080), Definition::Hd1080);
        assert_eq!(Definition::from_dimensions(3840, 2160), Definition::Uhd4k);
    }

    #[test]
    fn itunextc_roundtrip() {
        assert_eq!(encode_itunextc("PG-13").as_deref(), Some("mpaa|PG-13|300|"));
        assert_eq!(
            encode_itunextc("TV-14").as_deref(),
            Some("us-tv|TV-14|500|")
        );
        assert_eq!(encode_itunextc("bogus"), None);
        assert_eq!(decode_itunextc("mpaa|PG-13|300|").as_deref(), Some("PG-13"));
        assert_eq!(
            decode_itunextc("us-tv|TV-MA|600|").as_deref(),
            Some("TV-MA")
        );
        // Decoding then matching should yield a menu value.
        let enc = encode_itunextc("R").unwrap();
        assert_eq!(decode_itunextc(&enc).as_deref(), Some("R"));
    }

    #[test]
    fn escapes_xml_special_chars() {
        let mut meta = sample_movie();
        meta.cast = vec![Person::new("A & B <tag>")];
        let xml = build_itunmovi_plist(&meta).unwrap();
        assert!(xml.contains("A &amp; B &lt;tag&gt;"));
    }

    #[test]
    fn builds_episode_tv_atoms() {
        let mut meta = sample_movie();
        meta.video_kind = Some(crate::model::VideoKind::TvShow);
        meta.kind = MediaKindMeta::Episode(EpisodeInfo {
            show_name: "Breaking Bad".into(),
            season: 1,
            episode: 2,
            episode_title: Some("Cat's in the Bag".into()),
            network: Some("AMC".into()),
        });
        let tag = build_tag(&meta, None);
        assert_eq!(tag.media_type(), Some(MediaType::TvShow));
    }
}
