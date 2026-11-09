# 🌱 SW3 Images

Smart image compression for a lighter, faster and more sustainable web.
SW3 Images makes JPG, PNG and WebP images dramatically smaller without
visible loss in quality or color, and can resize them and convert them to
WebP at the same time. Everything runs on your own computer.

Made by [Sustainable WWW Sweden AB](https://sustainablewww.se) and
[Michael Andersen](https://michaelandersen.net). Free and open source (MIT).

## Download and install

Get the latest version from
[Releases](https://github.com/Sustainable-WWW/SW3-Images-Desktop/releases/latest).

| Platform | Requirements | File |
| --- | --- | --- |
| macOS (Apple Silicon) | macOS 11 (Big Sur) or later, Apple Silicon (M1 or newer) | `SW3 Images_x.y.z_aarch64.dmg` |
| macOS (Intel) | macOS 11 (Big Sur) or later, Intel processor | `SW3 Images_x.y.z_x64.dmg` |
| Windows | Windows 10 or 11, 64-bit (x64) | `SW3 Images_x.y.z_x64-setup.exe` |

Not sure which Mac you have? **Apple menu → About This Mac** shows **Chip**
(Apple Silicon) or **Processor** (Intel).

**macOS:** open the `.dmg` and drag SW3 Images to Applications. The app is
not notarized by Apple, so the first launch shows a warning. Right-click the
app and choose **Open**, or go to **System Settings → Privacy & Security →
Open Anyway**. This is only needed once.

**Windows:** run the `-setup.exe` (installs per user, no admin rights
needed). If SmartScreen warns, click **More info → Run anyway**. On the rare
Windows 10 PC without Microsoft's WebView2 component, the installer
downloads it from Microsoft.

Installed apps update themselves: SW3 Images checks for a new version at
launch (and via **Check for Updates…** in the menu) and offers to install it.

## Features

- **No file limit.** Drop as many images or whole folders as you like.
- **Up to 5 images at once**, with a memory budget sized to your computer's
  RAM so big batches never exhaust memory.
- **Resize** to a maximum width. Aspect ratio is kept, and images are never
  upscaled.
- **Convert to WebP** together with resizing and compression. Transparency
  and color profiles are kept.
- **Automatic output folder.** Results go to `Pictures/SW3 Compressed` (you
  can change this in the app). Originals are never modified, and existing
  files are never overwritten.
- **Native notification** when a batch finishes, plus progress in the Dock
  or taskbar.
- **Automatic updates** from GitHub, with signature verification.

## How it works

SW3 Images doesn't use a fixed "quality 80" setting. It **measures** every
candidate file against the original and keeps the smallest one that passes
two checks:

1. **Detail and artifacts:** [SSIMULACRA 2](https://github.com/cloudinary/ssimulacra2),
   a perceptual metric built to tell whether a compressed image can be told
   apart from its original. The target is 80, which means "impossible to
   distinguish side by side at 1:1". It works in the XYB color space, so
   color bleeding and chroma artifacts count, not only brightness.
2. **Color accuracy:** a CIEDE2000 color-difference check on area-averaged
   colors. It catches color casts and shifted colors, such as a brand color
   drifting, which artifact metrics barely notice. Limits: overall cast ≤ 1.0,
   the worst 1% of areas ≤ 3.0, and any single area ≤ 4.0 (1.0 is about a
   just-noticeable difference).

Images are judged at the size they are seen **without zooming in**: large
photos at up to 1920 px on the long edge (a Full HD screen), smaller images
at 1:1. Artifacts that would only show when zooming into a 4000 px photo
don't stop stronger compression, and that is where most of the savings on
big photos come from.

Other details:

- **Color profiles** (Display P3 phone photos, Adobe RGB…) are carried over,
  so colors don't shift or desaturate. Plain sRGB profiles are dropped,
  since that is the default anyway.
- **JPEG** uses mozjpeg (trellis quantization, progressive). It tries 4:2:0
  chroma subsampling, and also full-resolution 4:4:4 chroma when fine colored
  detail (red text, logos) needs it, then keeps the smaller file.
  Black-and-white images are stored as single-channel JPEGs.
- **PNG** tries lossless recompression (oxipng), 16→8-bit reduction and
  palette quantization at several dithering levels. A candidate is only used
  if it passes both checks.
- **WebP** uses libwebp at maximum effort with its accurate ("sharp") color
  conversion, plus near-lossless and lossless modes for graphics and text.
- **Resizing** happens in linear light with premultiplied alpha. This avoids
  darkened fine detail, shifted colors in fine patterns, and dark fringes
  around transparent edges.
- **Same format, same size:** if nothing beats the original file, the
  original is kept. SW3 Images never makes a file bigger unless you asked
  for WebP.
- **Metadata** (EXIF, GPS location, camera data) is removed, after applying
  the EXIF orientation to the pixels.
- **Never damaged:** animated PNG/WebP files and CMYK JPEGs are kept
  byte-for-byte, because re-encoding would lose the animation or shift the
  colors. They're skipped when resizing or conversion is requested.

## Privacy

- **Your images never leave your computer.** All decoding, compression and
  analysis happens locally. Nothing is uploaded, and there is no account.
- **No tracking or telemetry.** The app collects no usage data, crash
  reports or identifiers.
- **One network request:** the update check. It downloads the public
  release manifest (`latest.json`) from this GitHub repository at launch and
  when you choose *Check for Updates…*. The request contains no information
  about you or your files beyond what any web request carries (your IP
  address is visible to GitHub, as for any download). If you accept an
  update, the installer is downloaded from GitHub and verified with a
  signature before it is installed.
- The app's web interface is locked down: it can't reach the network, can't
  read files on its own, and can't choose where files are written.
- Compressed files are saved only to the output folder you choose.

## Disclaimer

SW3 Images is provided **"as is", without warranty of any kind** (see
[LICENSE](LICENSE)). The quality checks are measurements based on models of
human vision, not guarantees: results depend on your images, how they're
displayed, and who's looking. File-size savings vary from image to image.
Always keep your original files, and check important images before you
publish them.

## License and credits

Your use of SW3 Images is governed by the [MIT License](LICENSE).
© 2026 Sustainable WWW Sweden AB and Michael Andersen.

SW3 Images includes open-source components, each under its own license. All
of them allow redistribution, and none is copyleft (GPL/AGPL). The full
notices are in [THIRD-PARTY-LICENSES.md](THIRD-PARTY-LICENSES.md) and
in the app under **Help → Open-Source Licenses**. Highlights:

- [Tauri](https://tauri.app): app framework (MIT/Apache-2.0)
- [mozjpeg](https://github.com/mozilla/mozjpeg) / libjpeg-turbo: JPEG
  encoding (IJG/BSD-3-Clause/zlib). *This software is based in part on the
  work of the Independent JPEG Group.*
- [libwebp](https://chromium.googlesource.com/webm/libwebp): WebP encoding
  (BSD-3-Clause, with Google's patent grant)
- [oxipng](https://github.com/shssoichiro/oxipng),
  [libdeflate](https://github.com/ebiggers/libdeflate),
  [zopfli](https://github.com/zopfli-rs/zopfli): PNG optimization
- [quantizr](https://github.com/DarthSim/quantizr): palette quantization (MIT)
- [SSIMULACRA 2](https://github.com/cloudinary/ssimulacra2) by Jon Sneyers
  (Cloudinary), via the Rust
  [ssimulacra2](https://github.com/rust-av/ssimulacra2) crate (BSD-2-Clause)
- [image](https://github.com/image-rs/image),
  [fast_image_resize](https://github.com/cykooz/fast_image_resize): decoding
  and resizing
- Fonts: [Poppins](https://github.com/itfoundry/Poppins) and
  [Work Sans](https://github.com/weiweihuanghuang/Work-Sans) (SIL Open Font
  License 1.1)

CI checks every dependency's license against an allowlist
([deny.toml](deny.toml)) and fails if the notices are out of date. After
changing dependencies, regenerate them with
`python3 scripts/generate-licenses.py` (requires
`cargo install cargo-about --locked --features cli`).

### Code provenance

The code in this repository was written for SW3 Images with AI assistance,
and reviewed for copied code, license and security issues before release.
One part is adapted from existing open-source code: the SSIMULACRA 2 scoring
in [`src/quality.rs`](src/quality.rs), taken from the BSD-2-Clause
`ssimulacra2` crate. It is credited in the file, and a test checks it
against the original. The CIEDE2000 color difference is implemented from
the published formula and verified against its official test data.

## Development

Requirements: Rust (stable) and Node.js 20+. On Windows you also need
[NASM](https://nasm.us) for mozjpeg's SIMD code.

```sh
npm install        # installs the Tauri CLI
npm run dev        # runs the app
npm run build      # builds installers into target/release/bundle/
```

To preview only the UI in a browser (with fake data), run
`node scripts/preview-ui.mjs` and open http://127.0.0.1:1420.

A command-line tool for testing and benchmarking the engine:

```sh
cargo run --release --example compress_file -- [--webp] [--max-width 1920] photo.jpg logo.png
```

## Tests

```sh
cargo test --workspace   # engine, color accuracy, file safety, batch pipeline, security config
npm test                 # UI helpers + static security checks
```

[CI](.github/workflows/ci.yml) runs these on **macOS and Windows for every
push and pull request**, together with clippy, cargo-deny (known
vulnerabilities, licenses, sources), an npm audit, and a check that the
license notices are up to date.

What the tests cover, among other things:

- **Quality:** every output is re-scored from scratch against its
  original. Color casts must fail. The SSIMULACRA 2 code must match the
  reference crate, and CIEDE2000 must match its published test data.
- **Color:** Display P3 profiles survive in JPEG, PNG and WebP, and plain
  sRGB profiles are dropped. A small red mark keeps a B&W photo in color.
  Fine colored detail gets 4:4:4 chroma. Resizing is linear-light with no
  fringes on transparent edges.
- **Untrusted files:** formats are detected by content, not name.
  Fake, corrupt, truncated, empty and oversized files fail cleanly, and
  oversized files are rejected from the header before any decoding.
  Malformed EXIF, ICC, PNG and WebP structures can't cause panics.
  Animated and CMYK files are never damaged.
- **Writing files:** nothing can be written outside the output folder, and
  nothing is ever overwritten, including with parallel workers or when the
  output folder is the source folder. Windows reserved names are handled.
- **App security:** strict CSP, minimal webview permissions, and the UI can
  only refer to images you added (by ID, never by path). Links are
  allowlisted, and there are no network clients besides the signed updater.
  A test also fails if the private signing key is ever committed.

## Releasing

The app checks
`https://github.com/Sustainable-WWW/SW3-Images-Desktop/releases/latest/download/latest.json`
for updates. The [release workflow](.github/workflows/release.yml) builds the
installers, signs them for the updater, and publishes everything.

**One-time setup:** add the updater signing key as a repository secret.

1. Copy the key: `pbcopy < ~/.tauri/sw3-images-updater.key`
2. GitHub → the repo → **Settings → Secrets and variables → Actions → New
   repository secret**. Name: `TAURI_SIGNING_PRIVATE_KEY`, value: paste.
   The key has no password, so `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` isn't
   needed.

> ⚠️ Back up `~/.tauri/sw3-images-updater.key`, for example in your password
> manager. If it's lost, installed apps can no longer be updated. Never
> commit it (a test guards against that).

**Each release:**

1. Bump `version` in [`src-tauri/Cargo.toml`](src-tauri/Cargo.toml), for
   example `0.2.0`, and commit.
2. Push a matching tag: `git tag v0.2.0 && git push origin main v0.2.0`

   If your `origin` is a server that mirrors to GitHub, always create tags
   locally and push them to that server. A tag made on GitHub (for example
   through its "Draft a new release" page) is deleted by the next mirror
   sync, and its release disappears with it.

The workflow runs the tests, builds the macOS `.dmg` and the Windows
`-setup.exe`, and creates the GitHub Release with the files and
`latest.json`. That takes about 20 minutes. **Don't create the release on
GitHub by hand**: the workflow creates it. And don't delete a published
version's tag: the workflow can't recreate it, so to redo a release, bump the
version and push a new tag. A hand-made release shows only
"Source code" until the builds finish, and the updater reports no update
until `latest.json` is attached.

## Project layout

| Path | Purpose |
| --- | --- |
| `src/compress.rs` | The engine: decoding, quality search, JPEG/PNG/WebP encoders |
| `src/quality.rs` | SSIMULACRA 2 with a cached reference |
| `src/color.rs` | CIEDE2000 color-accuracy check |
| `src/resize.rs` | Linear-light, premultiplied-alpha resizing |
| `src/meta.rs` | ICC profiles, animation/CMYK detection, PNG/WebP chunk editing |
| `src/files.rs` | Folder scanning, header probing, safe output naming and writing |
| `src-tauri/src/lib.rs` | App commands, the image library, drag & drop, notifications |
| `src-tauri/src/batch.rs` | Parallel workers, memory budget, cancellation, progress |
| `src-tauri/src/menu.rs` | Menu bar, About and license windows |
| `src-tauri/src/updates.rs` | GitHub-based self-updates |
| `src-tauri/tauri.conf.json` | Window, CSP, bundle and updater config |
| `src-tauri/capabilities/` | What the webview may call |
| `ui/` | The interface (plain HTML/CSS/JS, no build step) |
| `tests/ui/`, `src-tauri/tests/` | UI tests and security-configuration tests |
| `scripts/` | License generator, UI preview server and its mock backend |
| `.github/workflows/` | CI and release builds |
