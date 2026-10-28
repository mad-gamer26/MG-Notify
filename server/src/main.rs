//! MG Notify's server. Usage:
//!
//!     mg-notify-server [--config PATH]               run the server
//!     mg-notify-server --configure [--config PATH]   edit config.json interactively
//!     mg-notify-server import FOLDER [--config PATH] load the Python server's JSON files
//!
//! The config defaults to config.json in the current folder.

mod api;
mod config;
mod db;
mod email;
mod events;
mod import;
mod push;
mod smtp;
mod util;

use std::io::IsTerminal;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const DATABASE_FILE: &str = "mg-notify.sqlite";
const BACKUPS_KEPT: usize = 14;
const SESSION_IDLE_DAYS: i64 = 180;

fn usage() -> ! {
    eprintln!("usage: mg-notify-server [--config PATH] [--configure | import FOLDER]");
    std::process::exit(2);
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut config_path = PathBuf::from("config.json");
    let mut configure = false;
    let mut import_from = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => config_path = args.next().map(PathBuf::from).unwrap_or_else(|| usage()),
            "-c" | "--configure" => configure = true,
            "import" => import_from = Some(args.next().map(PathBuf::from).unwrap_or_else(|| usage())),
            "-h" | "--help" => {
                println!("usage: mg-notify-server [--config PATH] [--configure | import FOLDER]");
                return;
            }
            _ => usage(),
        }
    }

    let settings = config::load(&config_path);
    if configure {
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            eprintln!("--configure requires an interactive terminal.");
            std::process::exit(1);
        }
        if let Err(error) = config::run_configure(&config_path, settings.as_ref().err().map(String::as_str)) {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }
    let settings = settings.unwrap_or_else(|error| {
        eprintln!("Invalid configuration in {}: {error}", config_path.display());
        eprintln!("Run `mg-notify-server --configure` to update the file.");
        std::process::exit(1);
    });

    let filter = match settings.log_level.as_str() {
        "CRITICAL" | "ERROR" => "error",
        "WARNING" => "warn",
        "DEBUG" => "debug",
        _ => "info",
    };
    tracing_subscriber::fmt()
        .with_env_filter(format!("mg_notify_server={filter},warn"))
        .with_ansi(std::io::stdout().is_terminal())
        .init();

    if let Err(error) = std::fs::create_dir_all(&settings.data_dir) {
        eprintln!("Cannot create {}: {error}", settings.data_dir.display());
        std::process::exit(1);
    }
    let db = db::Db::open(&settings.data_dir.join(DATABASE_FILE)).unwrap_or_else(|error| {
        eprintln!("Cannot open the database: {error}");
        std::process::exit(1);
    });

    if let Some(folder) = import_from {
        if let Err(error) = import::run(&db, &folder) {
            eprintln!("Import failed: {error}");
            std::process::exit(1);
        }
        println!("Imported into {}", settings.data_dir.join(DATABASE_FILE).display());
        return;
    }

    tokio::runtime::Runtime::new().expect("tokio runtime").block_on(serve(settings, db));
}

async fn serve(settings: config::Settings, db: db::Db) {
    let db = Arc::new(db);
    let pusher = Arc::new(push::Pusher::new(&settings, &db));
    let email = email::EmailSender::new(&settings);
    let address = format!("{}:{}", settings.bind_host, settings.bind_port);
    tokio::spawn(maintenance(db.clone(), settings.data_dir.clone()));
    let state = Arc::new(api::AppState { settings, db, pusher, email, hub: events::Hub::new() });
    let listener = tokio::net::TcpListener::bind(&address).await.unwrap_or_else(|error| {
        eprintln!("Cannot listen on {address}: {error}");
        std::process::exit(1);
    });
    tracing::info!("Starting MG Notify server on {address}");
    tokio::spawn(smtp::run(state.clone()));
    let app = api::router(state).into_make_service_with_connect_info::<SocketAddr>();
    let (stop, mut stopping) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
        let _ = stop.send(true);
    });
    let mut graceful = stopping.clone();
    let server = axum::serve(listener, app).with_graceful_shutdown(async move {
        let _ = graceful.wait_for(|stop| *stop).await;
    });
    // Open event streams never finish by themselves, so a graceful shutdown
    // would wait for them forever: give requests in flight a moment, then
    // stop. The apps and website reconnect on their own.
    tokio::select! {
        result = server => result.expect("server"),
        _ = async {
            let _ = stopping.wait_for(|stop| *stop).await;
            tokio::time::sleep(Duration::from_secs(3)).await;
        } => tracing::info!("Stopped, closing open event streams"),
    }
}

/// Once a day: forget sessions idle for 180 days, and snapshot the database
/// into backups/, keeping the newest 14.
async fn maintenance(db: Arc<db::Db>, data_dir: PathBuf) {
    let backups = data_dir.parent().unwrap_or(Path::new(".")).join("backups");
    loop {
        let cutoff = util::iso_after(-SESSION_IDLE_DAYS * 24 * 3600);
        match db.delete_sessions_unused_since(&cutoff) {
            Ok(0) => {}
            Ok(n) => tracing::info!("Removed {n} sessions unused for {SESSION_IDLE_DAYS} days"),
            Err(error) => tracing::error!("Session cleanup failed: {error}"),
        }
        if let Err(error) = backup(&db, &backups) {
            tracing::error!("Backup failed: {error}");
        }
        tokio::time::sleep(Duration::from_secs(24 * 3600)).await;
    }
}

fn backup(db: &db::Db, folder: &Path) -> Result<(), String> {
    std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    let stamp = util::iso_now().replace([':', '-'], "");
    let path = folder.join(format!("mg-notify-{stamp}.sqlite"));
    db.snapshot(&path).map_err(|e| e.to_string())?;
    let mut existing: Vec<PathBuf> = std::fs::read_dir(folder)
        .map_err(|e| e.to_string())?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("mg-notify-") && n.ends_with(".sqlite")))
        .collect();
    existing.sort();
    let excess = existing.len().saturating_sub(BACKUPS_KEPT);
    for old in existing.into_iter().take(excess) {
        let _ = std::fs::remove_file(old);
    }
    tracing::info!("Backed up the database to {}", path.display());
    Ok(())
}
