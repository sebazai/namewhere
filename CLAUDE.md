# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

A Rust CLI tool that renames image and video files using GPS reverse geocoding. It reads GPS coordinates from embedded metadata (EXIF for images, ffprobe for videos) and calls the Geoapify API to resolve coordinates into country/city names. Files are renamed to the pattern `YYMM-Country-City[-Description].ext`. When no GPS data is found, the user is prompted to enter location names manually.

## Build & Run Commands

```bash
cargo build                   # debug build
cargo build --release         # release build
cargo run                     # run the CLI
cargo test                    # run all tests
cargo test <test_name>        # run a single test by name
cargo clippy                  # lint
cargo fmt                     # format
cargo fmt -- --check          # check formatting without writing
```

## Runtime Requirements

- `ffprobe` (from ffmpeg) must be on `PATH` for GPS extraction from video files.
- A `.env` file or `GEOAPIFY_API_KEY` enables automatic reverse geocoding without prompting. If unset, the first file with GPS triggers a one-time password-style prompt for the key (Enter skips geocoding for that run). Files without GPS still prompt for manual place names.

## Architecture

The program has a straightforward linear flow through these modules:

- **`flow.rs`** — top-level orchestration. Picks a folder via native dialog or terminal prompt, collects media files, prompts for year/month, then iterates each file: opens it, reads GPS, calls Geoapify or prompts for manual input, builds a filename stem, and renames.
- **`gps.rs`** — GPS extraction with two backends: `exif` crate for images (EXIF tags), and `ffprobe` subprocess for videos (ISO 6709 location tags in format/stream metadata). `coordinates()` is the public entry point; it tries EXIF first for images and ffprobe first for videos.
- **`geoapify.rs`** — blocking HTTP call to `https://api.geoapify.com/v1/geocode/reverse`. Returns `(country, city)`. City falls back through `city → name → county → formatted` fields.
- **`naming.rs`** — filename construction. `sanitize_segment()` replaces unsafe characters with `-`. `build_stem()` produces `YYMM-Country-City[-Description]` (capped at 180 chars). `unique_target_path()` appends `-2`, `-3`, … to avoid collisions.
- **`media.rs`** — non-recursive directory scan returning sorted `Vec<PathBuf>` of files with recognized extensions.

## Key Behaviors

- File scan is **non-recursive** (single folder only).
- Files are opened in the system viewer before prompting for description, so the user can see the image/video.
- Extension is lowercased in the output filename.
- The `rfd` crate opens a native folder-picker dialog; falls back to terminal prompt if the dialog is unavailable.
