//! Prepares an installed Atom Fishing 2 client for a chosen server and starts it.

mod choice;
mod client_patch;
mod items;
mod local_server;
mod recipes;
mod skills;
mod steam;

use std::io::stdin;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use af2_server::files::find_config_dir;
use anyhow::{Context, Result, bail};

use choice::Target;
use local_server::LocalServer;

const USAGE: &str = "af2-launcher [--server local|<адрес>] [--game <папка игры>] [--no-launch] [--status] [--restore] [--extract]

  --server     выбрать сервер без меню
  --game       папка игры, если Steam не находит её сам
  --no-launch  подготовить всё, но не запускать игру
  --status     показать адрес, записанный в клиенте
  --restore    вернуть клиенту исходный адрес
  --extract    только извлечь данные игры в data/ (предметы, рецепты, навыки)";

#[derive(Default)]
struct Options {
    server: Option<String>,
    game: Option<PathBuf>,
    no_launch: bool,
    status: bool,
    restore: bool,
    extract: bool,
}

fn parse_options() -> Result<Option<Options>> {
    let mut options = Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().with_context(|| format!("{arg}: нужно значение"));
        match arg.as_str() {
            "--server" => options.server = Some(value()?),
            "--game" => options.game = Some(value()?.into()),
            "--no-launch" => options.no_launch = true,
            "--status" => options.status = true,
            "--restore" => options.restore = true,
            "--extract" => options.extract = true,
            "-h" | "--help" => return Ok(None),
            other => bail!("неизвестный параметр {other}\n{USAGE}"),
        }
    }
    Ok(Some(options))
}

fn wait_for_enter() {
    let mut line = String::new();
    // An error or a closed input just means nobody is there to press it.
    let _ = stdin().read_line(&mut line);
}

fn point_client_at(game: &Path, host: &str) -> Result<()> {
    let dll = client_patch::assembly_path(game);
    if client_patch::current_host(&dll)? == host {
        println!("Клиент уже настроен на {host}.");
        return Ok(());
    }
    if steam::game_is_running()? {
        bail!("Закрой игру: пока она запущена, адрес сервера поменять нельзя.");
    }
    client_patch::set_host(&dll, host)?;
    println!("Клиент настроен на {host}.");
    Ok(())
}

fn run(options: Options) -> Result<()> {
    let game = match options.game {
        Some(game) => game,
        None => steam::find_game()?,
    };
    let dll = client_patch::assembly_path(&game);

    if options.status {
        println!("Адрес в клиенте: {}", client_patch::current_host(&dll)?);
        return Ok(());
    }
    if options.restore {
        return point_client_at(&game, client_patch::ORIGINAL_HOST);
    }

    let config_dir = find_config_dir()?;
    let root = config_dir.canonicalize()?.parent().context("у папки config нет родителя")?.to_owned();
    let data_dir = root.join("data");

    items::ensure_extracted(&game, &data_dir)?;
    recipes::ensure_extracted(&game, &data_dir)?;
    skills::ensure_extracted(&game, &data_dir)?;
    if options.extract {
        return Ok(());
    }
    let target = choice::choose(&config_dir, &data_dir, options.server)?;
    point_client_at(&game, target.host())?;

    let server = match target {
        Target::Local => Some(LocalServer::start(&config_dir, &data_dir)?),
        Target::Remote(_) => None,
    };

    if options.no_launch {
        if server.is_some() {
            println!("Сервер работает. Нажми Enter, чтобы остановить его.");
            wait_for_enter();
        }
        return Ok(());
    }

    steam::launch_game()?;
    if server.is_some() {
        println!("Игра запускается. Не закрывай это окно: здесь работает сервер.");
        steam::wait_until_game_closes()?;
        println!("Игра закрыта, сервер остановлен.");
    }
    Ok(())
}

fn main() -> ExitCode {
    let result = match parse_options() {
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(Some(options)) => run(options),
        Err(error) => Err(error),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Ошибка: {error:#}");
            eprintln!("Нажми Enter, чтобы закрыть окно.");
            wait_for_enter();
            ExitCode::FAILURE
        }
    }
}
