//! `swiftfetch` — headless command-line control for the running app
//! (Milestone 3). The CLI shares the app's SQLite database: writes run
//! under `BEGIN IMMEDIATE` (single-writer discipline, system-design §5);
//! the desktop app consumes staged downloads and control commands within
//! about a second and acts through the live engine. Reads are plain WAL
//! readers — they never block the app.

use std::process::ExitCode;

use swiftfetch_store::Store;
use swiftfetch_store::repos;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(help) => {
            eprintln!("{help}");
            eprintln!();
            eprintln!("{}", usage());
            ExitCode::FAILURE
        }
    }
}

fn usage() -> &'static str {
    "swiftfetch — command-line control for the SwiftFetch desktop app\n\n\
     USAGE:\n    swiftfetch <command> [args]\n\n\
     COMMANDS:\n\
     \x20 add <url> [--dir <path>] [--queue <id>] [--name <file>]\n\
     \x20     Stage a download; the app picks it up within ~1 s.\n\
     \x20 list                       List downloads (short ids).\n\
     \x20 status <id>                Show one download in detail.\n\
     \x20 pause <id>                 Pause a download.\n\
     \x20 resume <id>                Resume a download.\n\
     \x20 cancel <id>                Cancel a download.\n\
     \x20 queues                     List queues.\n\
     \x20 start-queue <id|name>      Activate a queue.\n\
     \x20 stop-queue <id|name>       Deactivate a queue.\n\n\
     Ids may be full UUIDs or unique prefixes (8+ chars)."
}

fn run(args: &[String]) -> Result<(), String> {
    let Some(command) = args.first() else {
        return Err(usage().to_owned());
    };
    let store = Store::open_default().map_err(|e| format!("cannot open database: {e}"))?;
    match command.as_str() {
        "add" => cmd_add(&store, &args[1.min(args.len())..]),
        "list" | "ls" => cmd_list(&store),
        "status" => cmd_status(&store, arg(args, 1, "status <id>")?.as_str()),
        "pause" => cmd_control(&store, arg(args, 1, "pause <id>")?.as_str(), "pause"),
        "resume" => cmd_control(&store, arg(args, 1, "resume <id>")?.as_str(), "resume"),
        "cancel" => cmd_control(&store, arg(args, 1, "cancel <id>")?.as_str(), "cancel"),
        "queues" => cmd_queues(&store),
        "start-queue" => cmd_queue_active(&store, arg(args, 1, "start-queue <id>")?.as_str(), true),
        "stop-queue" => cmd_queue_active(&store, arg(args, 1, "stop-queue <id>")?.as_str(), false),
        "--help" | "-h" | "help" => {
            println!("{}", usage());
            Ok(())
        }
        other => Err(format!("unknown command: {other}")),
    }
}

fn arg(args: &[String], index: usize, what: &str) -> Result<String, String> {
    args.get(index)
        .cloned()
        .ok_or_else(|| format!("missing argument: {what}"))
}

/// Resolves a (possibly partial) id against the downloads table.
fn resolve_job(store: &Store, prefix: &str) -> Result<String, String> {
    let rows = repos::list_downloads(store).map_err(|e| e.to_string())?;
    if let Some(row) = rows.iter().find(|row| row.id == prefix) {
        return Ok(row.id.clone());
    }
    let matches: Vec<&repos::DownloadRow> = rows
        .iter()
        .filter(|row| row.id.starts_with(prefix))
        .collect();
    match matches.as_slice() {
        [one] => Ok(one.id.clone()),
        [] => Err(format!("no download matches {prefix}")),
        many => Err(format!(
            "id prefix {prefix} is ambiguous ({} matches)",
            many.len()
        )),
    }
}

fn cmd_add(store: &Store, args: &[String]) -> Result<(), String> {
    let mut url: Option<String> = None;
    let mut dir: Option<std::path::PathBuf> = None;
    let mut queue: Option<String> = None;
    let mut name: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dir" => {
                i += 1;
                dir = args.get(i).map(std::path::PathBuf::from);
            }
            "--queue" => {
                i += 1;
                queue = args.get(i).cloned();
            }
            "--name" => {
                i += 1;
                name = args.get(i).cloned();
            }
            other if url.is_none() && !other.starts_with("--") => url = Some(other.to_owned()),
            other => return Err(format!("unexpected argument: {other}")),
        }
        i += 1;
    }
    let Some(url) = url else {
        return Err("missing argument: add <url>".into());
    };
    if !(url.starts_with("http://") || url.starts_with("https://") || url.starts_with("ftp://")) {
        return Err("URL must start with http://, https:// or ftp://".into());
    }
    let id = repos::stage_download(
        store,
        &url,
        dir.as_deref(),
        name.as_deref(),
        queue.as_deref(),
        true,
        "cli",
    )
    .map_err(|e| e.to_string())?;
    println!("staged {id}");
    println!("the app will probe and add it within ~1 s (swiftfetch list to watch)");
    Ok(())
}

fn cmd_list(store: &Store) -> Result<(), String> {
    let rows = repos::list_downloads(store).map_err(|e| e.to_string())?;
    println!(
        "{:<10} {:<12} {:>6}  {:<28} URL",
        "ID", "STATE", "DONE", "NAME"
    );
    for row in &rows {
        let pct = row.total_len.map_or_else(
            || "-".to_owned(),
            |total| {
                #[allow(clippy::cast_precision_loss)] // display-only percentage
                let ratio = row.done_bytes as f64 / total as f64;
                format!("{:>3.0}%", ratio * 100.0)
            },
        );
        println!(
            "{:<10} {:<12} {:>6}  {:<28} {}",
            short(&row.id),
            row.state,
            pct,
            truncate(&row.filename, 28),
            truncate(&row.url, 48)
        );
    }
    Ok(())
}

fn cmd_status(store: &Store, prefix: &str) -> Result<(), String> {
    let id = resolve_job(store, prefix)?;
    let row = repos::get_download(store, &id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("download {id} vanished"))?;
    println!("id:         {}", row.id);
    println!("state:      {}", row.state);
    println!("url:        {}", row.url);
    println!("file:       {}", row.final_path);
    println!(
        "done:       {} of {}",
        row.done_bytes,
        row.total_len
            .map_or("unknown".to_owned(), |t| t.to_string())
    );
    println!("category:   {}", row.category_id.as_deref().unwrap_or("-"));
    println!("queue:      {}", row.queue_id.as_deref().unwrap_or("-"));
    println!("created:    {}", row.created_at);
    if let Some(code) = &row.error_code {
        println!(
            "error:      {} ({})",
            code,
            row.error_msg.as_deref().unwrap_or("-")
        );
    }
    Ok(())
}

fn cmd_control(store: &Store, prefix: &str, action: &str) -> Result<(), String> {
    let id = resolve_job(store, prefix)?;
    repos::enqueue_command(store, &id, action).map_err(|e| e.to_string())?;
    println!("{action} queued for {id} — the app will apply it within ~1 s");
    Ok(())
}

fn cmd_queues(store: &Store) -> Result<(), String> {
    let queues = repos::list_queues(store).map_err(|e| e.to_string())?;
    println!(
        "{:<10} {:<20} {:<6} {:<10} SCHEDULE",
        "ID", "NAME", "LIMIT", "ACTIVE"
    );
    for queue in &queues {
        println!(
            "{:<10} {:<20} {:<6} {:<10} {}",
            short(&queue.id),
            truncate(&queue.name, 20),
            queue.max_concurrent,
            if queue.is_active != 0 { "yes" } else { "no" },
            queue.schedule_json.as_deref().unwrap_or("manual"),
        );
    }
    Ok(())
}

fn cmd_queue_active(store: &Store, prefix: &str, active: bool) -> Result<(), String> {
    let queues = repos::list_queues(store).map_err(|e| e.to_string())?;
    let matches: Vec<&repos::QueueRow> = queues
        .iter()
        .filter(|q| q.id.starts_with(prefix) || q.name == prefix)
        .collect();
    let target = match matches.as_slice() {
        [one] => one,
        [] => return Err(format!("no queue matches {prefix}")),
        _ => return Err(format!("queue prefix {prefix} is ambiguous")),
    };
    repos::set_queue_active(store, &target.id, active).map_err(|e| e.to_string())?;
    println!(
        "queue {} {}",
        target.name,
        if active { "activated" } else { "deactivated" }
    );
    Ok(())
}

fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let cut: String = text.chars().take(max - 1).collect();
        format!("{cut}…")
    }
}
