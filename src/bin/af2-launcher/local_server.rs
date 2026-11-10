//! Runs the server beside the launcher for as long as the game needs it.

use std::fs;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use af2_server::files::read_commented;
use anyhow::{Context, Result, bail};
use serde::Deserialize;

const SERVER_EXE: &str = "af2-server.exe";
/// Only this computer plays here. Listening on every interface would also make
/// Windows ask each player to open the firewall.
const LOOPBACK: &str = "127.0.0.1";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const STARTUP_POLL: Duration = Duration::from_millis(100);
const CONNECT_TIMEOUT: Duration = Duration::from_millis(300);

#[derive(Deserialize)]
struct Settings {
    server: ServerSettings,
}

#[derive(Deserialize)]
struct ServerSettings {
    port: u16,
}

fn is_listening(port: u16) -> bool {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    TcpStream::connect_timeout(&address, CONNECT_TIMEOUT).is_ok()
}

/// Stops the server when dropped, unless it was already running before us.
pub struct LocalServer {
    child: Option<Child>,
}

impl LocalServer {
    pub fn start(config_dir: &Path, data_dir: &Path) -> Result<Self> {
        let settings: Settings = read_commented(&config_dir.join("settings.json"))?;
        let port = settings.server.port;
        if is_listening(port) {
            println!("Локальный сервер уже запущен, использую его.");
            return Ok(Self { child: None });
        }

        let program = std::env::current_exe()?.with_file_name(SERVER_EXE);
        if !program.is_file() {
            bail!("рядом с лаунчером нет {SERVER_EXE}");
        }
        fs::create_dir_all(data_dir)?;

        let child = Command::new(&program)
            .arg("--config")
            .arg(config_dir)
            .args(["--bind", LOOPBACK])
            .arg("--trace")
            .arg(data_dir.join("server.log"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .with_context(|| format!("не удалось запустить {}", program.display()))?;

        // From here on, dropping `server` stops the child on every early return.
        let mut server = Self { child: Some(child) };
        server.wait_until_ready(port)?;
        println!("Локальный сервер запущен, порт {port}.");
        Ok(server)
    }

    fn wait_until_ready(&mut self, port: u16) -> Result<()> {
        let child = self.child.as_mut().context("сервер не запущен")?;
        let started = Instant::now();
        while !is_listening(port) {
            if let Some(status) = child.try_wait()? {
                bail!("сервер завершился при запуске: {status}");
            }
            if started.elapsed() > STARTUP_TIMEOUT {
                bail!("сервер не начал принимать подключения за {} секунд", STARTUP_TIMEOUT.as_secs());
            }
            sleep(STARTUP_POLL);
        }
        Ok(())
    }
}

impl Drop for LocalServer {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            // Progress is saved after every change, so a plain kill loses nothing.
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
