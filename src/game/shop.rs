//! The shop window. Every shop sells the whole catalogue: which trader stocked
//! what was decided on the shut-down server.

use anyhow::Result;

use super::{Conn, Outcome, Session, active, character, inventory};
use crate::afc::{AfcMessage, AfcWriter};
use crate::config::CatalogItem;
use crate::messages as msg;

const DEFAULT_CATEGORY: i32 = 10;

fn digits_as_numbers(text: &str) -> Vec<i32> {
    text.split(|c: char| !c.is_ascii_digit()).filter_map(|part| part.parse().ok()).collect()
}

/// Client: int shop, string categories ("10", "7,13,9,8"), as 1309 when the
/// window opens and 1300 when a tab is chosen.
///
/// Reply case 1300: int shop, int n, n x (int category, int item count), int m,
/// m x (int template, string "money|premium|item", 3 x double, bool).
pub async fn list(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let mut r = message.reader();
    let shop = r.int32()?;
    let wanted = digits_as_numbers(&r.string()?);

    let conn = &session.conn;
    let catalog = &conn.world.catalog;
    let mut listed: Vec<&CatalogItem> = catalog.for_sale().filter(|item| wanted.contains(&item.kind)).collect();
    if listed.is_empty() {
        listed = catalog.for_sale().filter(|item| item.kind == DEFAULT_CATEGORY).collect();
    }

    let mut categories: Vec<(i32, i32)> = Vec::new();
    for item in catalog.for_sale() {
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
        w.int32(item.id).string(&format!("{}|0|0", item.cost));
        w.double(0.0).double(0.0).double(0.0).boolean(false);
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
    let Some(item) = world.catalog.get(template).filter(|item| item.cost > 0) else {
        conn.log(&format!("   buy: item {template} is not for sale"));
        return Ok(Outcome::Unanswered);
    };

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
            conn.log(&format!("   -> 1310 bought {template} x{count}, money {}", player.data.money));
        }
    }
    Ok(Outcome::Answered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_parse_from_the_client_string() {
        assert_eq!(digits_as_numbers("7,13,9,8"), vec![7, 13, 9, 8]);
        assert!(digits_as_numbers("").is_empty());
    }
}
