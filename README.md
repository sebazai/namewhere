# img-reverse-geolocation

A small Rust CLI that renames photos and videos in a folder using **GPS coordinates** (reverse geocoded via [Geoapify](https://www.geoapify.com/)) or **manual place names** when no location metadata is present. Output names follow:

`YYMMDD-Country-City[-Description].ext`

The **date** is taken from **EXIF** (images) or **ffprobe** tags (videos) when possible. If there is no usable capture date, the tool uses the year and month you enter plus **`00`** for the day (e.g. session April 2026 → `260400-…`). Legacy files that still use a **4-digit** `YYMM-…` prefix are recognized for skip / resume behavior. On each run, **images** that are skipped as fully named in the legacy `YYMM-Country-City-Description` shape (four dash segments, after stripping a trailing `-2` collision suffix) are **re-typed** to `YYMMDD-…` using EXIF capture date when present, or **`YYMM00-…`** from the filename’s month when not.

The scan is **one folder only** (not recursive). Files are opened in your default viewer before you enter an optional description.

## Supported formats

Images: `jpg`, `jpeg`, `png`, `gif`, `webp`, `heic`, `tif`, `tiff`  
Videos: `mp4`, `mov`, `m4v`, `avi`, `mkv`

GPS and capture dates come from **EXIF** on images and from **ffprobe** (ffmpeg) on video metadata.

## Requirements

- **Rust** (2021 edition toolchain; `cargo`, `rustc`)
- **`ffprobe`** on your `PATH` (from [ffmpeg](https://ffmpeg.org/)) — needed for video location tags
- **Geoapify API key** — optional for files *with* GPS. If `GEOAPIFY_API_KEY` is not set (and not in `.env`), the program prompts once when it hits the first file that has GPS (input is hidden). Press Enter to skip and fall back to manual place prompts for those files. The key is not saved; **each run** asks again unless the variable is set in the environment or `.env`.

## Quick start

1. Clone the repo and enter the project directory.

2. Set your API key (either is fine):

   - Copy `.env.example` to `.env` and set `GEOAPIFY_API_KEY`, or  
   - Export `GEOAPIFY_API_KEY` in your shell.

3. Build and run:

   ```bash
   cargo run --release
   ```

The program is **interactive**: it asks for a folder (native picker when available, otherwise a path in the terminal), then year and month used as **`YYMM00`** when a file has no embedded date, then walks each media file in that folder.

## Development

Common commands:

| Command | Purpose |
|--------|---------|
| `cargo build` | Debug build |
| `cargo build --release` | Optimized build |
| `cargo run` | Run the CLI (debug) |
| `cargo test` | Run tests |
| `cargo clippy` | Lint |
| `cargo fmt` | Format code |
| `cargo fmt -- --check` | Check formatting without writing |

### Dev container

This repo includes a [Dev Container](https://containers.dev/) under `.devcontainer/`. In VS Code or compatible editors, “Reopen in Container” gives you Rust, rustfmt, and Clippy. The container can pass through `GEOAPIFY_API_KEY` from your host via `localEnv` (see `.devcontainer/devcontainer.json`).

You still need **`ffprobe`** available inside the container (or on the host, depending on how you run) if you work with videos.

## Project layout

| Path | Role |
|------|------|
| `src/main.rs` | Binary entrypoint |
| `src/lib.rs` | Public `run()` entry |
| `src/flow.rs` | Folder pick, prompts, per-file rename loop |
| `src/gps.rs` | GPS from EXIF / ffprobe |
| `src/geoapify.rs` | Reverse geocoding HTTP client |
| `src/naming.rs` | Safe filename segments, stem, collision suffixes |
| `src/media.rs` | Non-recursive media file listing |
| `tests/` | Tests and fixtures |

## License

See the repository’s license file if one is present; otherwise treat usage as defined by the project owner.
