//! Lines in the chat's system tab, where the client itself writes the money it
//! gets. Case 50: int channel, long sender, string sender name, long private
//! addressee (0 for none), string addressee name, string text, string channel
//! name. Channel 0 is "sys"; the client's own lines there are signed ">".

use anyhow::Result;

use super::Conn;
use crate::afc::AfcWriter;
use crate::messages as msg;

const SYSTEM_CHANNEL: i32 = 0;
const SIGN: &str = ">";

pub async fn system(conn: &Conn, text: &str) -> Result<()> {
    let mut w = AfcWriter::new();
    w.int32(SYSTEM_CHANNEL).int64(0).string(SIGN);
    w.int64(0).string("");
    w.string(text).string("sys");
    conn.send(msg::CHAT, w.finish()).await?;
    conn.log(&format!("   -> 50 sys: {text}"));
    Ok(())
}
