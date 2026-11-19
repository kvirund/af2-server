//! One task per connection: read frames, hand game messages to [`game`], and
//! keep a record of what could not be answered.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::{Notify, mpsc};

use crate::afc::AfcMessage;
use crate::game::{self, Conn, Outcome, Session};
use crate::messages;
use crate::photon::{self, Frame, Params, Value};
use crate::world::Worlds;

const EXPECTED_APP: &str = "AFCServer";
const OUTGOING_QUEUE: usize = 256;
/// Admin jobs waiting for this connection. A couple at a time is plenty: the
/// web side waits for each answer before it sends the next.
const ADMIN_QUEUE: usize = 8;
const HEX_LIMIT: usize = 64;

async fn write_frames(mut socket: OwnedWriteHalf, mut frames: mpsc::Receiver<Vec<u8>>) {
    while let Some(frame) = frames.recv().await {
        if socket.write_all(&frame).await.is_err() {
            return;
        }
    }
}

pub async fn run_connection(stream: TcpStream, remote: SocketAddr, number: u64, worlds: Arc<Worlds>) {
    // Small request and reply frames must not wait for each other.
    let _ = stream.set_nodelay(true);
    let (mut reader, writer) = stream.into_split();
    let (sender, receiver) = mpsc::channel(OUTGOING_QUEUE);
    tokio::spawn(write_frames(writer, receiver));

    let kick = Arc::new(Notify::new());
    let (admin_sender, mut admin_jobs) = mpsc::channel(ADMIN_QUEUE);
    let conn = Conn::new(number, sender, worlds);
    conn.log(&format!("connected from {remote}"));

    let mut session = Session::new(conn, kick.clone(), admin_sender);
    let mut gaps = BTreeMap::new();
    let started = Instant::now();

    loop {
        let frame = tokio::select! {
            frame = photon::read_frame(&mut reader) => frame,
            // A change asked for on the web port. It runs here, between
            // messages, so it owns the character like every other handler.
            Some(job) = admin_jobs.recv() => {
                session.conn.refresh();
                game::admin::run(&mut session, job).await;
                continue;
            }
            () = kick.notified() => {
                session.conn.log("character opened from another connection; closing this one");
                break;
            }
        };
        let step = match frame {
            Ok(Some(frame)) => handle_frame(&mut session, frame, &mut gaps, started).await,
            Ok(None) => {
                session.conn.log("client closed the connection");
                break;
            }
            Err(error) => {
                session.conn.log(&format!("disconnected: {error:#}"));
                break;
            }
        };
        if let Err(error) = step {
            session.conn.log(&format!("connection failed: {error:#}"));
            break;
        }
    }

    session.leave();
    log_gap_summary(&session.conn, &gaps);
}

async fn handle_frame(
    session: &mut Session,
    frame: Frame,
    gaps: &mut BTreeMap<i32, u32>,
    started: Instant,
) -> Result<()> {
    session.conn.refresh();
    game::tick(session).await?;
    match frame {
        Frame::Ping { timestamp } => {
            let server_time = started.elapsed().as_millis() as i32;
            session.conn.send_raw(photon::ping_response(server_time, timestamp)).await
        }
        Frame::RouterInit => session.conn.send_raw(photon::router_ok()).await,
        Frame::Init { app } => {
            let note = if app == EXPECTED_APP { String::new() } else { format!(" (expected {EXPECTED_APP})") };
            session.conn.log(&format!("init request, application \"{app}\"{note}"));
            session.conn.send_raw(photon::init_response()).await
        }
        Frame::UnknownBody { subtype } => {
            session.conn.log(&format!("UNANSWERED photon message sub-type {subtype}"));
            Ok(())
        }
        Frame::Operation { code, params } => handle_operation(session, code, &params, gaps).await,
    }
}

async fn handle_operation(
    session: &mut Session,
    code: u8,
    params: &Params,
    gaps: &mut BTreeMap<i32, u32>,
) -> Result<()> {
    let Some(message) = AfcMessage::parse(code, params) else {
        session.conn.log(&format!("UNANSWERED op {code}: not a game message: {}", describe(params)));
        return Ok(());
    };

    let checksum = if message.checksum_valid() {
        "ok".to_owned()
    } else {
        format!("MISMATCH (got 0x{:08X})", message.checksum)
    };
    session.conn.log(&format!(
        "<- msg {} via op{code}, counter {}, checksum {checksum}, {} bytes",
        message.id,
        message.counter,
        message.payload.len()
    ));

    let outcome = match game::handle(session, &message).await {
        Ok(outcome) => outcome,
        Err(error) => {
            session.conn.log(&format!("   reply failed: {error:#}"));
            Outcome::Unanswered
        }
    };

    // A report we already know needs no reply is not a gap, so it neither lands
    // in the log nor raises the overlay. Leaving it in drowned the real gaps.
    if outcome == Outcome::Unanswered && !messages::REPORTS_ONLY.contains(&message.id) {
        record_unanswered(session, &message, gaps);
        session.conn.clear_overlay().await?;
        session.conn.log(&format!("   -> 1310 overlay cleared after unanswered msg {}", message.id));
    }
    Ok(())
}

/// Every client message left without an answer, bar the reports listed as
/// needing none, is a gap in the protocol, so each gets a greppable line. A
/// first sighting is flagged NEW and shows the whole payload.
fn record_unanswered(session: &Session, message: &AfcMessage, gaps: &mut BTreeMap<i32, u32>) {
    let count = gaps.entry(message.id).or_insert(0);
    *count += 1;
    let (flag, limit) = if *count == 1 { (" NEW", usize::MAX) } else { ("", HEX_LIMIT) };
    session.conn.log(&format!(
        "   UNANSWERED{flag} msg {} (#{count}), {} bytes: {}",
        message.id,
        message.payload.len(),
        hex(&message.payload, limit)
    ));
}

fn log_gap_summary(conn: &Conn, gaps: &BTreeMap<i32, u32>) {
    if gaps.is_empty() {
        return;
    }
    let summary: Vec<String> = gaps.iter().map(|(id, count)| format!("{id} x{count}")).collect();
    conn.log(&format!("UNANSWERED SUMMARY: {}", summary.join(", ")));
}

fn describe(params: &Params) -> String {
    let parts: Vec<String> = params
        .iter()
        .map(|(key, value)| match value {
            Value::Bytes(bytes) => format!("{key}=byte[{}]", bytes.len()),
            other => format!("{key}={other:?}"),
        })
        .collect();
    format!("{{{}}}", parts.join(", "))
}

fn hex(data: &[u8], limit: usize) -> String {
    let shown = data.len().min(limit);
    let mut text = String::new();
    for byte in &data[..shown] {
        let _ = write!(text, "{byte:02X} ");
    }
    if data.len() > shown {
        let _ = write!(text, "... (+{})", data.len() - shown);
    }
    text.trim_end().to_owned()
}
