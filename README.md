# TagTiger

Cross-platform MP4/M4V metadata tagger. Looks up movie (and, later, TV episode)
metadata and posters from TMDB and writes Apple/iTunes-style atoms directly into
the file — title, release date, short/long description, genres, content rating,
media kind, HD/4K definition, studio, the `iTunMOVI` cast/director/producer/
screenwriter plist, cover art, and TV atoms.

Written in Rust. No external command-line tools are required at runtime — MP4
atom writing is done in-process via the pure-Rust `mp4ameta` crate. It ships as
a single native binary per platform (Linux, macOS, Windows on x86-64 and ARM64).

The desktop GUI is a full metadata editor: search TMDB, pick from a poster grid,
edit every field (with per-field locks, undo/redo, and paste/drop of custom
poster art), choose the video definition, and toggle a **fast-start**
(web-optimized) layout on save.

## Workspace layout

```
core/   library — domain model, MP4 atom tagging, providers, naming, artwork
cli/    thin command-line frontend (binary: tagtiger)
gui/    egui desktop app (binary: tagtiger-gui): metadata editor + poster grid
```

The design keeps concerns decoupled:

- `providers/` (TMDB, and future OMDb/TVDB) produce a provider-agnostic
  `MediaMetadata`.
- `tag/` is the only module that knows atom codes; it maps `MediaMetadata` onto
  MP4 atoms.
- `naming/` parses Plex-style filenames into search queries.
- The CLI and GUI are thin frontends over `core`.

## Building

Requires a recent stable Rust toolchain.

```sh
cargo build --release            # builds all crates
cargo run -p tagtiger-cli -- --help
cargo run -p tagtiger-gui          # launch the GUI
```

### Linux build dependencies (GUI)

The egui GUI needs system X11/OpenGL client libraries at build and run time:

```sh
sudo apt-get install -y libx11-dev libxcursor-dev libxrandr-dev \
  libxi-dev libgl1-mesa-dev libxkbcommon-dev
```

## Usage (CLI)

Set a TMDB credential first. Preferred is a **v4 read access token** (Bearer):

```sh
export TMDB_BEARER_TOKEN=your_v4_read_access_token
```

Alternatively, a legacy **v3 API key** is still supported as a fallback:

```sh
export TMDB_API_KEY=your_v3_api_key
```

Both are found in your TMDB account under Settings → API. If both are set,
the Bearer token takes precedence.

```sh
tagtiger search  "The Matrix (1999).mp4"      # parse name + list TMDB matches
tagtiger inspect movie.m4v                     # show existing tags (all fields)
tagtiger tag     "The Matrix (1999).mp4" --id 603   # fetch + write tags (+poster)
```

`inspect` prints the full set of tags read back from the file — title, release
date, media kind, definition, rating, genres, studio, directors, cast,
producers, screenwriters, summary, long description, and whether cover art is
present. `tag` preserves the file's existing fast-start layout and, when TMDB
doesn't supply a definition, deduces it from the video track's dimensions.

## Usage (GUI)

```sh
cargo run -p tagtiger-gui          # or run the packaged binary
```

The GUI is a full editor. Open an MP4/M4V (File ▸ Open…, drag-and-drop, or an
"Open With" launch), search TMDB, and pick a poster from the grid. Every field
is editable with a per-field **Lock** (locked fields aren't overwritten when you
select a different match), and edits support **undo/redo**. Poster art can be
selected, copied/cut, and replaced by pasting or dropping an image; a
**Fast-start** checkbox controls whether the file is saved web-optimized
(`moov` before `mdat`) or with `moov` last. The window/dock/taskbar icon and, on
macOS/Windows, the executable and installer icons are bundled.

### License

TagTiger is donationware. Enter a license key via **Help ▸ License Key…** (email
+ key); it's validated and saved to `~/.tagtiger-settings.json`. A valid license
suppresses the startup splash and shows a thank-you in **Help ▸ About**.
Unlicensed use shows the splash on startup and again every 10 tag-write
operations.

## Metadata written

| Field         | Atom / location                              |
|---------------|----------------------------------------------|
| Title         | `©nam`                                        |
| Release date  | `©day`                                        |
| Description   | `desc` (short) + `ldes` (long)                |
| Genres        | `©gen`                                        |
| Content rating| `iTunEXTC` (e.g. `mpaa|PG-13|300|`)           |
| Media kind    | `stik` (Movie=9 / TV Show=10)                 |
| Definition    | `hdvd` (SD=0 / 720p=1 / 1080p=2 / 4K=3)       |
| Studio        | `©pub` and `iTunMOVI` `studio`                |
| Cast/crew     | `iTunMOVI` plist inside `----`/`com.apple.iTunes` (cast, directors, producers, screenwriters) |
| Poster        | `covr` (JPEG/PNG)                             |
| TV (later)    | `tvsh`, `tvnn`, `tvsn`, `tves`, `tven`        |

## Platform support & packaging

CI builds five targets and publishes them on tagged releases (`vX.Y.Z`):

| OS      | x86-64                        | ARM64                          |
|---------|-------------------------------|--------------------------------|
| Linux   | `x86_64-unknown-linux-gnu`    | `aarch64-unknown-linux-gnu`    |
| macOS   | — (use ARM64 via Rosetta 2)   | `aarch64-apple-darwin`         |
| Windows | `x86_64-pc-windows-msvc`      | `aarch64-pc-windows-msvc`      |

Linux releases include `.deb` and `.rpm` packages in addition to a `.tar.gz`.
A single Linux binary per architecture runs on both Debian- and Red Hat-family
distributions; the `-gnu` builds are produced on an older Ubuntu image to keep
the glibc requirement low. macOS ships a notarized `.dmg` containing the
`TagTiger.app` (with the `tagtiger` CLI inside it at
`Contents/MacOS/tagtiger-cli`). To put the CLI on your PATH, launch the app and
choose **Help ▸ Install Command-Line Tool…**, which symlinks
`/usr/local/bin/tagtiger` to the notarized binary inside the app (prompting for
admin rights only if `/usr/local/bin` isn't writable). A loose installer script
is intentionally not shipped — a standalone `.command` can't be notarized, so
Gatekeeper hard-blocks it on download. Windows ships a signed Inno Setup
installer (`TagTiger-<arch>-Setup.exe`) that lets you install the GUI, the
`tagtiger` CLI, or both — creating Start Menu/desktop shortcuts, registering
`.mp4`/`.m4v` in "Open with", and optionally adding the CLI to your PATH — plus
a portable `.zip` for those who prefer loose binaries. Loose macOS
binaries are intentionally not published — Gatekeeper quarantines them, so
everything ships inside the notarized `.dmg`.

Releases are cut from the **Release** workflow: run it manually with a version
(the tag is created and pushed for you) or push a `vX.Y.Z` tag directly.

### Code signing

Code signing is wired into the release workflow and activates when the relevant
secrets are configured (builds are produced unsigned otherwise):

- **macOS** — the `.app` and `.dmg` are codesigned with a Developer ID
  Application certificate (hardened runtime + `packaging/entitlements.plist`),
  then notarized with `notarytool` and stapled. Secrets: `APPLE_CERTIFICATE_BASE64`,
  `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY`, `APPLE_API_KEY`,
  `APPLE_API_KEY_ID`, `APPLE_API_ISSUER`.
- **Windows** — the executables are signed via Azure Trusted Signing. Secrets:
  `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, `AZURE_SUBSCRIPTION_ID`,
  `AZURE_SIGNING_ENDPOINT`, `AZURE_SIGNING_ACCOUNT_NAME`,
  `AZURE_SIGNING_CERTIFICATE_PROFILE_NAME`.

The GUI's license salt lives in the git-ignored `gui/src/license.rs`; CI
generates it from the `LICENSE_SALT` secret when the file is absent.

## License

GNU General Public License v3.0 or later (GPL-3.0-or-later). See [LICENSE](LICENSE).
