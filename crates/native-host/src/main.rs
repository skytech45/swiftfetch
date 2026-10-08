//! `SwiftFetch` browser native-messaging host.
//!
//! Runs the framing + dispatch loop over stdio (the browser spawn model)
//! and offers `--print-manifest` so installers can emit the native-
//! messaging manifest referencing the pinned extension ids.

use std::io::Write;
use std::sync::Arc;

use swiftfetch_native_host::{HOST_ID, serve};

fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("swiftfetch=info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => {
            let store = match swiftfetch_store::Store::open_default() {
                Ok(store) => store,
                Err(err) => {
                    eprintln!("swiftfetch-native-host: cannot open database: {err}");
                    return std::process::ExitCode::FAILURE;
                }
            };
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(err) => {
                    eprintln!("swiftfetch-native-host: cannot start tokio runtime: {err}");
                    return std::process::ExitCode::FAILURE;
                }
            };
            let result = runtime.block_on(serve(
                Arc::new(std::sync::Mutex::new(store)),
                tokio::io::stdin(),
                tokio::io::stdout(),
                tokio_util::sync::CancellationToken::new(),
            ));
            match result {
                Ok(()) => std::process::ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("swiftfetch-native-host: {err}");
                    std::process::ExitCode::FAILURE
                }
            }
        }
        Some("--print-manifest") => print_manifest(args.get(1).map(String::as_str)),
        Some("--help" | "-h") => {
            println!("usage: swiftfetch-native-host [--print-manifest <exe-path>]");
            println!("runs as a native-messaging host over stdio; HOST_ID = {HOST_ID}");
            std::process::ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("unknown argument {other}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Emits the native-messaging manifest JSON. `<exe-path>` defaults to this
/// binary; extension ids are pinned (MV3 `key` / Firefox gecko id) so the
/// host can allowlist them.
fn print_manifest(exe_override: Option<&str>) -> std::process::ExitCode {
    let exe = exe_override.map_or_else(
        || std::env::current_exe().unwrap_or_default(),
        std::path::PathBuf::from,
    );
    let manifest = serde_json::json!({
        "name": HOST_ID,
        "description": "SwiftFetch browser integration host",
        "path": exe,
        "type": "stdio",
        // Chrome/Edge/Opera: the extension manifest pins `key`, which fixes
        // the extension id; Firefox uses the gecko id directly.
        "allowed_extensions": ["swiftfetch@skytech45"],
        "allowed_origins": [
            "chrome-extension://ofinmfgldbecdccimfgknfhjekcioclb/",
            "chrome-extension://swiftfetch-edge@skytech45/"
        ]
    });
    match serde_json::to_string_pretty(&manifest) {
        Ok(text) => {
            println!("{text}");
            std::process::ExitCode::SUCCESS
        }
        Err(err) => {
            let _ = writeln!(std::io::stderr(), "manifest: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}
