//! Windows that open from the panel buttons and from objects in the base.
//!
//! The client draws most of the character card from what it already knows. Lists
//! that lived on the shut-down server (auction lots, artels, titles) go
//! out empty, so the windows open instead of leaving the client waiting.

use anyhow::Result;

use super::{Outcome, Session, active, character};
use crate::afc::{AfcMessage, AfcWriter};
use crate::config::BagSettings;
use crate::messages as msg;
use crate::storage::CharacterData;

/// Bag id of the warehouse window.
const STORAGE_BAG: i32 = 52;
const PAY_MONEY: i32 = 1;
const PAY_PREMIUM: i32 = 2;
/// The client asks for its own card with this text.
const OWN_CARD: &str = "?";

/// Case 32 reads nothing: the card is drawn from the character the client holds.
pub async fn card(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    if message.reader().string().ok().as_deref() != Some(OWN_CARD) {
        return Ok(Outcome::Unanswered);
    }
    session.conn.send(msg::CHARACTER_CARD, Vec::new()).await?;
    session.conn.log("   -> 32 character card");
    Ok(Outcome::Answered)
}

/// Tabs of the card whose lists are read until the message ends. 40 and 130 double
/// as other requests with other payloads; the card's tabs send a bare int 0.
pub async fn empty_tab(session: &mut Session, message: &AfcMessage, id: i32) -> Result<Outcome> {
    if matches!(id, msg::RECORDS | msg::ACHIEVEMENTS) && !matches!(message.reader().int32(), Ok(0)) {
        return Ok(Outcome::Unanswered);
    }
    session.conn.send(id, Vec::new()).await?;
    session.conn.log(&format!("   -> {id} empty card tab"));
    Ok(Outcome::Answered)
}

/// Case 1401: int free points, int spent points, then the perks.
/// Case 330: pairs of template and lot count. Nobody else is selling here.
pub async fn auction(session: &mut Session) -> Result<Outcome> {
    session.conn.send(msg::AUCTION, Vec::new()).await?;
    session.conn.log("   -> 330 auction, no lots");
    Ok(Outcome::Answered)
}

/// Case 450: two unread ints, then the artels.
pub async fn artels(session: &mut Session) -> Result<Outcome> {
    let mut w = AfcWriter::new();
    w.int32(0).int32(0);
    session.conn.send(msg::ARTELS, w.finish()).await?;
    session.conn.log("   -> 450 no artels");
    Ok(Outcome::Answered)
}

/// Client 1700: int 0, int station; 1701: int station. Reply: int station, then
/// the recipes. An empty list makes the window say there is nothing to make.
/// Client: the warehouse number. Case 1230 takes its size in cells.
pub async fn storage(session: &mut Session) -> Result<Outcome> {
    let player = active(&mut session.player)?;
    let cells = cells_in(&session.conn.world.settings.bags, &player.data, STORAGE_BAG);
    let mut w = AfcWriter::new();
    w.int32(cells);
    session.conn.send(msg::STORAGE, w.finish()).await?;
    session.conn.log(&format!("   -> 1230 storage of {cells} cells"));
    Ok(Outcome::Answered)
}

pub fn cells_in(settings: &BagSettings, character: &CharacterData, bag: i32) -> i32 {
    let start = if bag == STORAGE_BAG { settings.storage_cells } else { settings.start_cells };
    start + character.bag_extra.get(&bag).copied().unwrap_or(0)
}

pub fn can_expand(settings: &BagSettings, bag: i32) -> bool {
    bag == STORAGE_BAG || (1..=settings.pages).contains(&bag)
}

/// Each purchase costs one step more than the one before it.
fn price(settings: &BagSettings, character: &CharacterData, bag: i32) -> (i64, i64) {
    let bought = character.bag_extra.get(&bag).copied().unwrap_or(0) / settings.cells_per_purchase;
    let step = i64::from(bought) + 1;
    (settings.purchase_money * step, settings.purchase_premium * step)
}

/// Client: int bag. Case 210: int bag, then the two ways to pay, each a string
/// "money|premium|item|count".
pub async fn bag_price(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let bag = message.reader().int32()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let settings = &conn.world.settings.bags;
    if !can_expand(settings, bag) {
        conn.log(&format!("   bag {bag} cannot be expanded"));
        return Ok(Outcome::Unanswered);
    }

    let (money, premium) = price(settings, &player.data, bag);
    let mut w = AfcWriter::new();
    w.int32(bag).string(&format!("{money}|0")).string(&format!("0|{premium}"));
    conn.send(msg::BAG_PRICE, w.finish()).await?;
    conn.log(&format!("   -> 210 bag {bag} costs {money} or {premium} premium"));
    Ok(Outcome::Answered)
}

/// Takes the price and adds the cells; `false` if the character cannot pay.
fn expand(settings: &BagSettings, character: &mut CharacterData, bag: i32, method: i32) -> bool {
    let (money, premium) = price(settings, character, bag);
    match method {
        PAY_MONEY if character.money >= money => character.money -= money,
        PAY_PREMIUM if character.premium >= premium => character.premium -= premium,
        _ => return false,
    }
    *character.bag_extra.entry(bag).or_insert(0) += settings.cells_per_purchase;
    true
}

/// Client: int bag, int way to pay. Case 215 only plays a sound; the new size
/// reaches the client in case 102. The warehouse learns its size when reopened.
pub async fn buy_cells(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let mut r = message.reader();
    let (bag, method) = (r.int32()?, r.int32()?);
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let settings = &conn.world.settings.bags;

    let before = player.data.money;
    if !can_expand(settings, bag) || !expand(settings, &mut player.data, bag, method) {
        conn.log(&format!("   bag {bag}: payment {method} refused"));
        return Ok(Outcome::Unanswered);
    }

    conn.save(player).await?;
    character::send_money(conn, player, player.data.money - before).await?;
    character::send_update(conn, player).await?;
    conn.send(msg::BAG_BOUGHT, Vec::new()).await?;
    conn.log(&format!("   -> 21, 102, 215 bag {bag} now {} cells", cells_in(settings, &player.data, bag)));
    Ok(Outcome::Answered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> BagSettings {
        BagSettings {
            pages: 10,
            start_cells: 10,
            storage_cells: 45,
            cells_per_purchase: 5,
            purchase_money: 500,
            purchase_premium: 10,
            page_kinds: Default::default(),
            other_page: 10,
            worn_bags: vec![60],
            tackle_bags: vec![],
        }
    }

    #[test]
    fn each_purchase_adds_cells_and_costs_more() {
        let mut character = CharacterData { money: 1500, ..Default::default() };
        assert!(expand(&settings(), &mut character, 1, PAY_MONEY));
        assert_eq!(character.money, 1000);
        assert_eq!(cells_in(&settings(), &character, 1), 15);
        assert_eq!(price(&settings(), &character, 1), (1000, 20));
        assert_eq!(price(&settings(), &character, 2), (500, 10));
    }

    #[test]
    fn a_purchase_needs_the_chosen_currency() {
        let mut character = CharacterData { money: 5000, premium: 9, ..Default::default() };
        assert!(!expand(&settings(), &mut character, 1, PAY_PREMIUM));
        assert!(!expand(&settings(), &mut character, 1, 3));
        assert_eq!(cells_in(&settings(), &character, 1), 10);
        assert_eq!(character.money, 5000);
    }

    #[test]
    fn the_warehouse_has_its_own_size() {
        let character = CharacterData::default();
        assert_eq!(cells_in(&settings(), &character, STORAGE_BAG), 45);
        assert!(can_expand(&settings(), STORAGE_BAG));
        assert!(!can_expand(&settings(), 11));
        assert!(!can_expand(&settings(), 0));
    }
}
