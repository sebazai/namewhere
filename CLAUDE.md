# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

A Rust CLI tool that renames image and video files using GPS reverse geocoding. It reads GPS coordinates from embedded metadata (EXIF for images, ffprobe for videos) and calls the Geoapify API to resolve coordinates into country/city names. Files are renamed to `YYMMDD-Country-City[-Description].ext`. Capture date comes from EXIF / ffprobe when present; otherwise the user’s year+month become **`YYMM00`**. Legacy `YYMM-…` stems are still recognized. When no GPS data is found, the user is prompted for place names manually.

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

- **`flow.rs`** — top-level orchestration. Picks a folder via native dialog or terminal prompt, collects media files, prompts for year/month (fallback **`YYMM00`** when no embedded date), then iterates each file: opens it, reads GPS, calls Geoapify or prompts for manual input, builds a filename stem, and renames.
- **`gps.rs`** — GPS and capture date: `coordinates()` from EXIF / ffprobe; `capture_yymmdd()` from EXIF DateTime* tags or ffprobe `creation_time` / similar. Images prefer EXIF; videos prefer ffprobe.
- **`geoapify.rs`** — blocking HTTP call to `https://api.geoapify.com/v1/geocode/reverse`. Returns `(country, city)`. City falls back through `city → name → county → formatted` fields.
- **`naming.rs`** — filename construction. `sanitize_segment()` replaces unsafe characters with `-`. `build_stem()` produces `YYMMDD-Country-City[-Description]` (capped at 180 chars). `classify_tool_stem()` accepts legacy 4-digit or 6-digit date prefixes; `normalize_date_prefix_for_stem()` maps `YYMM` → `YYMM00`. `unique_target_path()` appends `-2`, `-3`, … to avoid collisions.
- **`media.rs`** — non-recursive directory scan returning sorted `Vec<PathBuf>` of files with recognized extensions.

## Key Behaviors

- After classifying “already named” files, **images** whose stem matches legacy four-segment `YYMM-Country-City-Description` (optionally `…-N` collision suffix) are renamed to `YYMMDD-…` using EXIF `capture_yymmdd`, or `YYMM00` from the file’s existing `YYMM` if EXIF has no date. Multi-segment place names (more than four logical segments) are not auto-upgraded.
- File scan is **non-recursive** (single folder only).
- Files are opened in the system viewer before prompting for description, so the user can see the image/video.
- Extension is lowercased in the output filename.
- The `rfd` crate opens a native folder-picker dialog; falls back to terminal prompt if the dialog is unavailable.
