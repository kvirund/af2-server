//! Which server to play on: the menu, and remembering the last answer.

use std::fs;
use std::io::{Write, stdin, stdout};
use std::path::Path;

use af2_server::files::read_commented;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::client_patch::validate_host;

const LOCAL_KEY: &str = "local";
const LOCAL_HOST: &str = "127.0.0.1";

#[derive(Debug, PartialEq)]
pub enum Target {
    Local,
    Remote(String),
}

impl Target {
    pub fn host(&self) -> &str {
        match self {
            Self::Local => LOCAL_HOST,
            Self::Remote(host) => host,
        }
    }

    fn key(&self) -> &str {
        match self {
            Self::Local => LOCAL_KEY,
            Self::Remote(host) => host,
        }
    }

    fn parse(text: &str) -> Result<Self> {
        if text.eq_ignore_ascii_case(LOCAL_KEY) {
            return Ok(Self::Local);
        }
        validate_host(text)?;
        Ok(Self::Remote(text.to_owned()))
    }
}

#[derive(Deserialize)]
struct LauncherConfig {
    servers: Vec<ServerEntry>,
}

#[derive(Deserialize)]
struct ServerEntry {
    name: String,
    host: String,
}

#[derive(Default, Deserialize, Serialize)]
struct Remembered {
    server: Option<String>,
}

#[derive(Debug, PartialEq)]
enum MenuAction {
    Use(String),
    AskAddress,
}

#[derive(Debug, PartialEq)]
struct MenuItem {
    label: String,
    action: MenuAction,
}

fn build_menu(servers: &[ServerEntry], last: Option<&str>) -> Vec<MenuItem> {
    let use_item = |label: String, key: &str| MenuItem { label, action: MenuAction::Use(key.to_owned()) };

    let mut menu = vec![use_item("Локальный сервер на этом компьютере".to_owned(), LOCAL_KEY)];
    menu.extend(servers.iter().map(|s| use_item(format!("{} ({})", s.name, s.host), &s.host)));
    if let Some(last) = last.filter(|last| *last != LOCAL_KEY && servers.iter().all(|s| s.host != *last)) {
        menu.push(use_item(format!("Прошлый адрес ({last})"), last));
    }
    menu.push(MenuItem { label: "Другой адрес...".to_owned(), action: MenuAction::AskAddress });
    menu
}

fn default_index(menu: &[MenuItem], last: Option<&str>) -> usize {
    let is_last = |item: &MenuItem| matches!(&item.action, MenuAction::Use(key) if Some(key.as_str()) == last);
    menu.iter().position(is_last).unwrap_or(0)
}

/// `None` when the answer is not a menu number.
fn pick(input: &str, count: usize, default: usize) -> Option<usize> {
    if input.is_empty() {
        return Some(default);
    }
    let number: usize = input.parse().ok()?;
    (1..=count).contains(&number).then(|| number - 1)
}

fn prompt(text: &str) -> Result<String> {
    print!("{text}");
    stdout().flush()?;
    let mut line = String::new();
    if stdin().read_line(&mut line)? == 0 {
        bail!("нет ввода; укажи сервер через --server");
    }
    Ok(line.trim().to_owned())
}

fn ask_address() -> Result<Target> {
    loop {
        match Target::parse(&prompt("Адрес сервера: ")?) {
            Ok(target) => return Ok(target),
            Err(error) => println!("{error}"),
        }
    }
}

fn run_menu(menu: &[MenuItem], default: usize) -> Result<Target> {
    println!("Выбери сервер:");
    for (index, item) in menu.iter().enumerate() {
        println!("  {}. {}", index + 1, item.label);
    }
    loop {
        let answer = prompt(&format!("Номер [{}]: ", default + 1))?;
        let Some(chosen) = pick(&answer, menu.len(), default) else {
            println!("Введи номер от 1 до {}.", menu.len());
            continue;
        };
        return match &menu[chosen].action {
            MenuAction::Use(key) => Target::parse(key),
            MenuAction::AskAddress => ask_address(),
        };
    }
}

pub fn choose(config_dir: &Path, data_dir: &Path, given: Option<String>) -> Result<Target> {
    let state_path = data_dir.join("launcher-state.json");
    let remembered: Remembered = read_commented(&state_path).unwrap_or_default();

    let target = match given {
        Some(text) => Target::parse(&text)?,
        None => {
            let config: LauncherConfig = read_commented(&config_dir.join("launcher.json"))?;
            let last = remembered.server.as_deref();
            let menu = build_menu(&config.servers, last);
            run_menu(&menu, default_index(&menu, last))?
        }
    };

    fs::create_dir_all(data_dir)?;
    let state = Remembered { server: Some(target.key().to_owned()) };
    fs::write(&state_path, serde_json::to_string(&state)?)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn servers() -> Vec<ServerEntry> {
        vec![ServerEntry { name: "Общий".into(), host: "af2.example.dev".into() }]
    }

    #[test]
    fn local_keyword_is_not_an_address() {
        assert_eq!(Target::parse("Local").unwrap(), Target::Local);
        assert_eq!(Target::parse("10.0.0.5").unwrap().host(), "10.0.0.5");
        assert!(Target::parse("bad host").is_err());
    }

    #[test]
    fn a_remembered_custom_address_gets_its_own_line() {
        let menu = build_menu(&servers(), Some("10.0.0.5"));
        assert_eq!(menu.len(), 4);
        assert_eq!(default_index(&menu, Some("10.0.0.5")), 2);
    }

    #[test]
    fn a_remembered_listed_server_is_not_repeated() {
        let menu = build_menu(&servers(), Some("af2.example.dev"));
        assert_eq!(menu.len(), 3);
        assert_eq!(default_index(&menu, Some("af2.example.dev")), 1);
    }

    #[test]
    fn enter_takes_the_default_and_numbers_are_checked() {
        assert_eq!(pick("", 3, 1), Some(1));
        assert_eq!(pick("3", 3, 0), Some(2));
        assert_eq!(pick("4", 3, 0), None);
        assert_eq!(pick("0", 3, 0), None);
        assert_eq!(pick("да", 3, 0), None);
    }
}
