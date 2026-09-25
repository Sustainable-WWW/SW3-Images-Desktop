# 🌱 SW3 Images — desktop app

Smart, visually lossless image compression for macOS (Apple Silicon) and
Windows, built with [Tauri 2](https://tauri.app) and Rust. Drop in JPG, PNG or
WebP images (or whole folders) and get dramatically smaller files with no
visible quality loss. You can also resize them and convert them to WebP.

Made by [Sustainable WWW Sweden AB](https://sustainablewww.se) and
[Michael Andersen](https://michaelandersen.net).

## Features

- **No file limit.** Drop as many images or folders as you like. Folders are
  searched recursively, and hidden files and non-images are skipped.
- **Parallel compression.** Up to 5 images at a time (one CPU core is kept
  free so the UI stays smooth; machines with fewer cores use fewer workers).
- **Resize** to a max width (aspect ratio kept, never upscaled).
- **Convert to WebP** together with resizing and compression. Transparency
  is kept.
- **Automatic output folder.** Results go to `Pictures/SW3 Compressed`
  (created on first launch; change it with *Change* in the app). Originals are
  never touched, and existing files are never overwritten: name clashes
  become `photo (2).jpg`.
- **Native notification** when a batch finishes, plus progress in the Dock
  or taskbar icon.
- **Automatic updates** from GitHub Releases, verified with a signature.
- **About window** from the menu bar (macOS: *SW3 Images → About SW3
  Images*; Windows: *Help → About SW3 Images*). The text is in
  [`ui/about.html`](ui/about.html).
- Stop at any time; a failed image can be retried on its own.

## Installing (for users)

Download the latest installer from
[Releases](https://github.com/Sustainable-WWW/SW3-Images-Desktop/releases/latest).

- **macOS:** open the `.dmg` and drag SW3 Images to Applications. The app is
  not notarized by Apple, so the first launch shows an "unidentified
  developer" warning. Right-click the app and choose **Open**, or go to
  **System Settings → Privacy & Security → Open Anyway**. You only need to do
  this once, and updates install without the warning.
- **Windows:** run `SW3 Images_x.y.z_x64-setup.exe`. If SmartScreen warns,
  click **More info → Run anyway**. It installs per user, so no admin rights
  are needed.

## Development

Requirements: Rust (stable), Node.js 20+, and on Windows also
[NASM](https://nasm.us) (for mozjpeg's SIMD code).

```sh
npm install        # installs the Tauri CLI
npm run dev        # runs the app with hot reload of the Rust side
npm run build      # builds installers into target/release/bundle/
```

To preview only the UI in a normal browser (with fake data), run
`node scripts/preview-ui.mjs` and open http://127.0.0.1:1420.

There's also a CLI for quick engine testing and benchmarking:

```sh
cargo run --release --example compress_file -- photo.jpg logo.png
```

## Tests

```sh
cargo test --workspace   # Rust: engine, file safety, batch pipeline, security config
npm test                 # UI: helpers + static security checks
```

GitHub Actions ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)) runs
all of this on **macOS and Windows on every push and pull request**, together
with clippy and a RustSec/npm vulnerability audit of all dependencies.

What the security tests check:

- **Untrusted files:** formats are detected from their content (magic bytes),
  not their names. Fake, corrupt, truncated and empty files fail cleanly, and
  malformed EXIF data can't crash the parser (a panic bug found in the old
  parser is fixed and covered by a test). Codec panics are caught per image.
- **Writing files:** names are sanitized (`../`, drive letters, Windows
  reserved names like `CON`), so nothing can be written outside the output
  folder. Files are never overwritten, including when the output folder is
  the same as the source folder or several workers write at once.
- **Folder walking:** symlinked folders aren't followed, so symlink loops
  can't cause endless scanning, and the output folder is never read back in.
- **Webview lockdown:** a strict CSP (no inline or remote scripts, no eval,
  IPC only), and the webview gets only the minimum permissions. The UI can't
  access the file system, shell, updater or notification APIs, and can't
  choose where files are written (only a native folder picker can). The About
  window can only open a fixed list of links.
- **UI code:** no `innerHTML` or other HTML sinks, since file names from disk
  are always written as text. No inline handlers and no remote resources.
- **Updates:** they must come from this GitHub repository over HTTPS and be
  signed. A test also fails if a private signing key is ever committed.

## Releasing and auto-updates

The app checks
`https://github.com/Sustainable-WWW/SW3-Images-Desktop/releases/latest/download/latest.json`
shortly after launch, then every 6 hours, and also from the menu: *Check for
Updates…*. When an update exists, a small "Update & restart" button appears.
Every download is checked against the public key in
`src-tauri/tauri.conf.json` before it is installed.

**One-time setup:** add the updater signing key as a repository secret.

1. GitHub → the repo → **Settings → Secrets and variables → Actions → New
   repository secret**
2. Name: `TAURI_SIGNING_PRIVATE_KEY`. Value: the full contents of
   `~/.tauri/sw3-images-updater.key` on the machine where it was generated.
   The key has no password, so `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` isn't
   needed.

> ⚠️ Back up `~/.tauri/sw3-images-updater.key` somewhere safe, like a
> password manager. If it is lost, installed apps can no longer be updated
> automatically. Never commit it.

**Each release:**

1. Bump `version` in [`src-tauri/Cargo.toml`](src-tauri/Cargo.toml) (e.g.
   `0.2.0`) and commit.
2. `git tag v0.2.0 && git push origin main v0.2.0`

The [release workflow](.github/workflows/release.yml) runs the tests, builds
the macOS (Apple Silicon) `.dmg` and the Windows `-setup.exe`, and publishes a
GitHub Release with `latest.json`. Installed apps pick it up automatically.

## How the compression works

SW3 Images doesn't use a fixed "quality 80" setting. For every image it
*measures* perceptual quality and searches for the smallest file that still
looks identical:

1. The image is decoded and used as a reference (EXIF orientation is applied
   first, so phone photos never come out rotated).
2. Each candidate encoding is compared with the reference using **DSSIM**, a
   structural-similarity measure in Lab color space that models how people
   actually see differences.
3. A binary search finds the lowest encoder quality whose DSSIM stays under a
   "visually lossless" ceiling.
4. **Large images are judged at display scale.** Anything over 1600 px on the
   long edge is compared at the size it is normally viewed at.
5. **Probe search keeps CPU use low.** For big images, the search runs on a
   small copy, and only 2–4 full-resolution encodes are used to confirm and
   fine-tune the result.

By format:

- **JPEG** is re-encoded with **mozjpeg** (trellis quantization, progressive
  scans). Grayscale photos are encoded as one channel. When 4:2:0 chroma
  subsampling would blur fine color detail, it falls back to 4:4:4.
- **PNG** tries two options: lossless **oxipng** and **imagequant** palette
  quantization, which is only used if DSSIM says it looks identical. The
  smaller one wins.
- **WebP** uses a lossy quality search via libwebp. Graphics with few colors
  also get a lossless candidate.
- **Convert to WebP** runs the WebP search against the decoded source pixels,
  so the converted file is held to the same visually lossless standard.

When the format and size stay the same, SW3 Images never makes a file
bigger: if nothing beats the original, the original is kept. Metadata (EXIF,
GPS) is removed, which saves space and protects privacy.

## Project layout

| Path | Purpose |
| --- | --- |
| `src/compress.rs` | The compression engine (DSSIM-guided quality search, WebP conversion) |
| `src/files.rs` | Folder scanning, header-only probing, safe output naming and writing |
| `src-tauri/src/lib.rs` | Tauri commands: scan, pick, batch start/stop/retry, notifications |
| `src-tauri/src/batch.rs` | Parallel worker pool, cancellation, progress events |
| `src-tauri/src/menu.rs` | Native menu bar and the About window |
| `src-tauri/src/updates.rs` | GitHub-based self-updates |
| `src-tauri/src/settings.rs` | Output-folder preference |
| `src-tauri/tauri.conf.json` | Window, CSP, bundle and updater config |
| `src-tauri/capabilities/` | What the webview is allowed to call |
| `src-tauri/tests/security.rs` | Security configuration tests |
| `ui/` | Frontend (plain HTML/CSS/JS, no build step) |
| `ui/tests/` | UI unit and static security tests |
| `.github/workflows/` | CI (tests on every push) and release builds |
