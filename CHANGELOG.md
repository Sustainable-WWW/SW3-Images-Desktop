# Changelog

All notable changes to SW3 Images. The release workflow publishes each
version's section below as its GitHub release notes, which the in-app update
prompt also shows.

## [0.1.1] — 2026-09-25

A big quality update. Every image is now checked for visible changes in both
detail **and color** before it's saved, color profiles are preserved, and the
app is now fully open source under the MIT license.

### Better compression, verified quality and color

- **New quality engine.** Every compressed candidate is measured against
  your original with SSIMULACRA 2, a modern perceptual metric that also
  catches color artifacts. A file is only saved if it can't be told apart
  from the original side by side at normal viewing size (large photos are
  judged at up to 1920 px, the width of a Full HD screen; smaller images at
  full size).
- **New color check.** A second test (CIEDE2000 color difference) catches
  color casts and shifted colors, such as a brand color that drifts. Artifact
  metrics barely notice those.
- **Color profiles are kept.** Photos in Display P3 (iPhone, Mac), Adobe RGB
  and other wide-gamut color spaces keep their profile in JPEG, PNG and WebP
  output. Previously the profile was removed, which made these photos look
  duller in browsers.
- **Sharper colored details in JPEG.** For each image the engine now chooses
  between standard chroma subsampling (4:2:0) and full-resolution color
  (4:4:4), which keeps red text, logos and saturated edges crisp.
- **More accurate WebP.** WebP uses libwebp's precise ("sharp") color
  conversion, a near-lossless mode for text and graphics, and an extra
  maximum-effort pass that makes files about 9% smaller at the same quality.
- **Smaller PNGs.** A new palette quantizer is tried at three dithering
  levels, and 16-bit PNGs are reduced to 8 bits when the difference is
  invisible.
- **Better resizing.** Resizing now happens in linear light with
  premultiplied alpha: no more dark fringes around transparent edges, and no
  color shifts in fine patterns.
- **Faster.** The quality search typically needs 3–5 test encodes per image
  instead of 8.

Large photos still typically shrink 85–90% in our tests. A few images now
come out slightly larger than with 0.1.0. Those are the ones where 0.1.0
made visible changes, usually to colors.

### Reliability

- **Memory budget.** Large images now wait for free memory instead of five
  big photos at once exhausting RAM.
- **Nothing gets damaged.** Animated PNG and WebP files and CMYK JPEGs are
  kept exactly as they are. Previously an animation could be flattened to its
  first frame. They're skipped, with a clear message, when resizing or WebP
  conversion is selected.
- Images over 90 megapixels are rejected from the file header, before any
  memory is used to decode them.
- Very small images (down to 1×1 px) now compress correctly.
- EXIF orientation (rotated phone photos) is now applied to PNG and WebP
  files too.
- Fixed: color profiles written into JPEG files were ignored by browsers.
- Files that get bigger when converted to WebP now show "+x%" instead of
  "Already optimal".

### Privacy and security

- Your images never leave your computer, and the app collects no usage
  data. The only network request is the update check, which reads the
  release list from GitHub.
- Hardened internals: the interface can only refer to images you added
  (dropped files are handled natively), and its content security policy is
  stricter.
- An update can no longer be installed while a batch is running.
- Clearer update messages: "You're on the latest version" instead of
  "Could not fetch a valid release json", and a plain message when you're
  offline.

### Open source and licensing

- SW3 Images is now fully licensed under the **MIT License** (see LICENSE).
  Two components under copyleft licenses (DSSIM, AGPL-3.0; libimagequant,
  GPL-3.0) were replaced with permissive alternatives (SSIMULACRA 2,
  BSD-2-Clause; quantizr, MIT).
- Full third-party license notices are included in the repository and in
  the app: **Help → Open-Source Licenses**, or the link in the About window.
- The About window now includes a privacy statement and an "as is"
  disclaimer.

### For developers

- The release workflow checks for the updater signing key before building,
  and takes its release notes from this changelog.
- CI now also runs cargo-deny (known vulnerabilities and a license
  allowlist), checks that the license notices are up to date, and uses the
  current GitHub Actions versions.
- 87 automated tests covering quality, color, file safety, security
  configuration and the UI.
- Test and design-preview files are no longer bundled into the app.

## [0.1.0] — 2026-09-25

First desktop release of SW3 Images for macOS (Apple Silicon) and Windows.

- Compress JPG, PNG and WebP images without visible quality loss.
- No file limit: drop images or whole folders.
- Up to 5 images compressed at the same time.
- Resize to a maximum width and convert to WebP.
- Results saved automatically to `Pictures/SW3 Compressed`.
- Native notification when a batch finishes.
- Automatic updates from GitHub Releases.
