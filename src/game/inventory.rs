//! A character's items. Bags below 50 are inventory pages, cells are row * 9 + column.

use anyhow::Result;
use chrono::{Local, Months};

use super::{Conn, Outcome, Player, Session, active};
use crate::afc::{AfcMessage, AfcWriter};
use crate::config::CatalogItem;
use crate::messages as msg;
use crate::storage::{CharacterData, ItemEntry};

/// The client's request for "any free inventory cell".
const ANY_BAG: i32 = -1;
const BACKPACK_BAG: i32 = 0;
const BACKPACK_CELLS: i32 = 45;
const FIRST_NON_INVENTORY_BAG: i32 = 50;
const ITEM_EXPIRY_MONTHS: u32 = 120;

/// The item record shared by cases 180 and 181.
fn write_item(w: &mut AfcWriter, conn: &Conn, owner: i64, item: &ItemEntry) {
    let xml = conn.world.catalog.get(item.template).map_or("", |template| template.xml.as_str());
    let now = Local::now().naive_local();
    let expiry = now.checked_add_months(Months::new(ITEM_EXPIRY_MONTHS)).unwrap_or(now);

    w.int64(item.id).int64(owner).int32(0).int64(0);
    w.int32(item.bag).int32(item.cell).int32(item.count);
    w.single(item.durability).boolean(false);
    w.date_time(expiry);
    w.string(xml);
}

pub async fn send_all(conn: &Conn, player: &Player) -> Result<()> {
    let mut w = AfcWriter::new();
    for item in &player.data.items {
        write_item(&mut w, conn, player.data.id, item);
    }
    conn.send(msg::ITEMS, w.finish()).await
}

pub async fn send_updates(conn: &Conn, owner: i64, items: &[ItemEntry]) -> Result<()> {
    let mut w = AfcWriter::new();
    for item in items {
        write_item(&mut w, conn, owner, item);
    }
    conn.send(msg::ITEM_UPDATES, w.finish()).await
}

fn free_cell(character: &CharacterData, moving: Option<i64>, also_used: &[i32]) -> Option<i32> {
    let taken = |cell: i32| {
        also_used.contains(&cell)
            || character.items.iter().any(|item| {
                item.bag == BACKPACK_BAG && item.cell == cell && Some(item.id) != moving
            })
    };
    (0..BACKPACK_CELLS).find(|cell| !taken(*cell))
}

/// Client: long item, int bag, int cell. Bag -1 means any free backpack cell.
pub async fn move_item(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let mut r = message.reader();
    let (id, mut bag, mut cell) = (r.int64()?, r.int32()?, r.int32()?);
    let player = active(&mut session.player)?;
    let conn = &session.conn;

    if bag == ANY_BAG {
        let Some(free) = free_cell(&player.data, Some(id), &[]) else {
            conn.log(&format!("   move: no free cell for item {id}"));
            return Ok(Outcome::Unanswered);
        };
        (bag, cell) = (BACKPACK_BAG, free);
    }

    let Some(item) = player.data.items.iter_mut().find(|item| item.id == id) else {
        conn.log(&format!("   move: character has no item {id}"));
        return Ok(Outcome::Unanswered);
    };
    (item.bag, item.cell) = (bag, cell);
    let moved = item.clone();

    conn.save(player).await?;
    send_updates(conn, player.data.id, &[moved]).await?;
    conn.log(&format!("   -> 181 item {id} now at {bag}/{cell}"));
    Ok(Outcome::Answered)
}

/// Exchanges two items' places; `None` if the character lacks either.
pub fn swap(character: &mut CharacterData, first: i64, second: i64) -> Option<[ItemEntry; 2]> {
    let a = character.items.iter().position(|item| item.id == first)?;
    let b = character.items.iter().position(|item| item.id == second)?;
    let (a_place, b_place) = (
        (character.items[a].bag, character.items[a].cell),
        (character.items[b].bag, character.items[b].cell),
    );
    (character.items[a].bag, character.items[a].cell) = b_place;
    (character.items[b].bag, character.items[b].cell) = a_place;
    Some([character.items[a].clone(), character.items[b].clone()])
}

/// Client: long item, long item.
pub async fn swap_items(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let mut r = message.reader();
    let (first, second) = (r.int64()?, r.int64()?);
    let player = active(&mut session.player)?;
    let conn = &session.conn;

    let Some(changed) = swap(&mut player.data, first, second) else {
        conn.log(&format!("   swap: character lacks {first} or {second}"));
        return Ok(Outcome::Unanswered);
    };
    conn.save(player).await?;
    send_updates(conn, player.data.id, &changed).await?;
    conn.log(&format!("   -> 181 swapped items {first} and {second}"));
    Ok(Outcome::Answered)
}

/// Why a purchase failed, as the status word the client shows.
#[derive(Debug, PartialEq, Eq)]
pub enum BuyError {
    NoMoney,
    NoRoom,
}

impl BuyError {
    pub fn status(&self) -> &'static str {
        match self {
            Self::NoMoney => "no_money",
            Self::NoRoom => "no_inventory",
        }
    }
}

/// The price of a purchase and where its pieces go, decided before anything changes.
#[derive(Debug, PartialEq, Eq)]
pub enum Purchase {
    AddToStack { index: usize },
    NewPieces { cells: Vec<i32> },
}

pub fn plan_purchase(character: &CharacterData, item: &CatalogItem, count: i32) -> Result<Purchase, BuyError> {
    if item.cost.saturating_mul(i64::from(count)) > character.money {
        return Err(BuyError::NoMoney);
    }

    if item.stacks {
        let stack = character
            .items
            .iter()
            .position(|held| held.template == item.id && held.bag < FIRST_NON_INVENTORY_BAG);
        if let Some(index) = stack {
            return Ok(Purchase::AddToStack { index });
        }
    }

    let pieces = if item.stacks { 1 } else { count };
    let mut cells = Vec::new();
    for _ in 0..pieces {
        cells.push(free_cell(character, None, &cells).ok_or(BuyError::NoRoom)?);
    }
    Ok(Purchase::NewPieces { cells })
}

/// Pays for and hands over the items, returning those that changed.
pub async fn buy(
    conn: &Conn,
    player: &mut Player,
    item: &CatalogItem,
    count: i32,
) -> Result<Result<Vec<ItemEntry>, BuyError>> {
    let purchase = match plan_purchase(&player.data, item, count) {
        Ok(purchase) => purchase,
        Err(error) => return Ok(Err(error)),
    };

    let changed = match purchase {
        Purchase::AddToStack { index } => {
            player.data.items[index].count += count;
            vec![player.data.items[index].clone()]
        }
        Purchase::NewPieces { cells } => {
            let mut pieces = Vec::with_capacity(cells.len());
            for cell in cells {
                pieces.push(ItemEntry {
                    id: conn.world.store.next_id().await?,
                    bag: BACKPACK_BAG,
                    cell,
                    count: if item.stacks { count } else { 1 },
                    durability: item.durability,
                    template: item.id,
                });
            }
            player.data.items.extend(pieces.iter().cloned());
            pieces
        }
    };

    player.data.money -= item.cost * i64::from(count);
    conn.save(player).await?;
    Ok(Ok(changed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog_item(stacks: bool, cost: i64) -> CatalogItem {
        CatalogItem { id: 10, kind: 10, cost, durability: 1.0, stacks, xml: String::new() }
    }

    fn held(id: i64, cell: i32, template: i32) -> ItemEntry {
        ItemEntry { id, bag: BACKPACK_BAG, cell, count: 1, durability: 1.0, template }
    }

    #[test]
    fn stacks_join_an_existing_stack() {
        let character = CharacterData { money: 100, items: vec![held(1, 0, 10)], ..Default::default() };
        let plan = plan_purchase(&character, &catalog_item(true, 1), 5);
        assert_eq!(plan, Ok(Purchase::AddToStack { index: 0 }));
    }

    #[test]
    fn single_items_take_free_cells_in_order() {
        let character = CharacterData { money: 100, items: vec![held(1, 0, 99)], ..Default::default() };
        let plan = plan_purchase(&character, &catalog_item(false, 1), 2);
        assert_eq!(plan, Ok(Purchase::NewPieces { cells: vec![1, 2] }));
    }

    #[test]
    fn cannot_buy_past_the_balance() {
        let character = CharacterData { money: 4, ..Default::default() };
        assert_eq!(plan_purchase(&character, &catalog_item(true, 1), 5), Err(BuyError::NoMoney));
    }

    #[test]
    fn full_backpack_refuses() {
        let items = (0..BACKPACK_CELLS).map(|cell| held(i64::from(cell), cell, 99)).collect();
        let character = CharacterData { money: 100, items, ..Default::default() };
        assert_eq!(plan_purchase(&character, &catalog_item(false, 1), 1), Err(BuyError::NoRoom));
    }

    #[test]
    fn swap_exchanges_places() {
        let mut character = CharacterData { items: vec![held(1, 0, 9), held(2, 5, 9)], ..Default::default() };
        swap(&mut character, 1, 2).unwrap();
        assert_eq!((character.items[0].cell, character.items[1].cell), (5, 0));
    }
}
