# 🌱 SW3 Images

Smart, visually-lossless image compression for the web, written in Rust.
Upload JPG, PNG or WebP images and get back dramatically smaller files with
no visible quality loss — ready for publishing on the internet.

Made by [Sustainable WWW Sweden AB](https://sustainablewww.se) and
[Michael Andersen](https://michaelandersen.net).

## Running locally

```sh
cargo run --release
```

Then open **http://localhost:2500**.

## Configuration — the `.env` file

Create a file called `.env` **in the project root** (same folder as
`Cargo.toml`; copy [`.env.example`](.env.example) to get started). It is
loaded automatically at startup and must never be committed to git
(`.gitignore` already excludes it).

```ini
# Brevo lead notifications (both required to enable emails)
BREVO_API_KEY=xkeysib-your-key-here
LEAD_NOTIFY_EMAIL=info@sustainablewww.org
# Optional: verified Brevo sender; defaults to LEAD_NOTIFY_EMAIL
LEAD_FROM_EMAIL=info@sustainablewww.org
```

With those set, every time someone unlocks the 50-file limit you get an email
("🌱 New SW3 Images lead: …") with their name, email, and how many files they
compressed. The API key comes from Brevo → SMTP & API → API Keys, and the
sender address must be a verified sender in your Brevo account. If the
variables are missing, notifications are simply disabled — leads are still
always appended to `data/leads.jsonl`. Emails are sent fire-and-forget: a
Brevo outage can never fail or slow down an upload.

All supported variables:

| Env var | Default | Purpose |
| --- | --- | --- |
| `BREVO_API_KEY` | unset | Brevo v3 API key for lead notification emails |
| `LEAD_NOTIFY_EMAIL` | unset | Recipient of lead notifications |
| `LEAD_FROM_EMAIL` | `LEAD_NOTIFY_EMAIL` | Verified Brevo sender address |
| `PORT` | `2500` | HTTP port |
| `SW3_DATA` | `./data` | Storage directory (jobs + leads) |
| `TRUST_PROXY` | off | Set `1` behind nginx so rate limiting uses the real client IP from `X-Forwarded-For` |

## How the compression works

SW3 Images doesn't use a fixed "quality 80" knob. For every image it *measures*
perceptual quality and searches for the smallest file that still looks
identical:

1. The image is decoded and used as a perceptual reference
   (EXIF orientation is applied first, so phone photos never come out rotated).
2. Candidate encodings are compared against the reference with **DSSIM** —
   structural similarity computed in Lab color space, which models how human
   vision actually perceives error.
3. A binary search finds the lowest encoder quality whose DSSIM stays under a
   "visually lossless" ceiling. Quality is guaranteed by measurement, not by
   guesswork.
4. **Large images are judged at display scale**: anything over 1600 px on the
   long edge is compared at the size a visitor actually sees on a web page.
   Artifacts that only appear when zooming past 100% no longer block stronger
   compression — that's where the big byte savings on multi-megapixel photos
   come from, without any visible degradation at normal viewing size.
5. **Probe search keeps CPU low**: for images with 2×+ the judge pixels, the
   quality search runs on the small judge-scale copy (scored through a
   simulation of the display downscale), and only 2-4 full-resolution encodes
   verify and fine-tune the result — measured ~2.5× faster on a 12 MP photo
   with byte-identical output.

Per format:

- **JPEG** — re-encoded with **mozjpeg** (trellis quantization, progressive
  scans, optimized Huffman tables). Grayscale photos are detected and encoded
  single-channel. If 4:2:0 chroma subsampling would visibly blur fine color
  detail (red text, logos), SW3 Images automatically falls back to 4:4:4.
- **PNG** — two candidates race: lossless recompression with **oxipng**, and
  palette quantization with **imagequant** (the pngquant algorithm). The
  quantized version is only used if DSSIM confirms it is visually identical;
  the smaller winner ships.
- **WebP** — lossy quality search via libwebp; graphics with few colors also
  get a lossless candidate, and the smaller file wins.

If no candidate beats the original file size, the original bytes are returned
untouched — SW3 Images never inflates a file. Metadata (EXIF, GPS location…) is
stripped, which shrinks files further and is kinder to visitors' privacy.

There's also a CLI for quick testing/benchmarking:

```sh
cargo run --release --example compress_file -- photo.jpg logo.png
```

## Features

- Drag & drop up to **10 images** (30 MB each) — or **50** after leaving your
  name and email (captured as leads, see below).
- Live per-file progress, before/after sizes and savings percentage.
- Optional **resize to max width** (aspect ratio preserved, never upscales)
  before compressing — for even bigger savings when full resolution isn't
  needed.
- Download files individually or all at once as a zip.
- Results are kept on disk for **24 hours**, then deleted automatically
  (sweep runs every 15 minutes; covered by tests).
- Estimated yearly CO₂e savings shown after compression.
- Same format in, same format out — no surprise conversions.

## Leads

When someone unlocks the 50-file limit, a line is appended to
`data/leads.jsonl`:

```json
{"ts":1784659005,"name":"Test User","email":"test@example.com","files":11,"job_id":"49b1a3e0-…"}
```

One JSON object per line — easy to import anywhere (`jq -s` turns it into an
array, or open it in any spreadsheet tool). The retention sweep never touches
this file. Remember to mention the sign-up in your privacy policy (GDPR).

## Abuse protection

- **Rate limiting** — max 10 compression jobs per IP per 15 minutes
  (sliding window, in-memory). Behind nginx, set `TRUST_PROXY=1` so the real
  client IP from `X-Forwarded-For` is used.
- **Honeypot** — an invisible `website` form field; bots that autofill it are
  rejected.
- **Content sniffing** — files are identified by magic bytes, not filename;
  anything that isn't a real JPG/PNG/WebP is rejected before decoding.
- Uploaded content is never public: results are reachable only via an
  unguessable UUID link, and everything is deleted within 24 hours.

If you later need stronger bot protection, [Cloudflare
Turnstile](https://developers.cloudflare.com/turnstile/) in invisible mode is
the natural next step (free tier, no user interaction).

## Deploying on a Raspberry Pi (or any Linux server) with PM2

Step by step, from a fresh Pi. Build **on the Pi** — a native ARM build is
the least-friction path.

**1. Install the build dependencies** (one time):

```sh
sudo apt update
sudo apt install -y build-essential git      # C compiler for mozjpeg/libwebp + git
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # Rust toolchain
source "$HOME/.cargo/env"
```

**2. Clone and build** (Rust dependencies are fetched automatically by cargo):

```sh
git clone https://github.com/<you>/sw3-images.git
cd sw3-images
cargo build --release                        # grab a coffee on a Pi 4 (~15-30 min)
```

**3. Create your `.env`** in the project root (see Configuration above):

```sh
cp .env.example .env
nano .env                                    # fill in the Brevo key + emails
```

**4. Start it with PM2** — runs on port 2500 and **restarts automatically if
it crashes** (`autorestart` is set in the config, plus a memory ceiling so a
runaway job can't take down the Pi):

```sh
sudo apt install -y nodejs npm && sudo npm install -g pm2   # if PM2 isn't installed yet
pm2 start ecosystem.config.cjs               # runs ./target/release/sw3-images on :2500
pm2 save                                     # remember the process list
pm2 startup                                  # print the command that enables start-on-boot — run it
```

Useful afterwards: `pm2 status`, `pm2 logs sw3-images`, and after deploying a
new build `git pull && cargo build --release && pm2 restart sw3-images`.

Then put nginx in front for the subdomain + TLS — a ready-made config with
the required `client_max_body_size` and proxy timeouts is in
[`deploy/nginx-sw3-images.conf`](deploy/nginx-sw3-images.conf):

```sh
sudo cp deploy/nginx-sw3-images.conf /etc/nginx/sites-available/sw3-images
sudo ln -s /etc/nginx/sites-available/sw3-images /etc/nginx/sites-enabled/
sudo certbot --nginx -d images.sustainablewww.se
sudo nginx -t && sudo systemctl reload nginx
```

`ecosystem.config.cjs` sets `TRUST_PROXY=1` and caps memory at 700 MB so a
runaway job can't take down the Pi. A Pi 4/5 handles typical batches fine;
compression is CPU-bound, so expect a large photo to take a few seconds.

## Project layout

| Path | Purpose |
| --- | --- |
| `src/server.rs` | Axum web server: upload, polling, downloads, rate limiting, retention |
| `src/compress.rs` | The compression engine (DSSIM-guided quality search) |
| `src/jobs.rs` | Job registry, on-disk layout, 24 h cleanup |
| `src/main.rs` | Thin binary entry point |
| `static/index.html` | The whole frontend (embedded into the binary) |
| `tests/api.rs` | HTTP integration tests against the real router |
| `ecosystem.config.cjs` | PM2 process config for the Raspberry Pi |
| `deploy/nginx-sw3-images.conf` | nginx reverse-proxy sample for the subdomain |
| `data/jobs/<id>/` | Per-job storage: `meta.json`, `out/` results, lazy zip |
| `data/leads.jsonl` | Name/email lead capture (never auto-deleted) |

## API

| Endpoint | Description |
| --- | --- |
| `POST /api/jobs` | Multipart upload (`file` ×N, optional `name` + `email`) → job JSON |
| `GET /api/jobs/{id}` | Job status: per-file progress, sizes, notes |
| `GET /api/jobs/{id}/files/{idx}` | Download one compressed file |
| `GET /api/jobs/{id}/zip` | Download everything as `sw3-images-compressed.zip` |

## Tests

```sh
cargo test
```

29 tests: unit coverage of the engine (visual-losslessness re-verified on
output, shrink behavior for all three formats, EXIF orientation, display-scale
judging, format sniffing, heuristics), the job registry (state machine,
restart recovery, **24 h retention deletion**), and full HTTP integration
tests (upload → compress → poll → download → zip, file limits, email unlock +
lead capture, honeypot, rate limiting, invalid input).

## Notes for future work

Planned: user accounts + database for compression history, subscriptions, and
a public API. The `data/jobs` layout and JSON endpoints were designed so a
database can slot in without changing the engine.
