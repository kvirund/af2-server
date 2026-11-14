//! The shop window. What each base sells comes from a player guide
//! (`config/shops.json`); with that switched off every priced item is for sale.

use anyhow::Result;

use super::{Conn, Outcome, Player, Session, active, character, crafting, inventory, travel};
use crate::afc::{AfcMessage, AfcWriter};
use crate::config::{CatalogItem, ShopSettings};
use crate::messages as msg;

const DEFAULT_CATEGORY: i32 = 10;

/// The level an item asks for, from its description. The client only colours it
/// in the tooltip, so the shop has to enforce it.
fn minimal_level(xml: &str) -> i32 {
    xml.split_once("<minimalLevel>")
        .and_then(|(_, rest)| rest.split('<').next())
        .and_then(|level| level.parse().ok())
        .unwrap_or(1)
}

fn level_allows(settings: &ShopSettings, level: i32, item: &CatalogItem) -> bool {
    !settings.enforce_levels || minimal_level(&item.xml) <= level
}

/// Priced items this character's base sells.
fn on_sale<'a>(conn: &'a Conn, player: &Player) -> Vec<&'a CatalogItem> {
    let world = &conn.world;
    let base = travel::current_base(&world.settings, player);
    let stock: Vec<&CatalogItem> = match world.settings.shop.stocked(base) {
        Some(ids) => ids
            .iter()
            .filter_map(|id| world.catalog.get(*id))
            .filter(|item| item.cost > 0 || item.premium_cost > 0)
            .collect(),
        None => world.catalog.for_sale().collect(),
    };
    // Recipes the character knows for good are left out: the client cannot say so.
    stock.into_iter().filter(|item| !crafting::teaches_nothing_new(&world.recipes, &player.data, item)).collect()
}

fn digits_as_numbers(text: &str) -> Vec<i32> {
    text.split(|c: char| !c.is_ascii_digit()).filter_map(|part| part.parse().ok()).collect()
}

/// Client: int shop, string categories ("10", "7,13,9,8"), as 1309 when the
/// window opens and 1300 when a tab is chosen.
///
/// Reply case 1300: int shop, int n, n x (int category, int item count), int m,
/// m x (int template, string "money|premium|item", 3 x double, bool can be bought). With `false` the client refuses the purchase.
pub async fn list(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let mut r = message.reader();
    let shop = r.int32()?;
    let wanted = digits_as_numbers(&r.string()?);

    let player = active(&mut session.player)?;
    let level = player.data.level;
    let conn = &session.conn;
    let stock = on_sale(conn, player);
    let mut listed: Vec<&CatalogItem> = stock.iter().copied().filter(|item| wanted.contains(&item.kind)).collect();
    if listed.is_empty() {
        listed = stock.iter().copied().filter(|item| item.kind == DEFAULT_CATEGORY).collect();
    }

    let mut categories: Vec<(i32, i32)> = Vec::new();
    for item in &stock {
        match categories.iter_mut().find(|(kind, _)| *kind == item.kind) {
            Some((_, count)) => *count += 1,
            None => categories.push((item.kind, 1)),
        }
    }

    let mut w = AfcWriter::new();
    w.int32(shop).int32(categories.len() as i32);
    for (kind, count) in &categories {
        w.int32(*kind).int32(*count);
    }
    w.int32(listed.len() as i32);
    for item in &listed {
        w.int32(item.id).string(&format!("{}|{}|0", item.cost, item.premium_cost));
        w.double(0.0).double(0.0).double(0.0).boolean(level_allows(&conn.world.settings.shop, level, item));
    }
    conn.send(msg::SHOP_LIST, w.finish()).await?;
    conn.log(&format!("   -> 1300 shop {shop}: {} items, {} categories", listed.len(), categories.len()));
    Ok(Outcome::Answered)
}

async fn send_status(conn: &Conn, status: &str) -> Result<()> {
    let mut w = AfcWriter::new();
    w.string(status);
    conn.send(msg::CLEAR_OVERLAY, w.finish()).await
}

/// Client: int shop, int template, int count. Reply: case 181 with the changed
/// items, case 21 with the money, case 1310 with a status that lowers the overlay.
pub async fn buy(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let mut r = message.reader();
    r.int32()?;
    let template = r.int32()?;
    let count = r.int32()?.max(1);

    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let world = conn.world.clone();
    let Some(item) = on_sale(conn, player).into_iter().find(|item| item.id == template) else {
        conn.log(&format!("   buy: item {template} is not for sale here"));
        return Ok(Outcome::Unanswered);
    };

    if !level_allows(&world.settings.shop, player.data.level, item) {
        send_status(conn, "none=0").await?;
        conn.log(&format!("   -> 1310 item {template} needs a higher level"));
        return Ok(Outcome::Answered);
    }

    let before = player.data.money;
    match inventory::buy(conn, player, item, count).await? {
        Err(error) => {
            send_status(conn, &format!("{}=0", error.status())).await?;
            conn.log(&format!("   -> 1310 cannot buy {template} x{count}: {}", error.status()));
        }
        Ok(changed) => {
            inventory::send_updates(conn, player.data.id, &changed).await?;
            character::send_money(conn, player, player.data.money - before).await?;
            send_status(conn, &format!("ok={template}/{count}/0")).await?;
            conn.log(&format!("   -> 1310 bought {template} x{count}, money {}, ergs {}", player.data.money, player.data.premium));
        }
    }
    Ok(Outcome::Answered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ShopStock;

    fn item(xml: &str) -> CatalogItem {
        CatalogItem { id: 1, kind: 1, cost: 1, durability: 1.0, stacks: false, shop_count: 1, max_count: 1, premium_cost: 0, xml: xml.to_owned(), refine: String::new(), name: String::new() }
    }

    #[test]
    fn an_item_waits_for_its_level() {
        let settings = ShopSettings { enforce_levels: true, ..Default::default() };
        let rod = item("<base><minimalLevel>10</minimalLevel></base>");
        assert!(!level_allows(&settings, 9, &rod));
        assert!(level_allows(&settings, 10, &rod));
        assert!(level_allows(&settings, 1, &item("<base></base>")));
    }

    #[test]
    fn levels_can_be_switched_off() {
        let settings = ShopSettings { enforce_levels: false, ..Default::default() };
        assert!(level_allows(&settings, 1, &item("<minimalLevel>40</minimalLevel>")));
    }

    #[test]
    fn a_base_without_a_list_sells_the_fallback() {
        let mut settings = ShopSettings { guide_stock: true, ..Default::default() };
        settings.stock = ShopStock { fallback_base: 1, bases: [(1, vec![10]), (6, vec![20])].into() };
        assert_eq!(settings.stocked(6), Some(&[20][..]));
        assert_eq!(settings.stocked(9), Some(&[10][..]));
        settings.guide_stock = false;
        assert_eq!(settings.stocked(6), None);
    }

    #[test]
    fn categories_parse_from_the_client_string() {
        assert_eq!(digits_as_numbers("7,13,9,8"), vec![7, 13, 9, 8]);
        assert!(digits_as_numbers("").is_empty());
    }
}
