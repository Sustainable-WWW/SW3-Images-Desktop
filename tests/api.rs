//! Integration tests driving the real Axum router end to end:
//! upload -> compress -> poll -> download -> zip, plus limits and abuse guards.

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use axum::Router;
use sw3_images::server::{app, new_state};
use http_body_util::BodyExt;
use std::net::SocketAddr;
use std::path::PathBuf;
use tower::ServiceExt;

const BOUNDARY: &str = "EcoPixelTestBoundary";

fn test_app(tag: &str) -> (Router, PathBuf) {
    let dir = std::env::temp_dir().join(format!("ecopixel-api-{tag}-{}", uuid()));
    (app(new_state(dir.clone())), dir)
}

fn uuid() -> String {
    format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())
}

struct Part<'a> {
    name: &'a str,
    filename: Option<&'a str>,
    content_type: Option<&'a str>,
    data: Vec<u8>,
}

fn text_part<'a>(name: &'a str, value: &str) -> Part<'a> {
    Part { name, filename: None, content_type: None, data: value.as_bytes().to_vec() }
}

fn file_part<'a>(name: &'a str, filename: &'a str, content_type: &'a str, data: Vec<u8>) -> Part<'a> {
    Part { name, filename: Some(filename), content_type: Some(content_type), data }
}

fn multipart_body(parts: &[Part]) -> Vec<u8> {
    let mut body = Vec::new();
    for p in parts {
        body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        match p.filename {
            Some(f) => body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\n", p.name, f).as_bytes(),
            ),
            None => body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{}\"\r\n", p.name).as_bytes(),
            ),
        }
        if let Some(ct) = p.content_type {
            body.extend_from_slice(format!("Content-Type: {ct}\r\n").as_bytes());
        }
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(&p.data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    body
}

fn post_jobs(parts: &[Part]) -> Request<Body> {
    Request::post("/api/jobs")
        .header("content-type", format!("multipart/form-data; boundary={BOUNDARY}"))
        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40000))))
        .body(Body::from(multipart_body(parts)))
        .unwrap()
}

fn get(path: &str) -> Request<Body> {
    Request::get(path)
        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40000))))
        .body(Body::empty())
        .unwrap()
}

async fn json_of(resp: axum::response::Response) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn tiny_png() -> Vec<u8> {
    let img = image::ImageBuffer::from_fn(64, 48, |x, y| image::Rgba([(x * 4) as u8, (y * 5) as u8, 120u8, 255]));
    let mut buf = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut buf, image::ImageFormat::Png)
        .unwrap();
    buf.into_inner()
}

fn tiny_jpeg() -> Vec<u8> {
    let img = image::ImageBuffer::from_fn(64, 48, |x, y| image::Rgb([(x * 4) as u8, 90u8, (y * 5) as u8]));
    let mut buf = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut buf, image::ImageFormat::Jpeg)
        .unwrap();
    buf.into_inner()
}

async fn wait_done(app: &Router, job_id: &str) -> serde_json::Value {
    for _ in 0..300 {
        let resp = app.clone().oneshot(get(&format!("/api/jobs/{job_id}"))).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let status = json_of(resp).await;
        if status["done"] == true {
            return status;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("job {job_id} did not finish in time");
}

#[tokio::test]
async fn index_serves_the_frontend() {
    let (app, dir) = test_app("index");
    let resp = app.oneshot(get("/")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8_lossy(&body);
    assert!(html.contains("SW3 Images"));
    assert!(html.contains("sustainablewww.se"));
    assert!(html.contains("michaelandersen.net"));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn full_pipeline_upload_compress_download_zip() {
    let (app, dir) = test_app("pipeline");

    let resp = app
        .clone()
        .oneshot(post_jobs(&[
            file_part("file", "a.png", "image/png", tiny_png()),
            file_part("file", "b.jpg", "image/jpeg", tiny_jpeg()),
        ]))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let job = json_of(resp).await;
    let job_id = job["job_id"].as_str().unwrap().to_string();
    assert_eq!(job["files"].as_array().unwrap().len(), 2);

    let status = wait_done(&app, &job_id).await;
    for f in status["files"].as_array().unwrap() {
        assert_eq!(f["status"], "done", "file failed: {f}");
        assert!(f["out_size"].as_u64().unwrap() > 0);
    }

    // Individual download with correct headers and valid image bytes.
    let resp = app.clone().oneshot(get(&format!("/api/jobs/{job_id}/files/0"))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["content-type"], "image/png");
    assert!(resp.headers()["content-disposition"]
        .to_str()
        .unwrap()
        .contains("a.min.png"));
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    image::load_from_memory(&bytes).expect("downloaded file must be a valid image");

    // Zip download containing both files.
    let resp = app.clone().oneshot(get(&format!("/api/jobs/{job_id}/zip"))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..2], b"PK", "zip magic");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn eleven_files_require_contact_details() {
    let (app, dir) = test_app("limit");
    let png = tiny_png();

    let parts: Vec<Part> = (0..11).map(|_| file_part("file", "x.png", "image/png", png.clone())).collect();
    let resp = app.clone().oneshot(post_jobs(&parts)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(json_of(resp).await["error"].as_str().unwrap().contains("limit is 10"));

    // Same 11 files WITH name+email: accepted, and the lead is recorded.
    let mut parts: Vec<Part> = vec![text_part("name", "Test Person"), text_part("email", "lead@example.com")];
    parts.extend((0..11).map(|_| file_part("file", "x.png", "image/png", png.clone())));
    let resp = app.clone().oneshot(post_jobs(&parts)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let leads = std::fs::read_to_string(dir.join("leads.jsonl")).expect("leads.jsonl must exist");
    assert!(leads.contains("lead@example.com"));
    assert!(leads.contains("\"files\":11"));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn honeypot_field_blocks_bots() {
    let (app, dir) = test_app("honeypot");
    let resp = app
        .oneshot(post_jobs(&[
            text_part("website", "https://spam.example"),
            file_part("file", "a.png", "image/png", tiny_png()),
        ]))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn non_images_are_rejected() {
    let (app, dir) = test_app("badfile");
    let resp = app
        .oneshot(post_jobs(&[file_part("file", "evil.jpg", "image/jpeg", b"#!/bin/sh\necho pwned".to_vec())]))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn unknown_job_is_404() {
    let (app, dir) = test_app("missing");
    let resp = app.oneshot(get("/api/jobs/00000000-0000-0000-0000-000000000000")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn max_width_resizes_the_output() {
    let (app, dir) = test_app("resize");
    let resp = app
        .clone()
        .oneshot(post_jobs(&[
            text_part("max_width", "32"),
            file_part("file", "a.png", "image/png", tiny_png()),
        ]))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let job = json_of(resp).await;
    let job_id = job["job_id"].as_str().unwrap().to_string();
    wait_done(&app, &job_id).await;

    let resp = app.clone().oneshot(get(&format!("/api/jobs/{job_id}/files/0"))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let img = image::load_from_memory(&bytes).unwrap();
    assert_eq!((img.width(), img.height()), (32, 24), "64x48 resized to max width 32");

    // Invalid widths are rejected up front.
    let resp = app
        .clone()
        .oneshot(post_jobs(&[
            text_part("max_width", "nonsense"),
            file_part("file", "a.png", "image/png", tiny_png()),
        ]))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn rate_limit_kicks_in_after_ten_jobs() {
    let (app, dir) = test_app("ratelimit");
    let png = tiny_png();
    let mut last = StatusCode::OK;
    for _ in 0..11 {
        let resp = app
            .clone()
            .oneshot(post_jobs(&[file_part("file", "a.png", "image/png", png.clone())]))
            .await
            .unwrap();
        last = resp.status();
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
    let _ = std::fs::remove_dir_all(dir);
}
