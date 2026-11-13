//! The client's quest dialog window, shared by the NPCs that talk.
//!
//! Case 70 loads `quests/{file}.lng` and shows its text `n{node}`, each case 72
//! adds the answer `a{id}`, and case 74 opens the window. A click sends 65 with
//! the answer and the long that came with it, and locks the buttons until the
//! next 74 or 76.

use anyhow::Result;

use super::{Conn, Player};
use crate::afc::AfcWriter;
use crate::messages as msg;

/// What the texts with a count of things carried show it as.
pub const COUNT_MARK: &str = "[cnt]";

pub async fn option(conn: &Conn, file: i32, node: i32, answer: i32, param: i64) -> Result<()> {
    let mut w = AfcWriter::new();
    w.int32(file).int32(node).int32(answer).int64(param);
    conn.send(msg::DIALOG_OPTION, w.finish()).await
}

pub async fn text(conn: &Conn, file: i32, node: i32) -> Result<()> {
    let mut w = AfcWriter::new();
    w.int32(file).int32(node);
    conn.send(msg::DIALOG_TEXT, w.finish()).await
}

pub async fn open(conn: &Conn, player: &mut Player, file: i32) -> Result<()> {
    player.dialog = Some(file);
    let mut w = AfcWriter::new();
    w.int32(file);
    conn.send(msg::DIALOG_OPEN, w.finish()).await
}

pub async fn close(conn: &Conn, player: &mut Player, file: i32) -> Result<()> {
    player.dialog = None;
    let mut w = AfcWriter::new();
    w.int32(file);
    conn.send(msg::DIALOG_CLOSE, w.finish()).await
}

/// Case 78 replaces a mark in the text shown last.
pub async fn replace(conn: &Conn, mark: &str, with: &str) -> Result<()> {
    let mut w = AfcWriter::new();
    w.string(mark).string(with);
    conn.send(msg::DIALOG_REPLACE, w.finish()).await
}
