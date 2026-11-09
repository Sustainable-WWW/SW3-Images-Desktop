//! Guards the app's security configuration so a careless edit can't silently
//! widen what the webview may do, weaken the CSP, or disable update signing.

use serde_json::Value;

fn load(rel: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn csp(conf: &Value) -> serde_json::Map<String, Value> {
    conf["app"]["security"]["csp"].as_object().expect("CSP must be configured").clone()
}

#[test]
fn csp_is_strict() {
    let conf = load("tauri.conf.json");
    let csp = csp(&conf);
    assert_eq!(csp["default-src"], "'self'");
    assert_eq!(csp["script-src"], "'self'", "no inline or remote scripts");
    assert_eq!(csp["object-src"], "'none'");
    assert_eq!(csp["frame-ancestors"], "'none'");
    for (directive, value) in &csp {
        let v = value.as_str().unwrap();
        assert!(!v.contains("unsafe-eval"), "{directive} allows eval");
        assert!(!v.contains("unsafe-inline"), "{directive} allows inline code");
        assert!(!v.contains("http:") && !v.contains("https:") || directive == "connect-src", "{directive} allows remote origins");
        assert!(!v.split_whitespace().any(|t| t == "*"), "{directive} allows everything");
    }
    assert_eq!(csp["img-src"], "'self'", "no data: or remote images");
    let connect = csp["connect-src"].as_str().unwrap();
    for token in connect.split_whitespace() {
        assert!(token == "ipc:" || token == "http://ipc.localhost", "connect-src may only reach Tauri IPC, got {token}");
    }
    assert_eq!(conf["app"]["security"]["freezePrototype"], true);
    assert!(conf["app"]["security"].get("dangerousDisableAssetCspModification").is_none());
    assert!(conf["app"]["security"].get("assetProtocol").is_none(), "asset protocol would expose the file system");
}

#[test]
fn webview_permissions_are_minimal() {
    let cap = load("capabilities/default.json");
    assert_eq!(cap["windows"], serde_json::json!(["main", "about"]));
    let perms: Vec<&str> = cap["permissions"].as_array().unwrap().iter().map(|p| p.as_str().expect("plain permission ids only")).collect();
    let allowed = ["core:default", "core:window:allow-start-dragging"];
    for p in &perms {
        assert!(allowed.contains(p), "unexpected webview permission {p}: file system, shell, dialog, updater and notification access must stay in Rust");
    }
    assert!(cap.get("remote").is_none(), "remote URLs must never get IPC access");
    assert!(
        !cap["windows"].as_array().unwrap().iter().any(|w| w == "licenses"),
        "the static licenses window needs no IPC access"
    );

    // No other capability files may sneak in extra permissions.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("capabilities");
    let files: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(files.len(), 1, "only capabilities/default.json is expected, found {files:?}");
}

/// Dropped files are only accepted in Rust. Tauri reports a drop on a
/// window's own webview as a `WindowEvent` (`WebviewEvent::DragDrop` only
/// fires for child webviews), so a handler registered anywhere else never
/// runs: the drop overlay still shows, but no images get added.
#[test]
fn dropped_files_reach_the_rust_handler() {
    let lib = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs")).unwrap();
    assert!(
        lib.contains(".on_window_event(") && lib.contains("WindowEvent::DragDrop(DragDropEvent::Drop"),
        "handle drops with Builder::on_window_event and WindowEvent::DragDrop"
    );
}

#[test]
fn updates_are_signed_and_come_from_github_over_https() {
    let conf = load("tauri.conf.json");
    let up = &conf["plugins"]["updater"];
    let pubkey = up["pubkey"].as_str().unwrap();
    assert!(pubkey.len() > 40, "updater public key missing");
    let endpoints = up["endpoints"].as_array().unwrap();
    assert!(!endpoints.is_empty());
    for e in endpoints {
        let e = e.as_str().unwrap();
        assert!(
            e.starts_with("https://github.com/Sustainable-WWW/SW3-Images-Desktop/releases/"),
            "unexpected update endpoint {e}"
        );
    }
    assert!(up.get("dangerousInsecureTransportProtocol").is_none());
    assert_eq!(conf["bundle"]["createUpdaterArtifacts"], true);
}

#[test]
fn bundle_targets_mac_and_windows() {
    let conf = load("tauri.conf.json");
    let targets: Vec<&str> = conf["bundle"]["targets"].as_array().unwrap().iter().map(|t| t.as_str().unwrap()).collect();
    for t in ["app", "dmg", "nsis"] {
        assert!(targets.contains(&t), "missing bundle target {t}");
    }
    // Ad-hoc signature: required for Apple Silicon to launch the app at all.
    assert_eq!(conf["bundle"]["macOS"]["signingIdentity"], "-");
    assert_eq!(conf["identifier"], "se.sustainablewww.sw3images");
}

#[test]
fn no_private_key_is_committed() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    fn walk(dir: &std::path::Path, hits: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if matches!(name.as_str(), "target" | "node_modules" | ".git") {
                continue;
            }
            if p.is_dir() {
                walk(&p, hits);
            } else if name.ends_with(".key") {
                hits.push(p.display().to_string());
            } else if p.metadata().map(|m| m.len() < 1_000_000).unwrap_or(false) {
                if let Ok(s) = std::fs::read_to_string(&p) {
                    // Tauri stores keys base64-encoded; check both forms.
                    // (Needles are split so this file doesn't match itself.)
                    let plain = ["untrusted comment: rsign ", "encrypted secret key"].concat();
                    let b64 = ["dW50cnVzdGVkIGNvbW1lbnQ6IHJzaWdu", "IGVuY3J5cHRlZCBzZWNyZXQga2V5"].concat();
                    if s.contains(&plain) || s.contains(&b64) {
                        hits.push(p.display().to_string());
                    }
                }
            }
        }
    }
    let mut hits = Vec::new();
    walk(root, &mut hits);
    assert!(hits.is_empty(), "private signing key material found in the repo: {hits:?}");
}

/// Privacy promise: images never leave the computer. The engine and the app
/// crate must not pull in an HTTP client of their own — the only network
/// code is Tauri's updater plugin, which fetches the release manifest.
#[test]
fn no_network_clients_besides_the_updater() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for manifest in [manifest_dir.join("Cargo.toml"), manifest_dir.parent().unwrap().join("Cargo.toml")] {
        let text = std::fs::read_to_string(&manifest).unwrap();
        let deps: Vec<&str> = text
            .lines()
            .skip_while(|l| !l.starts_with("[dependencies]"))
            .skip(1)
            .take_while(|l| !l.starts_with('['))
            .filter_map(|l| l.split('=').next())
            .map(str::trim)
            .filter(|n| !n.is_empty() && !n.starts_with('#'))
            .collect();
        for banned in ["reqwest", "hyper", "ureq", "isahc", "curl", "surf", "attohttpc", "tauri-plugin-http", "tauri-plugin-upload"] {
            assert!(!deps.contains(&banned), "{} depends on network client {banned}", manifest.display());
        }
    }
    let conf = load("tauri.conf.json");
    let plugins: Vec<&String> = conf["plugins"].as_object().unwrap().keys().collect();
    assert_eq!(plugins, vec!["updater"], "only the updater plugin is configured");
}

#[test]
fn license_files_are_present_and_shipped() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let license = std::fs::read_to_string(root.join("LICENSE")).unwrap();
    assert!(license.starts_with("MIT License"), "own code is MIT-licensed");
    let notices = std::fs::read_to_string(root.join("THIRD-PARTY-LICENSES.md")).unwrap();
    for must in ["libwebp", "Additional IP Rights Grant", "libdeflate", "Independent JPEG Group", "SIL Open Font License", "ssimulacra2", "quantizr"] {
        assert!(notices.contains(must), "third-party notices must cover {must}");
    }
    // License section titles (full texts may mention the GPL, e.g. MPL-2.0's
    // compatibility clause).
    for title in notices.lines().filter(|l| l.starts_with("## ")) {
        assert!(!title.contains("GPL") && !title.contains("General Public License"), "copyleft dependency crept in: {title}");
    }
    let page = std::fs::read_to_string(root.join("ui/licenses.html")).unwrap();
    assert!(page.contains("libwebp") && page.len() > 100_000, "in-app license page is generated");
}

/// A release can't be tagged without notes: the release workflow publishes
/// the CHANGELOG.md section for the app's version.
#[test]
fn changelog_documents_the_current_version() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let changelog = std::fs::read_to_string(root.join("CHANGELOG.md")).unwrap();
    let heading = format!("## [{}]", env!("CARGO_PKG_VERSION"));
    assert!(changelog.lines().any(|l| l.starts_with(&heading)), "CHANGELOG.md needs a section `{heading}`");
}
