//! Builds `data/items.json` from the player's own copy of the game. The item
//! definitions are not ours to publish, so every installation extracts them.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use regex::Regex;
use regex::bytes::Regex as BytesRegex;
use serde::Serialize;

/// Item types that pile up in one cell.
const STACKING_KINDS: [i32; 2] = [10, 11];

#[derive(Debug, PartialEq, Serialize)]
struct Item {
    id: i32,
    kind: i32,
    cost: i64,
    durability: f64,
    stacks: bool,
    shop_count: i32,
    max_count: i32,
    xml: String,
    /// What «Переработать» gives, as the client's refine window shows it.
    refine: String,
}

fn pattern(text: &str) -> Regex {
    Regex::new(text).expect("the patterns in this file are valid")
}

fn capture<'a>(regex: &Regex, text: &'a str) -> Option<&'a str> {
    Some(regex.captures(text)?.get(1)?.as_str())
}

/// The part of the asset file holding every definition, as text.
fn definitions_text(assets: &[u8]) -> Result<String> {
    let definition = BytesRegex::new(r"(?s)<wpn_add\b.*?</wpn_add>").expect("valid pattern");
    let mut matches = definition.find_iter(assets);
    let Some(first) = matches.next() else {
        bail!("в resources.assets нет описаний предметов");
    };
    let end = matches.last().map_or(first.end(), |last| last.end());

    let before = &assets[..first.start()];
    let id_line = before.windows(4).rposition(|w| w == b"\nid=").context("не найдено начало описаний")?;
    let start = before[..id_line].iter().rposition(|b| *b == b'#').context("не найдено начало описаний")?;
    Ok(String::from_utf8_lossy(&assets[start..end]).replace('\r', ""))
}

/// A number from the definitions; the game leaves unset fields empty.
fn number(value: Option<&str>) -> Result<f64, std::num::ParseFloatError> {
    value.map(str::trim).filter(|text| !text.is_empty()).unwrap_or("0").parse()
}

fn extract(assets: &[u8]) -> Result<Vec<Item>> {
    let id = pattern(r"(?m)^id=(\d+)");
    let kind = pattern(r"(?m)^wtype=(.*)$");
    let cost = pattern(r"(?m)^costCr=(.*)$");
    let shop_count = pattern(r"(?m)^shopCount=(.*)$");
    let max_count = pattern(r"(?m)^maxcount=(.*)$");
    let refine = pattern(r"(?m)^refine=(.*)$");
    let definition = pattern(r"(?s)<wpn_add.*?</wpn_add>");
    let durability = pattern(r"(?s)<defDur>(.*?)</defDur>");
    let line_breaks = pattern(r"\\[rn]");
    let between_tags = pattern(r">\s+<");

    let mut items = Vec::new();
    for block in definitions_text(assets)?.split("wpnend=1") {
        let Some(item_id) = capture(&id, block) else { continue };
        let item_id: i32 = item_id.parse().context("номер предмета")?;

        let xml = definition.find(block).map_or("", |found| found.as_str());
        let xml = line_breaks.replace_all(xml, "");
        let xml = between_tags.replace_all(xml.trim(), "><");
        let xml = xml.replacen("<base>", &format!("<base><tid>{item_id}</tid><quality>0</quality>"), 1);

        let item_kind = number(capture(&kind, block)).context("тип предмета")? as i32;
        let item_cost = number(capture(&cost, block)).context("цена предмета")? as i64;
        let in_shop = number(capture(&shop_count, block)).context("размер упаковки")? as i32;
        let in_cell = number(capture(&max_count, block)).context("размер стопки")? as i32;
        let item_durability = match capture(&durability, &xml).filter(|text| !text.is_empty()) {
            Some(text) => text.trim().parse().context("прочность предмета")?,
            None => 1.0,
        };

        items.push(Item {
            id: item_id,
            kind: item_kind,
            cost: item_cost,
            durability: item_durability,
            stacks: STACKING_KINDS.contains(&item_kind) || in_cell > 1,
            shop_count: in_shop.max(1),
            max_count: in_cell.max(1),
            xml,
            refine: capture(&refine, block).unwrap_or("").trim().to_string(),
        });
    }
    Ok(items)
}

/// A file without this field was made by an older version and is made again.
const CURRENT_FIELD: &str = "\"refine\"";

/// Does nothing once the file exists: it only depends on the installed game.
pub fn ensure_extracted(game: &Path, data_dir: &Path) -> Result<()> {
    let output = data_dir.join("items.json");
    if fs::read_to_string(&output).is_ok_and(|text| text.contains(CURRENT_FIELD)) {
        return Ok(());
    }

    let assets_path = game.join("AFCGame_Data").join("resources.assets");
    let assets = fs::read(&assets_path)
        .with_context(|| format!("не удалось прочитать {}", assets_path.display()))?;
    let items = extract(&assets)?;

    fs::create_dir_all(data_dir)?;
    fs::write(&output, serde_json::to_string(&items)?)?;
    println!("Описания предметов извлечены из игры: {}.", items.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASSETS: &[u8] = b"junk#head\nid=7\r\nwtype=10\r\ncostCr=12.5\r\nrefine=1391/1/100\r\naddpar=<wpn_add>\\r\\n<base> <name>x</name></base> <defDur>2.5</defDur></wpn_add>\nwpnend=1\nid=8\nwtype=3\naddpar=<wpn_add><base></base></wpn_add>\nwpnend=1\nend";

    #[test]
    fn extracts_every_field() {
        let items = extract(ASSETS).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], Item {
            id: 7,
            kind: 10,
            cost: 12,
            durability: 2.5,
            stacks: true,
            shop_count: 1,
            max_count: 1,
            xml: "<wpn_add><base><tid>7</tid><quality>0</quality><name>x</name></base><defDur>2.5</defDur></wpn_add>".into(),
            refine: "1391/1/100".into(),
        });
    }

    #[test]
    fn missing_fields_get_defaults() {
        let items = extract(ASSETS).unwrap();
        assert_eq!((items[1].cost, items[1].durability, items[1].stacks), (0, 1.0, false));
    }

    #[test]
    fn assets_without_items_are_an_error() {
        assert!(extract(b"nothing here").is_err());
    }
}
