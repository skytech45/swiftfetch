#![allow(clippy::unwrap_used, clippy::expect_used)] // integration tests may panic

//! Milestone 5 — site grabber acceptance: a local fixture site with 50 pages,
//! mixed files and a `robots.txt` disallowing `/private/`.
//!
//! Asserts: only allowed files are collected, depth + filters are honored,
//! politeness delays appear, and a scheduled re-grab picks up a newly added
//! file.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use swiftfetch_grabber::{GrabConfig, crawl};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type Pages = Arc<Mutex<HashMap<String, (u16, String, String)>>>;

async fn fixture_server(pages: Pages) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let pages = Arc::clone(&pages);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let Ok(n) = sock.read(&mut buf).await else {
                    return;
                };
                let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                let path = req
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_owned();
                let (status, ctype, body) = pages.lock().unwrap().get(&path).cloned().unwrap_or((
                    404,
                    "text/plain".to_owned(),
                    "nope".to_owned(),
                ));
                let head = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(body.as_bytes()).await;
            });
        }
    });
    format!("http://{addr}")
}

fn seed_pages(n: u32) -> HashMap<String, (u16, String, String)> {
    let mut map = HashMap::new();
    map.insert(
        "/robots.txt".to_owned(),
        (
            200,
            "text/plain".to_owned(),
            "User-agent: *\nDisallow: /private/\n".to_owned(),
        ),
    );
    for i in 0..n {
        let next = if i + 1 < n {
            format!("<a href=\"/page{}\">next</a>", i + 1)
        } else {
            String::new()
        };
        // Even pages link a public zip; every page links a private zip
        // (must be excluded via robots.txt) and an exe (filter test).
        let body = format!(
            "<html><body>page{i}{next} \
             <a href=\"/files/public{i}.zip\">zip</a> \
             <a href=\"/private/secret{i}.zip\">secret</a> \
             <a href=\"/files/tool{i}.exe\">exe</a></body></html>"
        );
        map.insert(
            if i == 0 {
                "/".to_owned()
            } else {
                format!("/page{i}")
            },
            (200, "text/html".to_owned(), body),
        );
        map.insert(
            format!("/files/public{i}.zip"),
            (200, "application/zip".to_owned(), "ZIPDATA".to_owned()),
        );
    }
    map
}

#[tokio::test]
async fn grabber_honors_robots_depth_and_filters() {
    let pages: Pages = Arc::new(Mutex::new(seed_pages(50)));
    let base = fixture_server(Arc::clone(&pages)).await;

    let mut config = GrabConfig::new(format!("{base}/"));
    config.max_pages = 50;
    config.max_files = 200;
    config.include_exts = vec!["zip".to_owned()];
    config.politeness = Duration::from_millis(20);

    let start = Instant::now();
    let report = crawl(config).await.expect("crawl succeeds");
    let elapsed = start.elapsed();

    // No private URL may leak through robots.txt.
    assert!(
        report.files.iter().all(|f| !f.url.contains("/private/")),
        "robots.txt /private/ must be excluded, got {:?}",
        report.files.iter().map(|f| &f.url).collect::<Vec<_>>()
    );
    // Include filter: only zips.
    assert!(
        report.files.iter().all(|f| f.extension == "zip"),
        "only .zip files expected"
    );
    // Depth 2 from the seed over a linear chain visits 3 pages.
    assert_eq!(
        report.pages_visited, 3,
        "linear chain depth=2 visits 3 pages"
    );
    assert!(report.skipped_robots >= 3, "private links must be counted");
    // Politeness: at least (pages + robots fetch is same host) delays applied.
    assert!(
        report.politeness_applied >= Duration::from_millis(20),
        "politeness delay must be applied"
    );
    assert!(
        elapsed >= Duration::from_millis(20),
        "wall time must include politeness"
    );
}

#[tokio::test]
async fn grabber_regrab_picks_up_new_file() {
    let pages: Pages = Arc::new(Mutex::new(seed_pages(5)));
    let base = fixture_server(Arc::clone(&pages)).await;

    let mut config = GrabConfig::new(format!("{base}/"));
    config.max_pages = 10;
    config.politeness = Duration::ZERO;
    let first = crawl(config.clone()).await.expect("first crawl works");
    let first_count = first.files.len();

    // Simulate the site adding a file + a link to it (scheduled re-grab).
    {
        let mut guard = pages.lock().unwrap();
        guard.insert(
            "/files/bonus.zip".to_owned(),
            (200, "application/zip".to_owned(), "ZIPDATA".to_owned()),
        );
        let home = guard.get("/").cloned().unwrap();
        guard.insert(
            "/".to_owned(),
            (
                home.0,
                home.1,
                format!("{} <a href=\"/files/bonus.zip\">bonus</a>", home.2),
            ),
        );
    }
    let second = crawl(config).await.expect("second crawl works");
    assert!(
        second.files.len() > first_count,
        "re-grab must pick up the new file"
    );
    assert!(
        second
            .files
            .iter()
            .any(|f| f.url.ends_with("/files/bonus.zip")),
        "bonus.zip must be found"
    );
}
