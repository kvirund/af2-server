//! The game's own message layer on top of Photon.
//!
//! The client funnels everything through two Photon operation codes, with the
//! real message id and a little-endian payload in the parameters:
//!
//! - op 0: `{ 0: id, 7: (counter + id) ^ -124, 2: counter, 5: payload }`
//! - op 1: `{ 1: id, 4: (counter + id) ^ 0x0A937F7F, 2: counter, 3: payload }`
//!
//! Server to client: `{ 1: id, 3: payload }`.

use anyhow::{Result, bail};
use chrono::{Datelike, NaiveDateTime, Timelike};

use crate::photon::{Params, Value};

const CHECKSUM_KEY_OP0: i32 = -124;
const CHECKSUM_KEY_OP1: i32 = 0x0A93_7F7F;
const PARAM_COUNTER: u8 = 2;

#[derive(Debug)]
pub struct AfcMessage {
    pub op: u8,
    pub id: i32,
    pub counter: i32,
    pub checksum: i32,
    pub payload: Vec<u8>,
}

impl AfcMessage {
    pub fn parse(op: u8, params: &Params) -> Option<Self> {
        let (id_key, checksum_key, payload_key) = match op {
            0 => (0, 7, 5),
            1 => (1, 4, 3),
            _ => return None,
        };
        let int = |key: u8| match params.get(&key) {
            Some(Value::Int(v)) => Some(*v),
            _ => None,
        };
        let payload = match params.get(&payload_key) {
            Some(Value::Bytes(bytes)) => bytes.clone(),
            _ => Vec::new(),
        };
        Some(Self {
            op,
            id: int(id_key)?,
            counter: int(PARAM_COUNTER).unwrap_or(0),
            checksum: int(checksum_key).unwrap_or(0),
            payload,
        })
    }

    pub fn checksum_valid(&self) -> bool {
        let key = if self.op == 0 { CHECKSUM_KEY_OP0 } else { CHECKSUM_KEY_OP1 };
        self.checksum == self.counter.wrapping_add(self.id) ^ key
    }

    pub fn reader(&self) -> AfcReader<'_> {
        AfcReader { data: &self.payload }
    }
}

pub fn outgoing_params(id: i32, payload: Vec<u8>) -> Params {
    Params::from([(1, Value::Int(id)), (3, Value::Bytes(payload))])
}

/// Reads a payload written by the client's `BinaryWriter`.
pub struct AfcReader<'a> {
    data: &'a [u8],
}

impl AfcReader<'_> {
    fn take(&mut self, count: usize) -> Result<&[u8]> {
        if self.data.len() < count {
            bail!("payload ended early");
        }
        let (head, tail) = self.data.split_at(count);
        self.data = tail;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.take(N)?.try_into()?)
    }

    pub fn remaining(&self) -> usize {
        self.data.len()
    }

    pub fn skip(&mut self, count: usize) -> Result<()> {
        self.take(count).map(|_| ())
    }

    pub fn int32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    pub fn int64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.array()?))
    }

    pub fn single(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.array()?))
    }

    /// An `i16` length followed by that many UTF-8 bytes.
    pub fn string(&mut self) -> Result<String> {
        let len = i16::from_le_bytes(self.array()?).max(0) as usize;
        Ok(String::from_utf8_lossy(self.take(len)?).into_owned())
    }
}

/// Builds a payload in the layout the client's `BinaryReader` expects.
#[derive(Default)]
pub struct AfcWriter {
    data: Vec<u8>,
}

impl AfcWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn boolean(&mut self, v: bool) -> &mut Self {
        self.data.push(u8::from(v));
        self
    }

    pub fn int16(&mut self, v: i16) -> &mut Self {
        self.data.extend(v.to_le_bytes());
        self
    }

    pub fn int32(&mut self, v: i32) -> &mut Self {
        self.data.extend(v.to_le_bytes());
        self
    }

    pub fn int64(&mut self, v: i64) -> &mut Self {
        self.data.extend(v.to_le_bytes());
        self
    }

    pub fn single(&mut self, v: f32) -> &mut Self {
        self.data.extend(v.to_le_bytes());
        self
    }

    pub fn double(&mut self, v: f64) -> &mut Self {
        self.data.extend(v.to_le_bytes());
        self
    }

    pub fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.data.extend(v);
        self
    }

    pub fn string(&mut self, v: &str) -> &mut Self {
        self.int16(v.len() as i16);
        self.bytes(v.as_bytes())
    }

    /// Six `i16` values: year, month, day, hour, minute, second.
    pub fn date_time(&mut self, t: NaiveDateTime) -> &mut Self {
        self.int16(t.year() as i16)
            .int16(t.month() as i16)
            .int16(t.day() as i16)
            .int16(t.hour() as i16)
            .int16(t.minute() as i16)
            .int16(t.second() as i16)
    }

    pub fn finish(self) -> Vec<u8> {
        self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_matches_the_client_formula() {
        let message = AfcMessage {
            op: 1,
            id: 90,
            counter: 5,
            checksum: 95 ^ CHECKSUM_KEY_OP1,
            payload: Vec::new(),
        };
        assert!(message.checksum_valid());
    }

    #[test]
    fn strings_round_trip() {
        let mut writer = AfcWriter::new();
        writer.string("Карп").int32(7);
        let payload = writer.finish();

        let mut reader = AfcReader { data: &payload };
        assert_eq!(reader.string().unwrap(), "Карп");
        assert_eq!(reader.int32().unwrap(), 7);
        assert_eq!(reader.remaining(), 0);
    }

    #[test]
    fn short_payload_is_an_error() {
        let mut reader = AfcReader { data: &[1, 2] };
        assert!(reader.int32().is_err());
    }
}
