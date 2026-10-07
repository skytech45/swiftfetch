//! Manual test server binary: serves one deterministic 8 MiB file with
//! Range support on 127.0.0.1:45999/file — used for app smoke tests.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test harness binary

use swiftfetch_test_server::{Route, TestServer};

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let server = TestServer::start().await.expect("bind");
    let url = server.url("/file");
    // Rewrite the ephemeral port onto the fixed smoke-test port by printing
    // the actual URL; the UIA driver reads it from stdout.
    eprintln!("SERVING {url}");
    let data = vec![0xABu8; 8 * 1024 * 1024];
    server.set_route("/file", Route::new(data));
    std::process::exit(i32::from(!wait_forever()));
}

fn wait_forever() -> bool {
    loop {
        std::thread::park();
    }
}
