//! A character's items. Bags 1 and up are the pages of the inventory window, 52 is
//! the warehouse, 60 and 100 are worn gear.

use anyhow::Result;
use chrono::{Local, Months};

use super::{Conn, Outcome, Player, Session, active, character, crafting, panels, perks};
use crate::afc::{AfcMessage, AfcWriter};
use crate::config::{BagSettings, CatalogItem};
use crate::messages as msg;
use crate::storage::{CharacterData, ItemEntry};
use crate::world::World;

/// The client's request for "any free inventory cell".
const ANY_BAG: i32 = -1;
const OLD_BACKPACK_BAG: i32 = 0;
const FIRST_NON_INVENTORY_BAG: i32 = 50;
const ITEM_EXPIRY_MONTHS: u32 = 120;

/// Lines: `basePar` is test/diameter/length, and the client reads the length
/// of what is left on the spool from the third number.
const LINE_KIND: i32 = 5;
const LINE_LENGTH_SLOT: usize = 2;

/// Where a kind keeps the piece's own `param` in `basePar`: a fish card's
/// species in the first number, a line's length left in the third.
fn param_slot(kind: i32) -> usize {
    if kind == LINE_KIND { LINE_LENGTH_SLOT } else { 0 }
}

/// The span of the `slot`-th number of `basePar`, if there is one.
fn base_par_span(xml: &str, slot: usize) -> Option<(usize, usize)> {
    const OPEN: &str = "<basePar>";
    let mut start = xml.find(OPEN)? + OPEN.len();
    for _ in 0..slot {
        let next = xml[start..].find(['/', '<'])?;
        if xml[start + next..].starts_with('<') {
            return None;
        }
        start += next + 1;
    }
    let end = xml[start..].find(['/', '<']).map_or(xml.len(), |at| start + at);
    Some((start, end))
}

/// The description with the piece's own `param` in place of the `slot`-th
/// number of `basePar`; 0 leaves the description as it is.
fn with_param(xml: &str, param: i32, slot: usize) -> std::borrow::Cow<'_, str> {
    let Some((start, end)) = base_par_span(xml, slot).filter(|_| param != 0) else {
        return xml.into();
    };
    format!("{}{param}{}", &xml[..start], &xml[end..]).into()
}

/// A line's length in metres as its description gives it.
fn template_length(xml: &str) -> Option<f64> {
    let (start, end) = base_par_span(xml, LINE_LENGTH_SLOT)?;
    xml[start..end].trim().parse().ok()
}

/// The piece's own description: its template's, with what it was built from
/// and its `param` written in.
pub fn item_xml(world: &World, item: &ItemEntry) -> String {
    let template = world.catalog.get(item.template);
    let xml = template.map_or("", |template| template.xml.as_str());
    let built = crafting::built_xml(&world.settings, &world.recipes, xml, item.template, item.parts);
    let xml = built.as_deref().unwrap_or(xml);
    with_param(xml, item.param, param_slot(template.map_or(0, |template| template.kind))).into_owned()
}

/// The item record shared by cases 180 and 181.
fn write_item(w: &mut AfcWriter, conn: &Conn, owner: i64, item: &ItemEntry) {
    let xml = item_xml(&conn.world, item);
    let now = Local::now().naive_local();
    let expiry = now.checked_add_months(Months::new(ITEM_EXPIRY_MONTHS)).unwrap_or(now);

    w.int64(item.id).int64(owner).int32(0).int64(0);
    w.int32(item.bag).int32(item.cell).int32(item.count);
    w.single(item.durability).boolean(false);
    w.date_time(expiry);
    w.string(&xml);
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

/// A free cell on the page for the item's kind. A full page refuses the item:
/// more room is bought with that page's "+".
fn free_place(
    settings: &BagSettings,
    character: &CharacterData,
    kind: i32,
    moving: Option<i64>,
    also_used: &[(i32, i32)],
) -> Option<(i32, i32)> {
    let page = settings.page_for(kind);
    let taken = |cell: i32| {
        also_used.contains(&(page, cell))
            || character.items.iter().any(|item| item.bag == page && item.cell == cell && Some(item.id) != moving)
    };
    (0..panels::cells_in(settings, character, page)).find(|cell| !taken(*cell)).map(|cell| (page, cell))
}

/// Puts a whole item as it is (its id, wear and parts) into a free cell of the
/// page for its kind; gives it back when the page is full.
pub fn put_in_free_cell(settings: &BagSettings, character: &mut CharacterData, mut item: ItemEntry, kind: i32) -> Result<ItemEntry, ItemEntry> {
    let Some((bag, cell)) = free_place(settings, character, kind, None, &[]) else {
        return Err(item);
    };
    (item.bag, item.cell) = (bag, cell);
    character.items.push(item.clone());
    Ok(item)
}

/// Older saves kept loose items in bag 0, which the client never draws, and
/// some kinds on the other page («Разное») before they had a page of their own.
/// An item whose page is full waits where it is until the page has room.
pub fn move_old_backpack(
    settings: &BagSettings,
    character: &mut CharacterData,
    kind_of: impl Fn(i32) -> i32,
) -> usize {
    let mut moved = 0;
    for index in 0..character.items.len() {
        let (id, template, bag) = (character.items[index].id, character.items[index].template, character.items[index].bag);
        let kind = kind_of(template);
        let misplaced = bag == settings.other_page && settings.page_for(kind) != settings.other_page;
        if bag != OLD_BACKPACK_BAG && !misplaced {
            continue;
        }
        if let Some((bag, cell)) = free_place(settings, character, kind, Some(id), &[]) {
            (character.items[index].bag, character.items[index].cell) = (bag, cell);
            moved += 1;
        }
    }
    moved
}

/// What the repair needs to know of a template: its kind, whether it stacks and
/// how many fit in one cell.
pub struct StackInfo {
    pub kind: i32,
    pub stacks: bool,
    pub max_count: i32,
}

/// Older saves could have two things in one cell, which the client draws as one.
/// The same thing goes onto the first stack as far as it fits; the rest moves to
/// a free cell of the same bag or else of its page, or stays put when all are full.
pub fn separate_shared_cells(settings: &BagSettings, character: &mut CharacterData, info: impl Fn(i32) -> StackInfo) -> usize {
    let mut changed = 0;
    let mut index = 0;
    while index < character.items.len() {
        let item = character.items[index].clone();
        let first = character.items[..index].iter().position(|other| (other.bag, other.cell) == (item.bag, item.cell));
        let Some(first) = first else {
            index += 1;
            continue;
        };
        changed += 1;
        let about = info(item.template);
        let target = &mut character.items[first];
        if about.stacks && target.template == item.template && target.param == item.param && target.parts == item.parts {
            let poured = item.count.min(about.max_count - target.count).max(0);
            target.count += poured;
            character.items[index].count -= poured;
            if character.items[index].count <= 0 {
                character.items.remove(index);
                continue;
            }
        }
        // Backpack pages and the warehouse keep the thing in the same bag; slots
        // of tackle and clothing do not fit anything else, so it goes to its page.
        let same_bag = panels::can_expand(settings, item.bag)
            .then(|| {
                let taken = |cell: i32| character.items.iter().any(|other| other.bag == item.bag && other.cell == cell);
                (0..panels::cells_in(settings, character, item.bag)).find(|cell| !taken(*cell)).map(|cell| (item.bag, cell))
            })
            .flatten();
        if let Some((bag, cell)) = same_bag.or_else(|| free_place(settings, character, about.kind, Some(item.id), &[])) {
            (character.items[index].bag, character.items[index].cell) = (bag, cell);
        }
        index += 1;
    }
    changed
}

/// Puts an item in a cell. Whatever lay there takes the item's old place, so
/// two things never share a cell. The moved items, the asked one first.
fn put_in_cell(character: &mut CharacterData, id: i64, bag: i32, cell: i32) -> Option<Vec<ItemEntry>> {
    let from = character.items.iter().position(|item| item.id == id)?;
    let old = (character.items[from].bag, character.items[from].cell);
    (character.items[from].bag, character.items[from].cell) = (bag, cell);
    let mut moved = vec![character.items[from].clone()];
    if old != (bag, cell) {
        for item in character.items.iter_mut().filter(|item| item.id != id && (item.bag, item.cell) == (bag, cell)) {
            (item.bag, item.cell) = old;
            moved.push(item.clone());
        }
    }
    Some(moved)
}

/// Client: long item, int bag, int cell. Bag -1 means any free cell.
pub async fn move_item(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let mut r = message.reader();
    let (id, mut bag, mut cell) = (r.int64()?, r.int32()?, r.int32()?);
    let player = active(&mut session.player)?;
    let conn = &session.conn;

    if bag == ANY_BAG {
        let kind = player
            .data
            .items
            .iter()
            .find(|item| item.id == id)
            .and_then(|item| conn.world.catalog.get(item.template))
            .map_or(0, |template| template.kind);
        let Some(free) = free_place(&conn.world.settings.bags, &player.data, kind, Some(id), &[]) else {
            conn.log(&format!("   move: no free cell for item {id}"));
            return Ok(Outcome::Unanswered);
        };
        (bag, cell) = free;
    }

    let Some(old_bag) = player.data.items.iter().find(|item| item.id == id).map(|item| item.bag) else {
        conn.log(&format!("   move: character has no item {id}"));
        return Ok(Outcome::Unanswered);
    };
    let moved = put_in_cell(&mut player.data, id, bag, cell).unwrap_or_default();
    let gear_changed = {
        let bags = &conn.world.settings.bags;
        bags.holds_gear(old_bag) || bags.holds_gear(bag)
    };

    conn.save(player).await?;
    send_updates(conn, player.data.id, &moved).await?;
    conn.log(&format!("   -> 181 item {id} now at {bag}/{cell}{}", if moved.len() > 1 { ", the one there moved to its place" } else { "" }));
    if gear_changed {
        character::send_update(conn, player).await?;
        perks::send(conn, player).await?;
    }
    Ok(Outcome::Answered)
}

/// What a stack looks like after some of it was used.
#[derive(Debug, PartialEq)]
enum Used {
    NotAStack,
    Left(ItemEntry),
    Gone,
}

fn take_from_stack(stacks: bool, character: &mut CharacterData, id: i64, amount: i32) -> Used {
    let Some(index) = character.items.iter().position(|item| item.id == id) else {
        return Used::NotAStack;
    };
    if !stacks {
        return Used::NotAStack;
    }
    character.items[index].count -= amount;
    if character.items[index].count > 0 {
        return Used::Left(character.items[index].clone());
    }
    character.items.remove(index);
    Used::Gone
}

/// Spends part of a stack such as bait. Lures and other single items stay.
pub async fn use_up(conn: &Conn, player: &mut Player, id: i64, amount: i32) -> Result<()> {
    let stacks = player
        .data
        .items
        .iter()
        .find(|item| item.id == id)
        .and_then(|item| conn.world.catalog.get(item.template))
        .is_some_and(|template| template.stacks);
    match take_from_stack(stacks, &mut player.data, id, amount) {
        Used::NotAStack => return Ok(()),
        Used::Left(item) => {
            conn.log(&format!("   -> 181 item {id}, {} left", item.count));
            send_updates(conn, player.data.id, &[item]).await?;
        }
        Used::Gone => {
            conn.log(&format!("   -> 190 item {id} used up"));
            send_removed(conn, id).await?;
        }
    }
    conn.save(player).await
}

/// Takes pieces from one item without telling the client; returns what to send.
pub fn take_pieces(character: &mut CharacterData, id: i64, amount: i32) -> Option<Result<ItemEntry, i64>> {
    match take_from_stack(true, character, id, amount) {
        Used::NotAStack => None,
        Used::Left(item) => Some(Ok(item)),
        Used::Gone => Some(Err(id)),
    }
}

/// Pieces of one item the character carries, storage and gear left out.
pub fn pieces_carried(character: &CharacterData, template: i32) -> i32 {
    character
        .items
        .iter()
        .filter(|item| item.template == template && item.bag < FIRST_NON_INVENTORY_BAG)
        .map(|item| item.count)
        .sum()
}

/// Takes pieces of one item out of the carried stacks, first stacks first.
/// Returns the stacks left smaller and the ids of those used up, or `None`
/// with nothing taken if there are too few.
pub fn take_carried(character: &mut CharacterData, template: i32, amount: i32) -> Option<(Vec<ItemEntry>, Vec<i64>)> {
    if pieces_carried(character, template) < amount {
        return None;
    }
    let stacks: Vec<(i64, i32)> = character
        .items
        .iter()
        .filter(|item| item.template == template && item.bag < FIRST_NON_INVENTORY_BAG)
        .map(|item| (item.id, item.count))
        .collect();
    let (mut changed, mut gone, mut left) = (Vec::new(), Vec::new(), amount);
    for (id, count) in stacks {
        if left == 0 {
            break;
        }
        let part = left.min(count);
        left -= part;
        match take_from_stack(true, character, id, part) {
            Used::Left(item) => changed.push(item),
            Used::Gone => gone.push(id),
            Used::NotAStack => {}
        }
    }
    Some((changed, gone))
}

/// Spends one piece of any item, single ones included.
pub async fn use_piece(conn: &Conn, player: &mut Player, id: i64) -> Result<()> {
    match take_from_stack(true, &mut player.data, id, 1) {
        Used::NotAStack => return Ok(()),
        Used::Left(item) => {
            conn.log(&format!("   -> 181 item {id}, {} left", item.count));
            send_updates(conn, player.data.id, &[item]).await?;
        }
        Used::Gone => {
            conn.log(&format!("   -> 190 item {id} used up"));
            send_removed(conn, id).await?;
        }
    }
    conn.save(player).await
}

/// Takes one piece away without telling the client, which has already done so.
pub fn remove_one(character: &mut CharacterData, id: i64) {
    if let Some(index) = character.items.iter().position(|item| item.id == id) {
        character.items[index].count -= 1;
        if character.items[index].count <= 0 {
            character.items.remove(index);
        }
    }
}

/// Takes the whole item away; `None` when the character has no such item.
fn throw_out(character: &mut CharacterData, id: i64) -> Option<ItemEntry> {
    let index = character.items.iter().position(|item| item.id == id)?;
    Some(character.items.remove(index))
}

/// Client 190 long item: the item dropped on the bin after the client asked
/// "are you sure". The client has already taken it out of its own list. Other
/// shapes of 190 (with the fridge or a place before the item) are not this.
pub async fn throw_away(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let mut r = message.reader();
    if r.remaining() != 8 {
        return Ok(Outcome::Unanswered);
    }
    let id = r.int64()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let Some(item) = destroy(conn, player, id).await? else {
        conn.log(&format!("   throw away: character has no item {id}"));
        return Ok(Outcome::Unanswered);
    };
    conn.log(&format!("   -> 190 item {id} (template {}, {} pieces) thrown away", item.template, item.count));
    Ok(Outcome::Answered)
}

/// What is left of a line after `torn` metres went: `None` when nothing is.
/// `left` is the piece's own `param`, 0 for a whole line.
fn shortened(full: f64, left: i32, torn: f32) -> Option<i32> {
    let now = if left > 0 { f64::from(left) } else { full };
    let after = (now - f64::from(torn)).round() as i32;
    (after > 0).then_some(after)
}

/// Takes a whole item away and tells the client; worn gear also resends
/// the character and skills.
async fn destroy(conn: &Conn, player: &mut Player, id: i64) -> Result<Option<ItemEntry>> {
    let Some(item) = throw_out(&mut player.data, id) else {
        return Ok(None);
    };
    conn.save(player).await?;
    send_removed(conn, id).await?;
    if conn.world.settings.bags.holds_gear(item.bag) {
        character::send_update(conn, player).await?;
        perks::send(conn, player).await?;
    }
    Ok(Some(item))
}

/// Client 1305 long item: a rod broke in the fight, or a line of 10 m or less
/// tore. The client has put it aside on its side (bag 500).
pub async fn broken(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let id = message.reader().int64()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    match destroy(conn, player, id).await? {
        Some(item) => conn.log(&format!("   -> 190 item {id} (template {}) broke in the fight", item.template)),
        None => conn.log(&format!("   broken: character has no item {id}")),
    }
    Ok(Outcome::Answered)
}

/// Client 1306 long line, float metres: the line tore and that much of it
/// went. The client leaves its own copy as it was, so the shorter line goes
/// back as 181 with the length left in its description.
pub async fn line_torn(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let mut r = message.reader();
    let (id, metres) = (r.int64()?, r.single()?);
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let Some(index) = player.data.items.iter().position(|item| item.id == id) else {
        conn.log(&format!("   line torn: character has no item {id}"));
        return Ok(Outcome::Answered);
    };
    let item = &player.data.items[index];
    let full = conn.world.catalog.get(item.template).and_then(|template| template_length(&template.xml)).unwrap_or(0.0);
    match shortened(full, item.param, metres) {
        Some(left) => {
            player.data.items[index].param = left;
            let changed = player.data.items[index].clone();
            conn.save(player).await?;
            send_updates(conn, player.data.id, &[changed]).await?;
            conn.log(&format!("   -> 181 line {id} lost {metres:.1} m, {left} m left"));
        }
        None => {
            destroy(conn, player, id).await?;
            conn.log(&format!("   -> 190 line {id} lost {metres:.1} m, nothing left"));
        }
    }
    Ok(Outcome::Answered)
}

/// Client 1307 long item, once per piece that went with a torn line: hook,
/// bait, float, lure or feeder. One piece of it goes.
pub async fn tackle_lost(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let id = message.reader().int64()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    if !player.data.items.iter().any(|item| item.id == id) {
        conn.log(&format!("   tackle lost: character has no item {id}"));
        return Ok(Outcome::Answered);
    }
    conn.log(&format!("   item {id} went with the line"));
    use_piece(conn, player, id).await?;
    Ok(Outcome::Answered)
}

/// Case 190: item ids until the end of the message.
pub async fn send_removed(conn: &Conn, id: i64) -> Result<()> {
    let mut w = AfcWriter::new();
    w.int64(id);
    conn.send(msg::ITEMS_REMOVED, w.finish()).await
}

/// Which items changed when part of a stack moved, and any stack that ended.
#[derive(Debug, PartialEq)]
struct Split {
    changed: Vec<ItemEntry>,
    removed: Option<i64>,
}

/// `None` when the count is wrong or something else occupies the cell.
fn split(character: &mut CharacterData, id: i64, count: i32, place: (i32, i32), new_id: i64) -> Option<Split> {
    let source = character.items.iter().position(|item| item.id == id)?;
    if count < 1 || count > character.items[source].count {
        return None;
    }
    let (bag, cell) = place;
    let template = character.items[source].template;
    let target = character.items.iter().position(|item| item.bag == bag && item.cell == cell && item.id != id);
    if target.is_some_and(|target| character.items[target].template != template) {
        return None;
    }

    let whole = count == character.items[source].count;
    let Some(target) = target else {
        if whole {
            (character.items[source].bag, character.items[source].cell) = place;
            return Some(Split { changed: vec![character.items[source].clone()], removed: None });
        }
        character.items[source].count -= count;
        let piece = ItemEntry { id: new_id, bag, cell, count, ..character.items[source].clone() };
        character.items.push(piece.clone());
        return Some(Split { changed: vec![character.items[source].clone(), piece], removed: None });
    };

    character.items[target].count += count;
    let merged = character.items[target].clone();
    if whole {
        character.items.remove(source);
        return Some(Split { changed: vec![merged], removed: Some(id) });
    }
    character.items[source].count -= count;
    Some(Split { changed: vec![character.items[source].clone(), merged], removed: None })
}

/// Client: long item, int count, int bag, int cell. It lowers the count on its
/// side first, so a refused move sends the item back unchanged.
pub async fn split_item(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let mut r = message.reader();
    let (id, count, bag, cell) = (r.int64()?, r.int32()?, r.int32()?, r.int32()?);
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let new_id = conn.world.store.next_id().await?;

    let Some(done) = split(&mut player.data, id, count, (bag, cell), new_id) else {
        conn.log(&format!("   split of item {id} to {bag}/{cell} refused"));
        let unchanged: Vec<_> = player.data.items.iter().filter(|item| item.id == id).cloned().collect();
        send_updates(conn, player.data.id, &unchanged).await?;
        return Ok(Outcome::Answered);
    };

    conn.save(player).await?;
    send_updates(conn, player.data.id, &done.changed).await?;
    if let Some(removed) = done.removed {
        send_removed(conn, removed).await?;
    }
    conn.log(&format!("   -> 181 moved {count} of item {id} to {bag}/{cell}"));
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

/// Pours `source` onto `target`, as far as one cell holds. The two must be the
/// same thing. `None` when they are not, or either is missing.
fn merge(character: &mut CharacterData, target: i64, source: i64, capacity: i32) -> Option<Split> {
    let to = character.items.iter().position(|item| item.id == target)?;
    let from = character.items.iter().position(|item| item.id == source)?;
    let (into, poured) = (&character.items[to], &character.items[from]);
    if to == from || into.template != poured.template || into.param != poured.param || into.parts != poured.parts {
        return None;
    }
    let moved = character.items[from].count.min(capacity - character.items[to].count).max(0);
    character.items[to].count += moved;
    character.items[from].count -= moved;
    let mut changed = vec![character.items[to].clone()];
    let removed = if character.items[from].count <= 0 {
        character.items.remove(from);
        Some(source)
    } else {
        changed.push(character.items[from].clone());
        None
    };
    Some(Split { changed, removed })
}

/// Client: long item dropped on, long item dragged. The client has already
/// put the stacks together on its side and waits for no reply.
pub async fn merge_items(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let mut r = message.reader();
    let (target, source) = (r.int64()?, r.int64()?);
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let capacity = player
        .data
        .items
        .iter()
        .find(|item| item.id == target)
        .and_then(|item| conn.world.catalog.get(item.template))
        .filter(|template| template.stacks)
        .map(|template| template.max_count.max(1));

    let Some(done) = capacity.and_then(|capacity| merge(&mut player.data, target, source, capacity)) else {
        conn.log(&format!("   merge of {source} onto {target} refused"));
        let unchanged: Vec<_> =
            player.data.items.iter().filter(|item| item.id == target || item.id == source).cloned().collect();
        send_updates(conn, player.data.id, &unchanged).await?;
        return Ok(Outcome::Answered);
    };
    conn.save(player).await?;
    send_updates(conn, player.data.id, &done.changed).await?;
    if let Some(removed) = done.removed {
        send_removed(conn, removed).await?;
    }
    conn.log(&format!("   -> 181 merged item {source} onto {target}"));
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
pub struct Purchase {
    /// Stacks to top up: position in the character's items and how many pieces.
    topped: Vec<(usize, i32)>,
    /// New cells and how many pieces go in each.
    new_stacks: Vec<((i32, i32), i32)>,
    /// The `param` of the new pieces.
    param: i32,
    parts: [i32; 4],
}

pub fn plan_purchase(
    settings: &BagSettings,
    character: &CharacterData,
    item: &CatalogItem,
    count: i32,
) -> Result<Purchase, BuyError> {
    if item.cost.saturating_mul(i64::from(count)) > character.money
        || item.premium_cost.saturating_mul(i64::from(count)) > character.premium
    {
        return Err(BuyError::NoMoney);
    }
    plan_pieces(settings, character, item, count.saturating_mul(item.shop_count.max(1)))
}

/// Where this many pieces of an item would go: onto stacks first, then free cells.
pub fn plan_pieces(
    settings: &BagSettings,
    character: &CharacterData,
    item: &CatalogItem,
    pieces: i32,
) -> Result<Purchase, BuyError> {
    plan_pieces_of(settings, character, item, pieces, 0)
}

/// As [`plan_pieces`], for pieces that carry their own `param`: they only go
/// onto stacks with the same one.
pub fn plan_pieces_of(
    settings: &BagSettings,
    character: &CharacterData,
    item: &CatalogItem,
    pieces: i32,
    param: i32,
) -> Result<Purchase, BuyError> {
    let capacity = if item.stacks { item.max_count.max(1) } else { 1 };
    let mut left = pieces;
    let mut topped = Vec::new();
    if item.stacks {
        for (index, held) in character.items.iter().enumerate() {
            if held.template == item.id
                && held.param == param
                && held.parts == [0; 4]
                && held.bag < FIRST_NON_INVENTORY_BAG
                && held.count < capacity
                && left > 0
            {
                let added = left.min(capacity - held.count);
                topped.push((index, added));
                left -= added;
            }
        }
    }

    let mut places = Vec::new();
    let mut new_stacks = Vec::new();
    while left > 0 {
        let place = free_place(settings, character, item.kind, None, &places).ok_or(BuyError::NoRoom)?;
        let pieces = left.min(capacity);
        places.push(place);
        new_stacks.push((place, pieces));
        left -= pieces;
    }
    Ok(Purchase { topped, new_stacks, param, parts: [0; 4] })
}

/// Room for one built thing: it never joins a stack, its parts are its own.
pub fn plan_built(settings: &BagSettings, character: &CharacterData, item: &CatalogItem, parts: [i32; 4]) -> Result<Purchase, BuyError> {
    if parts == [0; 4] {
        return plan_pieces_of(settings, character, item, 1, 0);
    }
    let place = free_place(settings, character, item.kind, None, &[]).ok_or(BuyError::NoRoom)?;
    Ok(Purchase { topped: Vec::new(), new_stacks: vec![(place, 1)], param: 0, parts })
}

pub async fn give_built(conn: &Conn, player: &mut Player, item: &CatalogItem, parts: [i32; 4]) -> Result<Result<Vec<ItemEntry>, BuyError>> {
    match plan_built(&conn.world.settings.bags, &player.data, item, parts) {
        Ok(plan) => Ok(Ok(place(conn, player, item, plan).await?)),
        Err(error) => Ok(Err(error)),
    }
}

/// Pays for and hands over the items, returning those that changed.
pub async fn buy(
    conn: &Conn,
    player: &mut Player,
    item: &CatalogItem,
    count: i32,
) -> Result<Result<Vec<ItemEntry>, BuyError>> {
    let purchase = match plan_purchase(&conn.world.settings.bags, &player.data, item, count) {
        Ok(purchase) => purchase,
        Err(error) => return Ok(Err(error)),
    };
    let changed = place(conn, player, item, purchase).await?;
    player.data.money -= item.cost * i64::from(count);
    character::spend_premium(&mut player.data, item.premium_cost * i64::from(count));
    conn.save(player).await?;
    Ok(Ok(changed))
}

/// Gives pieces of an item for free, as loot or a crafted result. The caller
/// sends the changed items and saves.
pub async fn give(
    conn: &Conn,
    player: &mut Player,
    item: &CatalogItem,
    pieces: i32,
) -> Result<Result<Vec<ItemEntry>, BuyError>> {
    give_of(conn, player, item, pieces, 0).await
}

/// As [`give`], for pieces with their own `param`, such as a fish card's species.
pub async fn give_of(
    conn: &Conn,
    player: &mut Player,
    item: &CatalogItem,
    pieces: i32,
    param: i32,
) -> Result<Result<Vec<ItemEntry>, BuyError>> {
    match plan_pieces_of(&conn.world.settings.bags, &player.data, item, pieces, param) {
        Ok(plan) => Ok(Ok(place(conn, player, item, plan).await?)),
        Err(error) => Ok(Err(error)),
    }
}

async fn place(conn: &Conn, player: &mut Player, item: &CatalogItem, purchase: Purchase) -> Result<Vec<ItemEntry>> {
    let mut changed = Vec::with_capacity(purchase.topped.len() + purchase.new_stacks.len());
    for (index, added) in purchase.topped {
        player.data.items[index].count += added;
        changed.push(player.data.items[index].clone());
    }
    for ((bag, cell), pieces) in purchase.new_stacks {
        let entry = ItemEntry {
            id: conn.world.store.next_id().await?,
            bag,
            cell,
            count: pieces,
            durability: item.durability,
            template: item.id,
            param: purchase.param,
            parts: purchase.parts,
        };
        player.data.items.push(entry.clone());
        changed.push(entry);
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog_item(stacks: bool, cost: i64) -> CatalogItem {
        CatalogItem { id: 10, kind: 10, cost, durability: 1.0, stacks, shop_count: 1, max_count: 99, premium_cost: 0, xml: String::new(), refine: String::new(), name: String::new(), cost_item: [0, 0], ..Default::default() }
    }

    fn held(id: i64, cell: i32, template: i32) -> ItemEntry {
        ItemEntry { id, bag: 1, cell, count: 1, durability: 1.0, template, param: 0, parts: [0; 4] }
    }

    #[test]
    fn a_thrown_item_goes_whole() {
        let mut character = CharacterData { items: vec![held(1, 0, 10), ItemEntry { count: 5, ..held(2, 1, 11) }], ..Default::default() };
        assert_eq!(throw_out(&mut character, 2).map(|item| item.count), Some(5));
        assert_eq!(character.items, vec![held(1, 0, 10)]);
        assert_eq!(throw_out(&mut character, 2), None);
    }

    /// Two pages of two cells; kind 10 belongs on the first.
    fn bags() -> BagSettings {
        BagSettings {
            pages: 2,
            start_cells: 2,
            storage_cells: 45,
            cells_per_purchase: 5,
            token_item: 1179,
            purchase_tokens: 10,
            storage_token_item: 1336,
            storage_purchase_tokens: 5,
            purchase_premium: 10,
            page_kinds: [(1, vec![10])].into(),
            other_page: 2,
            worn_bags: vec![60],
            tackle_bags: vec![],
        }
    }

    fn stack(id: i64, bag: i32, cell: i32, count: i32, template: i32) -> ItemEntry {
        ItemEntry { id, bag, cell, count, durability: 1.0, template, param: 0, parts: [0; 4] }
    }

    #[test]
    fn merging_fills_the_target_up_to_a_cell() {
        let mut character =
            CharacterData { items: vec![stack(1, 0, 0, 15, 1010), stack(2, 0, 1, 10, 1010)], ..Default::default() };
        let done = merge(&mut character, 1, 2, 20).unwrap();
        assert_eq!((character.items[0].count, character.items[1].count), (20, 5));
        assert_eq!((done.changed.len(), done.removed), (2, None));

        let done = merge(&mut character, 1, 2, 30).unwrap();
        assert_eq!(character.items.len(), 1);
        assert_eq!((character.items[0].count, done.removed), (25, Some(2)));
    }

    #[test]
    fn different_things_do_not_merge() {
        let mut character =
            CharacterData { items: vec![stack(1, 0, 0, 1, 1010), stack(2, 0, 1, 1, 1088)], ..Default::default() };
        assert!(merge(&mut character, 1, 2, 20).is_none());
        assert!(merge(&mut character, 1, 1, 20).is_none());
    }

    #[test]
    fn bait_runs_out_one_catch_at_a_time() {
        let mut character = CharacterData { items: vec![stack(1, 100, 20, 2, 1018)], ..Default::default() };
        assert!(matches!(take_from_stack(true, &mut character, 1, 1), Used::Left(item) if item.count == 1));
        assert_eq!(take_from_stack(true, &mut character, 1, 1), Used::Gone);
        assert!(character.items.is_empty());
    }

    #[test]
    fn a_lure_is_not_used_up() {
        let mut character = CharacterData { items: vec![stack(1, 100, 20, 1, 1018)], ..Default::default() };
        assert_eq!(take_from_stack(false, &mut character, 1, 1), Used::NotAStack);
        assert_eq!(character.items[0].count, 1);
    }

    #[test]
    fn part_of_a_stack_goes_to_an_empty_cell() {
        let mut character = CharacterData { items: vec![stack(1, 0, 0, 5, 7)], ..Default::default() };
        let done = split(&mut character, 1, 2, (52, 3), 9).unwrap();
        assert_eq!(done.changed, vec![stack(1, 0, 0, 3, 7), stack(9, 52, 3, 2, 7)]);
        assert_eq!(character.items.len(), 2);
    }

    #[test]
    fn a_whole_stack_merges_into_the_same_item() {
        let mut character = CharacterData { items: vec![stack(1, 0, 0, 5, 7), stack(2, 52, 3, 1, 7)], ..Default::default() };
        let done = split(&mut character, 1, 5, (52, 3), 9).unwrap();
        assert_eq!(done.removed, Some(1));
        assert_eq!(character.items, vec![stack(2, 52, 3, 6, 7)]);
    }

    #[test]
    fn a_split_cannot_land_on_another_item() {
        let mut character = CharacterData { items: vec![stack(1, 0, 0, 5, 7), stack(2, 52, 3, 1, 8)], ..Default::default() };
        assert_eq!(split(&mut character, 1, 2, (52, 3), 9), None);
        assert_eq!(split(&mut character, 1, 6, (52, 4), 9), None);
    }

    #[test]
    fn stacks_join_an_existing_stack() {
        let character = CharacterData { money: 100, items: vec![held(1, 0, 10)], ..Default::default() };
        let plan = plan_purchase(&bags(), &character, &catalog_item(true, 1), 5);
        assert_eq!(plan, Ok(Purchase { topped: vec![(0, 5)], new_stacks: vec![], param: 0, parts: [0; 4] }));
    }

    #[test]
    fn single_items_take_free_cells_in_order() {
        let character = CharacterData { money: 100, items: vec![held(1, 0, 99)], ..Default::default() };
        let plan = plan_purchase(&bags(), &character, &catalog_item(false, 1), 1);
        assert_eq!(plan, Ok(Purchase { topped: vec![], new_stacks: vec![((1, 1), 1)], param: 0, parts: [0; 4] }));
    }

    #[test]
    fn one_purchase_brings_a_whole_pack() {
        let item = CatalogItem { shop_count: 50, ..catalog_item(true, 1) };
        let character = CharacterData { money: 100, ..Default::default() };
        let plan = plan_purchase(&bags(), &character, &item, 2);
        assert_eq!(plan, Ok(Purchase { topped: vec![], new_stacks: vec![((1, 0), 99), ((1, 1), 1)], param: 0, parts: [0; 4] }));
    }

    #[test]
    fn a_full_stack_overflows_into_a_new_cell() {
        let item = CatalogItem { shop_count: 30, ..catalog_item(true, 1) };
        let mut full = held(1, 0, 10);
        full.count = 90;
        let character = CharacterData { money: 100, items: vec![full], ..Default::default() };
        let plan = plan_purchase(&bags(), &character, &item, 1);
        assert_eq!(plan, Ok(Purchase { topped: vec![(0, 9)], new_stacks: vec![((1, 1), 21)], param: 0, parts: [0; 4] }));
    }

    #[test]
    fn using_the_last_piece_removes_the_item() {
        let mut character = CharacterData { items: vec![held(1, 0, 10)], ..Default::default() };
        remove_one(&mut character, 1);
        assert!(character.items.is_empty());
    }

    #[test]
    fn ergs_pay_for_an_erg_price() {
        let item = CatalogItem { cost: 0, premium_cost: 20, ..catalog_item(false, 0) };
        let poor = CharacterData { money: 1_000_000, premium: 19, ..Default::default() };
        assert_eq!(plan_purchase(&bags(), &poor, &item, 1), Err(BuyError::NoMoney));
        let rich = CharacterData { premium: 20, ..Default::default() };
        assert!(plan_purchase(&bags(), &rich, &item, 1).is_ok());
    }

    #[test]
    fn cannot_buy_past_the_balance() {
        let character = CharacterData { money: 4, ..Default::default() };
        assert_eq!(plan_purchase(&bags(), &character, &catalog_item(true, 1), 5), Err(BuyError::NoMoney));
    }

    #[test]
    fn a_full_page_refuses_even_with_room_elsewhere() {
        let items = vec![stack(1, 1, 0, 1, 99), stack(2, 1, 1, 1, 99)];
        let character = CharacterData { money: 100, items, ..Default::default() };
        assert_eq!(plan_purchase(&bags(), &character, &catalog_item(false, 1), 1), Err(BuyError::NoRoom));
    }

    #[test]
    fn an_unlisted_kind_goes_to_the_other_page() {
        assert_eq!(free_place(&bags(), &CharacterData::default(), 77, None, &[]), Some((2, 0)));
    }

    #[test]
    fn the_old_backpack_moves_onto_pages() {
        let mut character = CharacterData {
            items: vec![stack(1, 0, 0, 1, 500), stack(2, 0, 1, 1, 600), stack(3, 100, 1, 1, 500)],
            ..Default::default()
        };
        let moved = move_old_backpack(&bags(), &mut character, |template| if template == 500 { 10 } else { 77 });
        assert_eq!(moved, 2);
        assert_eq!((character.items[0].bag, character.items[0].cell), (1, 0));
        assert_eq!((character.items[1].bag, character.items[1].cell), (2, 0));
        assert_eq!(character.items[2].bag, 100);
    }

    #[test]
    fn items_left_on_the_other_page_move_to_their_own() {
        let mut character = CharacterData { items: vec![stack(1, 2, 0, 1, 500), stack(2, 2, 1, 1, 600)], ..Default::default() };
        let moved = move_old_backpack(&bags(), &mut character, |template| if template == 500 { 10 } else { 77 });
        assert_eq!(moved, 1);
        assert_eq!((character.items[0].bag, character.items[0].cell), (1, 0));
        assert_eq!((character.items[1].bag, character.items[1].cell), (2, 1));
    }

    #[test]
    fn old_items_wait_for_room_on_their_own_page() {
        let mut character = CharacterData {
            items: vec![stack(1, 1, 0, 1, 99), stack(2, 1, 1, 1, 99), stack(3, 0, 0, 1, 500)],
            ..Default::default()
        };
        assert_eq!(move_old_backpack(&bags(), &mut character, |_| 10), 0);
        assert_eq!(character.items[2].bag, OLD_BACKPACK_BAG);
    }

    #[test]
    fn swap_exchanges_places() {
        let mut character = CharacterData { items: vec![held(1, 0, 9), held(2, 5, 9)], ..Default::default() };
        swap(&mut character, 1, 2).unwrap();
        assert_eq!((character.items[0].cell, character.items[1].cell), (5, 0));
    }

    #[test]
    fn moving_onto_an_occupied_cell_trades_places() {
        let mut character = CharacterData { items: vec![held(1, 0, 1001), held(2, 1, 1287)], ..Default::default() };
        let moved = put_in_cell(&mut character, 2, 1, 0).unwrap();
        assert_eq!(moved.iter().map(|item| (item.id, item.cell)).collect::<Vec<_>>(), vec![(2, 0), (1, 1)]);
        assert_eq!(put_in_cell(&mut character, 1, 1, 5).unwrap().len(), 1);
    }

    #[test]
    fn things_sharing_a_cell_are_stacked_or_moved_apart() {
        let info = |template| StackInfo { kind: if template == 1089 { 5 } else { 10 }, stacks: template == 1089, max_count: 30 };
        let mut character = CharacterData {
            items: vec![held(1, 0, 1018), held(2, 0, 1089), held(3, 1, 1089), held(4, 1, 1089)],
            ..Default::default()
        };
        character.items[2].count = 3;
        character.items[3].count = 3;
        assert_eq!(separate_shared_cells(&bags(), &mut character, info), 2);
        let places: Vec<_> = character.items.iter().map(|item| (item.id, item.bag, item.cell, item.count)).collect();
        assert_eq!(places, vec![(1, 1, 0, 1), (2, 2, 0, 1), (3, 1, 1, 6)]);
    }

    #[test]
    fn a_thing_sharing_a_warehouse_cell_stays_in_the_warehouse() {
        let info = |_| StackInfo { kind: 10, stacks: false, max_count: 1 };
        let mut character = CharacterData { items: vec![held(1, 0, 1018), held(2, 0, 1252)], ..Default::default() };
        for item in &mut character.items {
            item.bag = 52;
        }
        assert_eq!(separate_shared_cells(&bags(), &mut character, info), 1);
        assert_eq!((character.items[1].bag, character.items[1].cell), (52, 1));
    }

    #[test]
    fn pieces_are_taken_across_carried_stacks_but_not_from_storage() {
        let stack = |id, bag, count| ItemEntry { id, bag, cell: id as i32, count, durability: 1.0, template: 1167, param: 0, parts: [0; 4] };
        let mut character = CharacterData { items: vec![stack(1, 1, 3), stack(2, 52, 10), stack(3, 2, 4)], ..Default::default() };
        assert_eq!(pieces_carried(&character, 1167), 7);
        assert_eq!(take_carried(&mut character, 1167, 8), None);
        let (smaller, gone) = take_carried(&mut character, 1167, 5).unwrap();
        assert_eq!(gone, vec![1]);
        assert_eq!(smaller.iter().map(|item| (item.id, item.count)).collect::<Vec<_>>(), vec![(3, 2)]);
        assert_eq!(pieces_carried(&character, 1167), 2);
    }

    #[test]
    fn a_card_shows_its_species_in_the_description() {
        let xml = "<base><tid>1309</tid><basePar>0/0/0</basePar></base>";
        assert_eq!(with_param(xml, 77, 0), "<base><tid>1309</tid><basePar>77/0/0</basePar></base>");
        assert_eq!(with_param(xml, 0, 0), xml);
        assert_eq!(with_param("<base></base>", 5, 0), "<base></base>");
    }

    #[test]
    fn a_line_shows_what_is_left_of_it() {
        let xml = "<base><basePar>40/0.26/100</basePar></base>";
        assert_eq!(with_param(xml, 96, param_slot(LINE_KIND)), "<base><basePar>40/0.26/96</basePar></base>");
        assert_eq!(template_length(xml), Some(100.0));
        assert_eq!(with_param("<basePar>40</basePar>", 96, 2), "<basePar>40</basePar>");
        assert_eq!(shortened(100.0, 0, 3.4), Some(97));
        assert_eq!(shortened(100.0, 97, 3.6), Some(93));
        assert_eq!(shortened(100.0, 3, 4.0), None);
    }

    #[test]
    fn cards_of_different_species_keep_apart() {
        let mut card = catalog_item(true, 0);
        card.id = 1309;
        let mut held = stack(1, 1, 0, 3, 1309);
        held.param = 78;
        let character = CharacterData { items: vec![held], ..Default::default() };
        let other = plan_pieces_of(&bags(), &character, &card, 1, 77).unwrap();
        assert!(other.topped.is_empty());
        let same = plan_pieces_of(&bags(), &character, &card, 1, 78).unwrap();
        assert_eq!(same.topped, vec![(0, 1)]);
    }
}
