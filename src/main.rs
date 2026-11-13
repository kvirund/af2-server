mod afc;
mod config;
mod game;
mod messages;
mod photon;
mod server;
mod storage;
mod world;

use std::fs::{self, OpenOptions};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use af2_server::files::find_config_dir;
use anyhow::{Context, Result, bail};
use tokio::net::TcpListener;

use storage::{SqliteStorage, Storage};
use world::{Log, World};

const USAGE: &str = "af2-server [--config <dir>] [--db <file>] [--bind <ip>] [--port <n>] [--trace <file>]";

#[derive(Default)]
struct Options {
    config: Option<PathBuf>,
    db: Option<PathBuf>,
    bind: Option<String>,
    port: Option<u16>,
    trace: Option<PathBuf>,
}

fn parse_options() -> Result<Option<Options>> {
    let mut options = Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().with_context(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--config" => options.config = Some(value()?.into()),
            "--db" => options.db = Some(value()?.into()),
            "--bind" => options.bind = Some(value()?),
            "--port" => options.port = Some(value()?.parse().context("--port")?),
            "--trace" => options.trace = Some(value()?.into()),
            "-h" | "--help" => return Ok(None),
            other => bail!("unknown argument {other}\n{USAGE}"),
        }
    }
    Ok(Some(options))
}

async fn run(options: Options) -> Result<()> {
    let config_dir = match options.config {
        Some(dir) => dir,
        None => find_config_dir()?,
    };
    let root = config_dir.canonicalize()?.parent().context("config has no parent")?.to_owned();
    let data = config::load(&config_dir, &root)?;

    let db_path = options.db.unwrap_or_else(|| root.join("data").join("af2.sqlite"));
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let store = SqliteStorage::open(&db_path)
        .await
        .with_context(|| format!("cannot open {}", db_path.display()))?;

    let gather_points = store.load_gather_points().await?;

    let trace = match options.trace {
        Some(path) => Some(OpenOptions::new().create(true).append(true).open(path)?),
        None => None,
    };

    let bind = options.bind.unwrap_or_else(|| data.settings.server.bind.clone());
    let port = options.port.unwrap_or(data.settings.server.port);
    let world = Arc::new(World::new(
        data.settings,
        data.fish,
        data.catalog,
        data.recipes,
        Box::new(store),
        Log::new(trace),
    ));
    world.restore_gather_points(gather_points);

    let listener = TcpListener::bind((bind.as_str(), port)).await?;
    world.log.line(&format!("listening on {bind}:{port}, saving to {}", db_path.display()));

    let mut next_connection = 0u64;
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, remote) = accepted?;
                next_connection += 1;
                tokio::spawn(server::run_connection(stream, remote, next_connection, world.clone()));
            }
            _ = tokio::signal::ctrl_c() => {
                world.log.line("shutting down");
                return Ok(());
            }
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let result = match parse_options() {
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(Some(options)) => run(options).await,
        Err(error) => Err(error),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}
