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
    name: String,
    /// The `costWpn=` line: pieces of another item asked on top of the price,
    /// [item, pieces], or zeros.
    cost_item: [i32; 2],
    /// The `costPt=` line: the price in ergs.
    cost_pt: i64,
    /// Whether a shop buys the item back, whether the auction takes it, and
    /// whether it takes one that was built from parts.
    sale_to_shop: bool,
    sale_to_auk: bool,
    wpn_to_auk: bool,
    /// Whether the item wears at all (`isiznos=`) and what one use costs it
    /// (`iznos=`); `defDur` is the whole of it, this is the step.
    wears: bool,
    wear: f64,
    /// Slots for parts: the `holes_min=` and `holes_max=` lines.
    holes_min: i32,
    holes_max: i32,
    /// The `subtype=` and `viewType=` lines. The description carries the same
    /// two, but only for some items; these are on every one.
    subtype: i32,
    view_type: i32,
    /// Whether the item can be handed to another player.
    take_to_friend: bool,
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

/// A number from the definitions; the game leaves unset fields empty and
/// writes the fractional ones with a comma.
fn number(value: Option<&str>) -> Result<f64, std::num::ParseFloatError> {
    value.map(str::trim).filter(|text| !text.is_empty()).unwrap_or("0").replace(',', ".").parse()
}

/// A `0`/`1` line of the definitions.
fn flag(value: Option<&str>) -> bool {
    matches!(value.map(str::trim), Some("1"))
}

fn extract(assets: &[u8]) -> Result<Vec<Item>> {
    let id = pattern(r"(?m)^id=(\d+)");
    let kind = pattern(r"(?m)^wtype=(.*)$");
    let cost = pattern(r"(?m)^costCr=(.*)$");
    let shop_count = pattern(r"(?m)^shopCount=(.*)$");
    let max_count = pattern(r"(?m)^maxcount=(.*)$");
    let refine = pattern(r"(?m)^refine=(.*)$");
    let name = pattern(r"(?m)^iname=(.*)$");
    let cost_wpn = pattern(r"(?m)^costWpn=(.*)$");
    let cost_pt = pattern(r"(?m)^costPt=(.*)$");
    let sale_to_shop = pattern(r"(?m)^saleToShop=(.*)$");
    let sale_to_auk = pattern(r"(?m)^saleToAuk=(.*)$");
    let wpn_to_auk = pattern(r"(?m)^wpnToAuk=(.*)$");
    let wears = pattern(r"(?m)^isiznos=(.*)$");
    let wear = pattern(r"(?m)^iznos=(.*)$");
    let holes_min = pattern(r"(?m)^holes_min=(.*)$");
    let holes_max = pattern(r"(?m)^holes_max=(.*)$");
    let subtype = pattern(r"(?m)^subtype=(.*)$");
    let view_type = pattern(r"(?m)^viewType=(.*)$");
    let take_to_friend = pattern(r"(?m)^takeToFrend=(.*)$");
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
            name: capture(&name, block).unwrap_or("").trim().to_string(),
            cost_item: item_price(capture(&cost_wpn, block).unwrap_or("")),
            cost_pt: number(capture(&cost_pt, block)).context("цена предмета в эргах")? as i64,
            sale_to_shop: flag(capture(&sale_to_shop, block)),
            sale_to_auk: flag(capture(&sale_to_auk, block)),
            wpn_to_auk: flag(capture(&wpn_to_auk, block)),
            wears: flag(capture(&wears, block)),
            wear: number(capture(&wear, block)).context("износ предмета")?,
            holes_min: number(capture(&holes_min, block)).context("меньшее число ячеек")? as i32,
            holes_max: number(capture(&holes_max, block)).context("большее число ячеек")? as i32,
            subtype: number(capture(&subtype, block)).context("подвид предмета")? as i32,
            view_type: number(capture(&view_type, block)).context("вид показа предмета")? as i32,
            take_to_friend: flag(capture(&take_to_friend, block)),
        });
    }
    Ok(items)
}

/// "1179/60" or "1179/60/0": the item and how many; anything else is none.
fn item_price(text: &str) -> [i32; 2] {
    let mut fields = text.trim().split('/').map(|field| field.trim().parse::<i32>().unwrap_or(0));
    match (fields.next(), fields.next()) {
        (Some(item), Some(pieces)) if item > 0 && pieces > 0 => [item, pieces],
        _ => [0, 0],
    }
}

/// A file without this field was made by an older version and is made again.
const CURRENT_FIELD: &str = "\"sale_to_shop\"";

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

    const ASSETS: &[u8] = b"junk#head\nid=7\r\nwtype=10\r\ncostCr=12.5\r\nrefine=1391/1/100\r\niname=Box\r\ncostWpn=1179/60/0\r\ncostPt=0\r\nsaleToShop=1\r\nsaleToAuk=0\r\nwpnToAuk=0\r\nisiznos=1\r\niznos=0,01\r\nholes_min=0\r\nholes_max=2\r\nsubtype=3\r\nviewType=34\r\ntakeToFrend=1\r\naddpar=<wpn_add>\\r\\n<base> <name>x</name></base> <defDur>2.5</defDur></wpn_add>\nwpnend=1\nid=8\nwtype=3\naddpar=<wpn_add><base></base></wpn_add>\nwpnend=1\nend";

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
            name: "Box".into(),
            cost_item: [1179, 60],
            cost_pt: 0,
            sale_to_shop: true,
            sale_to_auk: false,
            wpn_to_auk: false,
            wears: true,
            wear: 0.01,
            holes_min: 0,
            holes_max: 2,
            subtype: 3,
            view_type: 34,
            take_to_friend: true,
        });
    }

    #[test]
    fn a_price_in_items_needs_both_numbers() {
        assert_eq!(item_price("1515/1"), [1515, 1]);
        assert_eq!(item_price("1094/2/1"), [1094, 2]);
        assert_eq!(item_price("0"), [0, 0]);
        assert_eq!(item_price(""), [0, 0]);
    }

    #[test]
    fn missing_fields_get_defaults() {
        let items = extract(ASSETS).unwrap();
        assert_eq!((items[1].cost, items[1].durability, items[1].stacks), (0, 1.0, false));
        assert_eq!((items[1].cost_pt, items[1].wear, items[1].holes_max), (0, 0.0, 0));
        assert!(!items[1].sale_to_shop && !items[1].wears && !items[1].take_to_friend);
    }

    #[test]
    fn assets_without_items_are_an_error() {
        assert!(extract(b"nothing here").is_err());
    }
}
