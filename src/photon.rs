//! Server side of Photon's TCP wire protocol, as implemented by the client's
//! `ExitGames.Client.Photon.TPeer`. Multi-byte values are big-endian.
//!
//! Client to server:
//! - `0xF0` + i32 timestamp: ping
//! - `0xF1` + byte node id: TCP router init
//! - `0xFB` + i32 total length + channel + flag + body, where the body is
//!   `0xF3 0x00` plus nine header bytes and a 32-byte app name (init), or
//!   `0xF3 0x02` plus an operation request
//!
//! Server to client:
//! - `0xF0` + i32 server time + i32 echoed timestamp
//! - `0xFB` + i32 total length + `0x00 0x00 0xF3` + message type + body

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use tokio::io::{AsyncRead, AsyncReadExt};

const MAGIC_PING: u8 = 0xF0;
const MAGIC_ROUTER: u8 = 0xF1;
const MAGIC_MESSAGE: u8 = 0xFB;
const MAGIC_BODY: u8 = 0xF3;

const MESSAGE_INIT_RESPONSE: u8 = 1;
const MESSAGE_EVENT: u8 = 4;

const TYPE_NULL: u8 = 42;
const TYPE_BYTE: u8 = 98;
const TYPE_DOUBLE: u8 = 100;
const TYPE_FLOAT: u8 = 102;
const TYPE_INTEGER: u8 = 105;
const TYPE_SHORT: u8 = 107;
const TYPE_LONG: u8 = 108;
const TYPE_BOOLEAN: u8 = 111;
const TYPE_STRING: u8 = 115;
const TYPE_BYTE_ARRAY: u8 = 120;

const FRAME_HEADER_LEN: usize = 5;
const MIN_FRAME_LEN: usize = 7;
const MAX_FRAME_LEN: usize = 1 << 20;
const APP_NAME_OFFSET: usize = 11;
const APP_NAME_LEN: usize = 32;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Byte(u8),
    Bool(bool),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Str(String),
    Bytes(Vec<u8>),
}

pub type Params = BTreeMap<u8, Value>;

#[derive(Debug, PartialEq)]
pub enum Frame {
    Ping { timestamp: i32 },
    RouterInit,
    Init { app: String },
    Operation { code: u8, params: Params },
    UnknownBody { subtype: u8 },
}

/// Reads one frame, or `None` when the client closed the connection between frames.
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Option<Frame>> {
    let mut magic = [0u8; 1];
    if reader.read(&mut magic).await? == 0 {
        return Ok(None);
    }

    let frame = match magic[0] {
        MAGIC_PING => Frame::Ping { timestamp: reader.read_i32().await? },
        MAGIC_ROUTER => {
            reader.read_u8().await?;
            Frame::RouterInit
        }
        MAGIC_MESSAGE => {
            let total = reader.read_i32().await? as usize;
            if !(MIN_FRAME_LEN..=MAX_FRAME_LEN).contains(&total) {
                bail!("bogus frame length {total}");
            }
            let mut body = vec![0u8; total - FRAME_HEADER_LEN];
            reader.read_exact(&mut body).await?;
            parse_body(&body)?
        }
        other => bail!("unexpected magic byte 0x{other:02X}"),
    };
    Ok(Some(frame))
}

/// `body` is channel, flag, `0xF3`, subtype, then the rest.
fn parse_body(body: &[u8]) -> Result<Frame> {
    if body.len() < 4 || body[2] != MAGIC_BODY {
        bail!("frame body is not a Photon message");
    }
    let subtype = body[3];
    Ok(match subtype {
        0 => Frame::Init { app: parse_app_name(body) },
        2 => {
            let mut cursor = Cursor { data: &body[4..] };
            let code = cursor.byte()?;
            Frame::Operation { code, params: cursor.param_table()? }
        }
        other => Frame::UnknownBody { subtype: other },
    })
}

fn parse_app_name(body: &[u8]) -> String {
    body.get(APP_NAME_OFFSET..APP_NAME_OFFSET + APP_NAME_LEN)
        .map(|name| String::from_utf8_lossy(name).trim_end_matches('\0').to_owned())
        .unwrap_or_default()
}

struct Cursor<'a> {
    data: &'a [u8],
}

impl Cursor<'_> {
    fn take(&mut self, count: usize) -> Result<&[u8]> {
        if self.data.len() < count {
            bail!("Protocol16: unexpected end of data");
        }
        let (head, tail) = self.data.split_at(count);
        self.data = tail;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.take(N)?.try_into()?)
    }

    fn byte(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }

    fn short(&mut self) -> Result<i16> {
        Ok(i16::from_be_bytes(self.array()?))
    }

    fn int(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.array()?))
    }

    fn param_table(&mut self) -> Result<Params> {
        let count = self.short()?;
        let mut params = Params::new();
        for _ in 0..count {
            let key = self.byte()?;
            let kind = self.byte()?;
            params.insert(key, self.value(kind)?);
        }
        Ok(params)
    }

    fn value(&mut self, kind: u8) -> Result<Value> {
        Ok(match kind {
            TYPE_NULL => Value::Null,
            TYPE_BYTE => Value::Byte(self.byte()?),
            TYPE_BOOLEAN => Value::Bool(self.byte()? != 0),
            TYPE_SHORT => Value::Short(self.short()?),
            TYPE_INTEGER => Value::Int(self.int()?),
            TYPE_LONG => Value::Long(i64::from_be_bytes(self.array()?)),
            TYPE_FLOAT => Value::Float(f32::from_be_bytes(self.array()?)),
            TYPE_DOUBLE => Value::Double(f64::from_be_bytes(self.array()?)),
            TYPE_STRING => {
                let len = self.short()?.max(0) as usize;
                Value::Str(String::from_utf8_lossy(self.take(len)?).into_owned())
            }
            TYPE_BYTE_ARRAY => {
                let len = self.int()?.max(0) as usize;
                Value::Bytes(self.take(len)?.to_vec())
            }
            other => bail!("Protocol16: unhandled type code {other}"),
        })
    }
}

fn write_value(out: &mut Vec<u8>, value: &Value) {
    match value {
        Value::Null => out.push(TYPE_NULL),
        Value::Byte(v) => out.extend([TYPE_BYTE, *v]),
        Value::Bool(v) => out.extend([TYPE_BOOLEAN, u8::from(*v)]),
        Value::Short(v) => {
            out.push(TYPE_SHORT);
            out.extend(v.to_be_bytes());
        }
        Value::Int(v) => {
            out.push(TYPE_INTEGER);
            out.extend(v.to_be_bytes());
        }
        Value::Long(v) => {
            out.push(TYPE_LONG);
            out.extend(v.to_be_bytes());
        }
        Value::Float(v) => {
            out.push(TYPE_FLOAT);
            out.extend(v.to_be_bytes());
        }
        Value::Double(v) => {
            out.push(TYPE_DOUBLE);
            out.extend(v.to_be_bytes());
        }
        Value::Str(v) => {
            out.push(TYPE_STRING);
            out.extend((v.len() as i16).to_be_bytes());
            out.extend(v.as_bytes());
        }
        Value::Bytes(v) => {
            out.push(TYPE_BYTE_ARRAY);
            out.extend((v.len() as i32).to_be_bytes());
            out.extend(v);
        }
    }
}

fn message_frame(message_type: u8, body: &[u8]) -> Vec<u8> {
    let total = 9 + body.len();
    let mut frame = Vec::with_capacity(total);
    frame.push(MAGIC_MESSAGE);
    frame.extend((total as i32).to_be_bytes());
    frame.extend([0, 0, MAGIC_BODY, message_type]);
    frame.extend(body);
    frame
}

pub fn init_response() -> Vec<u8> {
    message_frame(MESSAGE_INIT_RESPONSE, &[])
}

pub fn router_ok() -> Vec<u8> {
    vec![0]
}

pub fn ping_response(server_time: i32, timestamp: i32) -> Vec<u8> {
    let mut frame = vec![MAGIC_PING];
    frame.extend(server_time.to_be_bytes());
    frame.extend(timestamp.to_be_bytes());
    frame
}

pub fn event(code: u8, params: &Params) -> Vec<u8> {
    let mut body = vec![code];
    body.extend((params.len() as i16).to_be_bytes());
    for (key, value) in params {
        body.push(*key);
        write_value(&mut body, value);
    }
    message_frame(MESSAGE_EVENT, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reads_an_operation_frame() {
        let mut body = vec![0, 0, MAGIC_BODY, 2, 1];
        body.extend(1i16.to_be_bytes());
        body.push(3);
        write_value(&mut body, &Value::Bytes(vec![7, 8]));

        let mut wire = vec![MAGIC_MESSAGE];
        wire.extend(((body.len() + FRAME_HEADER_LEN) as i32).to_be_bytes());
        wire.extend(&body);

        let frame = read_frame(&mut wire.as_slice()).await.unwrap().unwrap();
        let expected = Params::from([(3, Value::Bytes(vec![7, 8]))]);
        assert_eq!(frame, Frame::Operation { code: 1, params: expected });
    }

    #[tokio::test]
    async fn clean_close_is_not_an_error() {
        let empty: &[u8] = &[];
        assert!(read_frame(&mut { empty }).await.unwrap().is_none());
    }

    #[test]
    fn event_frame_states_its_full_length() {
        let frame = event(0, &Params::from([(1, Value::Int(90))]));
        let total = i32::from_be_bytes(frame[1..5].try_into().unwrap()) as usize;
        assert_eq!(total, frame.len());
    }
}
