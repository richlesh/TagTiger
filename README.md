# TagTiger

Cross-platform MP4/M4V metadata tagger. Looks up movie (and, later, TV episode)
metadata and posters from TMDB and writes Apple/iTunes-style atoms
(`©nam`, `©day`, `desc`/`ldes`, `covr`, `stik`, the `iTunMOVI` cast/director
plist, and TV atoms) directly into the file.

Written in Rust. No external command-line tools are required at runtime — MP4
atom writing is done in-process via the pure-Rust `mp4ameta` crate. It ships as
a single native binary per platform (Linux, macOS, Windows on x86-64 and ARM64).

## Workspace layout

```
core/   library — domain model, MP4 atom tagging, providers, naming, artwork
cli/    thin command-line frontend (binary: tagtiger)
gui/    egui desktop app (binary: tagtiger-gui) with a poster-selection grid
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
tagtiger inspect movie.m4v                     # show existing tags
tagtiger tag     "The Matrix (1999).mp4" --id 603   # fetch + write tags (+poster)
```

## Metadata written

| Field         | Atom / location                              |
|---------------|----------------------------------------------|
| Title         | `©nam`                                        |
| Release date  | `©day`                                        |
| Description   | `desc` (short) + `ldes` (long)                |
| Genres        | `©gen`                                        |
| Media kind    | `stik` (Movie=9 / TV Show=10)                 |
| Cast/crew     | `iTunMOVI` plist inside `----`/`com.apple.iTunes` |
| Poster        | `covr` (JPEG/PNG)                             |
| TV (later)    | `tvsh`, `tvnn`, `tvsn`, `tves`, `tven`        |

## Platform support & packaging

CI builds six targets and publishes them on tagged releases (`vX.Y.Z`):

| OS      | x86-64                        | ARM64                          |
|---------|-------------------------------|--------------------------------|
| Linux   | `x86_64-unknown-linux-gnu`    | `aarch64-unknown-linux-gnu`    |
| macOS   | `x86_64-apple-darwin`         | `aarch64-apple-darwin`         |
| Windows | `x86_64-pc-windows-msvc`      | `aarch64-pc-windows-msvc`      |

Linux releases include `.deb` and `.rpm` packages in addition to a `.tar.gz`.
A single Linux binary per architecture runs on both Debian- and Red Hat-family
distributions; the `-gnu` builds are produced on an older Ubuntu image to keep
the glibc requirement low. macOS ships `.tar.gz`, Windows ships `.zip`.

> macOS note: distributed binaries should be codesigned and notarized for
> Gatekeeper. That step is not yet wired into CI.

## License

GNU General Public License v3.0 or later (GPL-3.0-or-later). See [LICENSE](LICENSE).
