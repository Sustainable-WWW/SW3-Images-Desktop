#!/usr/bin/env python3
"""Generate the third-party license notices shipped with SW3 Images.

Writes THIRD-PARTY-LICENSES.md (repository) and ui/licenses.html (shown in
the app under Help → Open-Source Licenses) from:

  * cargo-about: every Rust crate compiled into the app, with its license
    text (install: cargo install cargo-about --locked --features cli);
  * C libraries bundled inside those crates, whose own notices differ from
    the wrapper crate's license (libwebp incl. its patent grant, libdeflate);
  * the bundled fonts (SIL Open Font License).

Run from the repository root:  python3 scripts/generate-licenses.py
Re-run whenever dependencies change (CI checks that the files are current).
"""

import html
import json
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
TARGETS = ["aarch64-apple-darwin", "x86_64-apple-darwin", "x86_64-pc-windows-msvc"]

IJG_STATEMENT = "This software is based in part on the work of the Independent JPEG Group."


def run(*args):
    return subprocess.run(args, cwd=ROOT, check=True, capture_output=True, text=True).stdout


def crate_dirs():
    dirs = {}
    for target in TARGETS:
        meta = json.loads(run("cargo", "metadata", "--format-version", "1", "--filter-platform", target))
        for p in meta["packages"]:
            dirs[p["name"]] = pathlib.Path(p["manifest_path"]).parent
    return dirs


def main():
    about = json.loads(run("cargo", "about", "generate", "--workspace", "--format", "json"))
    dirs = crate_dirs()

    sections = []  # (title, used_by, text)
    # Group crates by identical license text (keeps each distinct copyright line).
    for lic in about["licenses"]:
        crates = sorted({f"{u['crate']['name']} {u['crate']['version']}" for u in lic["used_by"]})
        crates = [c for c in crates if not c.startswith(("sw3-images ", "sw3-images-desktop "))]
        if crates:
            sections.append((f"{lic['name']} ({lic['id']})", crates, lic["text"].strip()))
    sections.sort(key=lambda s: (s[0], s[1]))

    extra = []
    webp = dirs["libwebp-sys"] / "vendor"
    extra.append((
        "libwebp — WebP encoder/decoder (bundled C library)",
        ["libwebp (via libwebp-sys)"],
        (webp / "COPYING").read_text().strip() + "\n\n" + (webp / "PATENTS").read_text().strip(),
    ))
    extra.append((
        "libdeflate — DEFLATE compression (bundled C library)",
        ["libdeflate (via libdeflate-sys)"],
        (dirs["libdeflate-sys"] / "libdeflate" / "COPYING").read_text().strip(),
    ))
    extra.append((
        "mozjpeg / libjpeg-turbo / IJG libjpeg — JPEG encoder (bundled C library)",
        ["mozjpeg (via mozjpeg-sys)"],
        IJG_STATEMENT + "\n\n" + (dirs["mozjpeg-sys"] / "LICENSE").read_text().strip(),
    ))
    for font, file in [("Poppins", "OFL-Poppins.txt"), ("Work Sans", "OFL-WorkSans.txt")]:
        extra.append((f"{font} font (SIL Open Font License 1.1)", [f"{font} (ui/fonts)"], (ROOT / "ui/fonts" / file).read_text().strip()))

    intro = (
        "SW3 Images is licensed under the MIT License (see LICENSE). It includes the "
        "third-party components listed below, each under its own license. "
        + IJG_STATEMENT
        + " Components under the Mozilla Public License 2.0 are used unmodified; their "
        "source code is available from crates.io under the names and versions listed."
    )

    md = ["# Third-party licenses", "", intro, ""]
    for title, used_by, text in extra + sections:
        md += [f"## {title}", "", "Used by: " + ", ".join(used_by), "", "```", text, "```", ""]
    (ROOT / "THIRD-PARTY-LICENSES.md").write_text("\n".join(md))

    body = [f"<h1>Open-source licenses</h1><p>{html.escape(intro)}</p>"]
    for title, used_by, text in extra + sections:
        body.append(
            f"<section><h2>{html.escape(title)}</h2><p class=\"used\">{html.escape(', '.join(used_by))}</p>"
            f"<pre>{html.escape(text)}</pre></section>"
        )
    page = (
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n"
        "<title>Open-Source Licenses</title>\n<link rel=\"stylesheet\" href=\"styles.css\">\n"
        "<link rel=\"stylesheet\" href=\"about.css\">\n</head>\n<body class=\"licenses\">\n<main>\n"
        + "\n".join(body)
        + "\n</main>\n</body>\n</html>\n"
    )
    (ROOT / "ui/licenses.html").write_text(page)
    print(f"{len(sections)} crate license texts + {len(extra)} bundled components")


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as e:
        sys.exit(f"command failed: {' '.join(e.cmd)}\n{e.stderr}")
