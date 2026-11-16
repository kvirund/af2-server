//! The shop window. What each base sells comes from a player guide
//! (`config/shops.json`); with that switched off every priced item is for sale.

use anyhow::Result;

use super::{Conn, Outcome, Player, Session, active, character, chat, crafting, inventory, reputation, travel};
use crate::afc::{AfcMessage, AfcWriter};
use crate::config::{CatalogItem, ReputationSettings, ShopSettings};
use crate::storage::CharacterData;
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

pub(super) fn level_allows(settings: &ShopSettings, level: i32, item: &CatalogItem) -> bool {
    !settings.enforce_levels || minimal_level(&item.xml) <= level
}

/// The faction and level an item is sold from (`reputation` in shops.json),
/// if the character is not there yet.
fn reputation_short(shop: &ShopSettings, factions: &ReputationSettings, character: &CharacterData, item: &CatalogItem) -> Option<[i32; 2]> {
    let [faction, level] = *shop.stock.reputation.get(&item.id)?;
    (reputation::level_with(factions, character, faction) < level).then_some([faction, level])
}

/// Whether the base's shops sell the item.
pub(super) fn sells(world: &crate::world::World, base: i32, template: i32) -> bool {
    let priced = world.catalog.get(template).is_some_and(|item| item.cost > 0 || item.premium_cost > 0 || item.cost_item[0] > 0);
    priced && world.settings.shop.stocked(base).is_none_or(|ids| ids.contains(&template))
}

/// The level an item asks for.
pub(super) fn level_of(item: &CatalogItem) -> i32 {
    minimal_level(&item.xml)
}

/// Priced items this character's base sells.
fn on_sale<'a>(conn: &'a Conn, player: &Player) -> Vec<&'a CatalogItem> {
    let world = &conn.world;
    let base = travel::current_base(&world.settings, player);
    let stock: Vec<&CatalogItem> = match world.settings.shop.stocked(base) {
        Some(ids) => ids
            .iter()
            .filter_map(|id| world.catalog.get(*id))
            .filter(|item| item.cost > 0 || item.premium_cost > 0 || item.cost_item[0] > 0)
            .collect(),
        None => world.catalog.for_sale().collect(),
    };
    // Recipes the character knows for good are left out: the client cannot say so.
    stock.into_iter().filter(|item| !crafting::teaches_nothing_new(&world.recipes, &player.data, item)).collect()
}

/// What else the item costs: [item, pieces] per purchase, or `None`.
fn item_price(settings: &ShopSettings, item: &CatalogItem) -> Option<[i32; 2]> {
    let [template, pieces] = item.cost_item;
    (template > 0 && pieces > 0 && !settings.no_item_price.contains(&item.id)).then_some([template, pieces])
}

/// "money|premium|item/pieces", as the client splits it (it reads '/' as '|').
fn price_text(settings: &ShopSettings, item: &CatalogItem) -> String {
    match item_price(settings, item) {
        Some([template, pieces]) => format!("{}|{}|{template}/{pieces}", item.cost, item.premium_cost),
        None => format!("{}|{}|0", item.cost, item.premium_cost),
    }
}

fn digits_as_numbers(text: &str) -> Vec<i32> {
    text.split(|c: char| !c.is_ascii_digit()).filter_map(|part| part.parse().ok()).collect()
}

/// Client: int shop, string categories ("10", "7,13,9,8"), as 1309 when the
/// window opens and 1300 when a tab is chosen.
///
/// Reply case 1300: int shop, int n, n x (int category, int item count), int m,
/// m x (int template, string "money|premium|item/pieces", 3 x double, bool can be
/// bought). With `false` the client refuses the purchase. The pieces of the item
/// (Trophy Hunter's tokens, mostly) are asked together with the money.
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
    let settings = &conn.world.settings;
    for item in &listed {
        let allowed = level_allows(&settings.shop, level, item) && reputation_short(&settings.shop, &settings.reputation, &player.data, item).is_none();
        w.int32(item.id).string(&price_text(&settings.shop, item));
        w.double(0.0).double(0.0).double(0.0).boolean(allowed);
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
    if let Some([faction, level]) = reputation_short(&world.settings.shop, &world.settings.reputation, &player.data, item) {
        let name = world.settings.reputation.factions.get(&faction).map_or(String::new(), |found| found.name.clone());
        chat::system(conn, &format!("Нужна репутация «{name}»: {level}-й уровень.")).await?;
        send_status(conn, "none=0").await?;
        conn.log(&format!("   -> 1310 item {template} needs level {level} with faction {faction}"));
        return Ok(Outcome::Answered);
    }

    let wanted = item_price(&world.settings.shop, item).map(|[template, pieces]| (template, pieces * count));
    if let Some((template, pieces)) = wanted
        && inventory::pieces_carried(&player.data, template) < pieces
    {
        send_status(conn, "no_money=0").await?;
        conn.log(&format!("   -> 1310 buying {} x{count} needs {pieces} of item {template}", item.id));
        return Ok(Outcome::Answered);
    }

    let before = player.data.money;
    match inventory::buy(conn, player, item, count).await? {
        Err(error) => {
            send_status(conn, &format!("{}=0", error.status())).await?;
            conn.log(&format!("   -> 1310 cannot buy {template} x{count}: {}", error.status()));
        }
        Ok(changed) => {
            if let Some((template, pieces)) = wanted
                && let Some((smaller, gone)) = inventory::take_carried(&mut player.data, template, pieces)
            {
                conn.save(player).await?;
                for id in gone {
                    inventory::send_removed(conn, id).await?;
                }
                inventory::send_updates(conn, player.data.id, &smaller).await?;
                conn.log(&format!("   -> 181/190 paid {pieces} of item {template}"));
            }
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
        CatalogItem { id: 1, kind: 1, cost: 1, durability: 1.0, stacks: false, shop_count: 1, max_count: 1, premium_cost: 0, xml: xml.to_owned(), refine: String::new(), name: String::new(), cost_item: [0, 0] }
    }

    #[test]
    fn tokens_go_with_the_money_unless_switched_off() {
        let line = CatalogItem { id: 1472, cost: 21000, cost_item: [1179, 60], ..item("") };
        assert_eq!(price_text(&ShopSettings::default(), &line), "21000|0|1179/60");
        let off = ShopSettings { no_item_price: vec![1472], ..Default::default() };
        assert_eq!(price_text(&off, &line), "21000|0|0");
        assert_eq!(item_price(&off, &line), None);
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
        settings.stock = ShopStock { fallback_base: 1, bases: [(1, vec![10]), (6, vec![20])].into(), ..Default::default() };
        assert_eq!(settings.stocked(6), Some(&[20][..]));
        assert_eq!(settings.stocked(9), Some(&[10][..]));
        settings.guide_stock = false;
        assert_eq!(settings.stocked(6), None);
    }

    #[test]
    fn some_goods_wait_for_reputation() {
        use crate::config::FactionSettings;
        let mut shop = ShopSettings::default();
        shop.stock.reputation.insert(1402, [1002, 2]);
        let mut factions = ReputationSettings::default();
        factions.factions.insert(1002, FactionSettings { start_a: 100, start_b: 0, kf_b: 0.0, levels: 6, name: String::new() });
        let mut character = CharacterData::default();
        let rod = CatalogItem { id: 1402, ..item("") };
        assert_eq!(reputation_short(&shop, &factions, &character, &rod), Some([1002, 2]));
        character.reputation.insert(1002, 200);
        assert_eq!(reputation_short(&shop, &factions, &character, &rod), None);
        assert_eq!(reputation_short(&shop, &factions, &character, &item("")), None);
    }

    #[test]
    fn categories_parse_from_the_client_string() {
        assert_eq!(digits_as_numbers("7,13,9,8"), vec![7, 13, 9, 8]);
        assert!(digits_as_numbers("").is_empty());
    }
}
