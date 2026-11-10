//! Rewrites the server address inside the client's `Assembly-CSharp.dll`.
//!
//! The address is a string in the .NET metadata `#US` heap, stored as a length
//! byte, UTF-16LE characters and one terminator byte. A shorter address fits in
//! the same space, so nothing else in the file moves. The assembly is not
//! strong-named, so no signature needs fixing.

use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub const ORIGINAL_HOST: &str = "gamesrv1.af2.cryptodev.app";
pub const MAX_HOST_LEN: usize = ORIGINAL_HOST.len();

/// Bytes after the length byte: the original characters and the terminator.
const SLOT_CAPACITY: usize = MAX_HOST_LEN * 2 + 1;

/// Where the address sits in the game's final build. An already patched file no
/// longer contains the original text to search for.
const KNOWN_SLOT_OFFSET: usize = 0xB1C521;

pub fn assembly_path(game: &Path) -> PathBuf {
    game.join("AFCGame_Data").join("Managed").join("Assembly-CSharp.dll")
}

/// Letters, digits, dots and dashes: the client supplies the port itself.
pub fn validate_host(host: &str) -> Result<()> {
    if host.is_empty() || host.len() > MAX_HOST_LEN {
        bail!("адрес должен быть длиной от 1 до {MAX_HOST_LEN} символов");
    }
    if !host.bytes().all(is_host_byte) {
        bail!("в адресе допустимы только латинские буквы, цифры, точка и дефис; порт всегда 9201");
    }
    Ok(())
}

fn is_host_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-'
}

fn utf16(text: &str) -> Vec<u8> {
    text.bytes().flat_map(|byte| [byte, 0]).collect()
}

/// The length byte and every byte of the slot, padded with zeros.
fn encode_slot(host: &str) -> Vec<u8> {
    let characters = utf16(host);
    let mut slot = vec![(characters.len() + 1) as u8];
    slot.extend(&characters);
    slot.resize(1 + SLOT_CAPACITY, 0);
    slot
}

fn find_original(data: &[u8]) -> Option<usize> {
    let needle = utf16(ORIGINAL_HOST);
    let mut hits = data.windows(needle.len()).enumerate().filter(|(_, window)| *window == needle);
    let (offset, _) = hits.next()?;
    let unique = hits.next().is_none();
    let prefixed = offset > 0 && usize::from(data[offset - 1]) == SLOT_CAPACITY;
    (unique && prefixed).then_some(offset)
}

/// The address stored at `offset`, if the bytes there really are one.
fn host_at(data: &[u8], offset: usize) -> Option<String> {
    let slot = data.get(offset - 1..offset + SLOT_CAPACITY)?;
    let total = usize::from(slot[0]);
    if total % 2 == 0 || !(3..=SLOT_CAPACITY).contains(&total) {
        return None;
    }
    let (characters, rest) = slot[1..].split_at(total - 1);
    if rest.iter().any(|byte| *byte != 0) {
        return None;
    }
    let mut host = String::with_capacity(characters.len() / 2);
    for pair in characters.chunks_exact(2) {
        if pair[1] != 0 || !is_host_byte(pair[0]) {
            return None;
        }
        host.push(char::from(pair[0]));
    }
    Some(host)
}

fn locate(data: &[u8]) -> Result<usize> {
    if let Some(offset) = find_original(data) {
        return Ok(offset);
    }
    if host_at(data, KNOWN_SLOT_OFFSET).is_some() {
        return Ok(KNOWN_SLOT_OFFSET);
    }
    bail!("не удалось найти адрес сервера в клиенте; проверь целостность файлов игры в Steam")
}

fn read(dll: &Path) -> Result<Vec<u8>> {
    fs::read(dll).with_context(|| format!("не удалось прочитать {}", dll.display()))
}

pub fn current_host(dll: &Path) -> Result<String> {
    let data = read(dll)?;
    let offset = locate(&data)?;
    host_at(&data, offset).context("адрес в клиенте повреждён; проверь целостность файлов игры в Steam")
}

pub fn set_host(dll: &Path, host: &str) -> Result<()> {
    validate_host(host)?;
    let offset = locate(&read(dll)?)?;
    write_at(dll, offset as u64 - 1, &encode_slot(host)).with_context(|| {
        format!("не удалось записать {}; закрой игру или запусти лаунчер от администратора", dll.display())
    })
}

fn write_at(path: &Path, position: u64, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = OpenOptions::new().write(true).open(path)?;
    file.seek(SeekFrom::Start(position))?;
    file.write_all(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client_image() -> (Vec<u8>, usize) {
        let mut data = vec![0xAB; 100];
        let offset = data.len() + 1;
        data.extend(encode_slot(ORIGINAL_HOST));
        data.extend([0xCD; 50]);
        (data, offset)
    }

    #[test]
    fn finds_the_original_address() {
        let (data, offset) = client_image();
        assert_eq!(locate(&data).unwrap(), offset);
        assert_eq!(host_at(&data, offset).unwrap(), ORIGINAL_HOST);
    }

    #[test]
    fn shorter_address_keeps_the_neighbours() {
        let (mut data, offset) = client_image();
        let before = data.clone();
        data[offset - 1..offset + SLOT_CAPACITY].copy_from_slice(&encode_slot("127.0.0.1"));

        assert_eq!(host_at(&data, offset).unwrap(), "127.0.0.1");
        assert_eq!(data[..offset - 1], before[..offset - 1]);
        assert_eq!(data[offset + SLOT_CAPACITY..], before[offset + SLOT_CAPACITY..]);
    }

    #[test]
    fn restoring_is_byte_exact() {
        let (original, offset) = client_image();
        let mut data = original.clone();
        data[offset - 1..offset + SLOT_CAPACITY].copy_from_slice(&encode_slot("10.0.0.1"));
        data[offset - 1..offset + SLOT_CAPACITY].copy_from_slice(&encode_slot(ORIGINAL_HOST));
        assert_eq!(data, original);
    }

    #[test]
    fn unrelated_bytes_are_not_an_address() {
        assert_eq!(host_at(&[0xFF; 100], 10), None);
    }

    #[test]
    fn hosts_are_validated() {
        assert!(validate_host("af2.kvirund.dev").is_ok());
        assert!(validate_host(ORIGINAL_HOST).is_ok());
        assert!(validate_host("").is_err());
        assert!(validate_host("host:9201").is_err());
        assert!(validate_host("сервер.рф").is_err());
        assert!(validate_host(&"a".repeat(MAX_HOST_LEN + 1)).is_err());
    }
}
