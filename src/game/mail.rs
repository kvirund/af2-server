//! Mail: «Служба доставки» on a base's post box.
//!
//! A click on the box sends 1600 (int 0); case 1600 back is int 0, then one
//! row per letter until the end: long id, string from, string subject, date,
//! bool carries something, bool seen. Case 1601 (empty) then opens the window
//! on that list. A click on a letter marks it seen in the client and sends
//! 1610 (long letter); case 1610 back opens it: long id, long sender (the
//! client offers «Ответить» when it is above 10), string from, string
//! subject, date, string text, string what it carries, `;` between
//! `money=N`, `template=id/pieces` and `weapon=item` (an item the client
//! must already know, so it comes first in a 181 with no owner of this
//! client's, which leaves it out of the bags).
//!
//! «Забрать вложения» sends 1620 (long letter), «Удалить» 1640 (long letter;
//! the client refuses a letter that still carries something), «Удалить все»
//! 1650 (long). «Написать» opens the form, its «Найти» sends 45 (string
//! name) and case 45 back lists who matches: long id, string name, int base,
//! int location, until the end. «Отправить» sends 1660: string to, string
//! subject, string text, long money, int n, n × long item; case 1660 back is
//! one int: 0 sent, 1 no such recipient, 2 not enough money, 3 failed, 4 the
//! items are not there, 5 to oneself. Case 275 (int) shows «Вам пришло
//! письмо».

use anyhow::Result;
use chrono::{DateTime, Local, Utc};

use super::{Conn, Outcome, Player, Session, active, character, chat, inventory, vip};
use crate::afc::{AfcMessage, AfcWriter};
use crate::config::MailSettings;
use crate::messages as msg;
use crate::storage::{CharacterData, ItemEntry, Letter};

/// The client offers «Ответить» only for a sender id above this.
const REPLY_FROM: i64 = 11;
/// Answers of case 1660.
const SENT: i32 = 0;
const NO_RECIPIENT: i32 = 1;
const NO_MONEY: i32 = 2;
const FAILED: i32 = 3;
const NO_ITEMS: i32 = 4;
const TO_ONESELF: i32 = 5;
/// The deleted things' bag: nothing in it can be sent.
const DELETED_BAG: i32 = 50;

fn when(at: i64) -> chrono::NaiveDateTime {
    DateTime::from_timestamp(at, 0).map_or_else(|| Local::now().naive_local(), |at| at.with_timezone(&Local).naive_local())
}

/// What a letter costs to send, in hundredths: the client's own sum (shown
/// in the form), less with VIP.
pub fn cost(settings: &MailSettings, vip_percent: f64, vip_on: bool, money: i64, items: usize) -> i64 {
    let full = settings.base_cost + (money as f64 * settings.money_percent / 100.0) as i64 + settings.item_cost * items as i64;
    if vip_on { (full as f64 * (1.0 - vip_percent / 100.0)).round() as i64 } else { full }
}

/// The attachment line of case 1610.
fn attachment_line(letter: &Letter) -> String {
    let mut parts = Vec::new();
    if letter.money > 0 {
        parts.push(format!("money={}", letter.money));
    }
    parts.extend(letter.items.iter().map(|item| format!("weapon={}", item.id)));
    parts.join(";")
}

/// Whether an item may go by mail: the character's own, not thrown away, not
/// bound to its owner.
fn sendable(world: &crate::world::World, character: &CharacterData, id: i64) -> Option<ItemEntry> {
    let item = character.items.iter().find(|item| item.id == id && item.bag != DELETED_BAG)?;
    let template = world.catalog.get(item.template)?;
    (!template.xml.contains("<isPersonal>1</isPersonal>")).then(|| item.clone())
}

async fn send_list(conn: &Conn, player: &Player) -> Result<()> {
    let letters = conn.world.store.letters_for(player.data.id).await?;
    let mut w = AfcWriter::new();
    w.int32(0);
    for letter in &letters {
        w.int64(letter.id).string(&letter.from_name).string(&letter.subject).date_time(when(letter.sent_at));
        w.boolean(letter.carries()).boolean(letter.seen);
    }
    conn.send(msg::MAIL, w.finish()).await?;
    conn.send(msg::MAIL_WINDOW, Vec::new()).await?;
    conn.log(&format!("   -> 1600, 1601 mailbox, {} letter(s)", letters.len()));
    Ok(())
}

async fn letter_of(conn: &Conn, player: &Player, id: i64) -> Result<Option<Letter>> {
    Ok(conn.world.store.letters_for(player.data.id).await?.into_iter().find(|letter| letter.id == id))
}

/// Client 1600 int 0.
pub async fn open(session: &mut Session) -> Result<Outcome> {
    let player = active(&mut session.player)?;
    send_list(&session.conn, player).await?;
    Ok(Outcome::Answered)
}

/// Client 1610 long letter.
pub async fn read(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let id = message.reader().int64()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let Some(letter) = letter_of(conn, player, id).await? else {
        conn.log(&format!("   mail: no letter {id}"));
        return Ok(Outcome::Unanswered);
    };
    if !letter.seen {
        conn.world.store.mark_letter_seen(id).await?;
    }
    if !letter.items.is_empty() {
        inventory::send_updates(conn, 0, &letter.items).await?;
    }
    let sender = if letter.from > 0 { letter.from.max(REPLY_FROM) } else { 0 };
    let mut w = AfcWriter::new();
    w.int64(letter.id).int64(sender).string(&letter.from_name).string(&letter.subject).date_time(when(letter.sent_at));
    w.string(&letter.body).string(&attachment_line(&letter));
    conn.send(msg::MAIL_READ, w.finish()).await?;
    conn.log(&format!("   -> 1610 letter {id} from {}: {}", letter.from_name, attachment_line(&letter)));
    Ok(Outcome::Answered)
}

/// Client 1620 long letter: money and things into the character's bags, as
/// many things as there is room for.
pub async fn take(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let id = message.reader().int64()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let Some(letter) = letter_of(conn, player, id).await? else {
        return Ok(Outcome::Unanswered);
    };
    let mut said = Vec::new();
    if letter.money > 0 {
        player.data.money += letter.money;
        said.push(format!("{},{:02} кр.", letter.money / 100, letter.money % 100));
    }
    let mut placed = Vec::new();
    let mut left = Vec::new();
    for item in letter.items {
        let kind = conn.world.catalog.get(item.template).map_or(0, |template| template.kind);
        match inventory::put_in_free_cell(&conn.world.settings.bags, &mut player.data, item, kind) {
            Ok(put) => placed.push(put),
            Err(stays) => left.push(stays),
        }
    }
    conn.world.store.set_letter_contents(id, 0, &left).await?;
    conn.save(player).await?;
    if letter.money > 0 {
        character::send_money(conn, player, letter.money).await?;
    }
    if !placed.is_empty() {
        inventory::send_updates(conn, player.data.id, &placed).await?;
        said.push(format!("предметов: {}", placed.len()));
    }
    if !left.is_empty() {
        said.push(format!("не поместилось в сумки: {}, осталось в письме", left.len()));
    }
    if !said.is_empty() {
        chat::system(conn, &format!("Из письма: {}.", said.join(", "))).await?;
    }
    conn.log(&format!("   letter {id}: took {} item(s), {} left", placed.len(), left.len()));
    if !left.is_empty() {
        send_list(conn, player).await?;
    }
    Ok(Outcome::Answered)
}

/// Client 1640 long letter.
pub async fn delete(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let id = message.reader().int64()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    match letter_of(conn, player, id).await? {
        Some(letter) if !letter.carries() => {
            conn.world.store.delete_letter(id).await?;
            conn.log(&format!("   letter {id} deleted"));
        }
        Some(_) => {
            conn.log(&format!("   letter {id} still carries something, kept"));
            send_list(conn, player).await?;
        }
        None => {}
    }
    Ok(Outcome::Answered)
}

/// Client 1650 long: every letter that carries nothing.
pub async fn delete_all(session: &mut Session) -> Result<Outcome> {
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let letters = conn.world.store.letters_for(player.data.id).await?;
    let mut gone = 0;
    for letter in letters.iter().filter(|letter| !letter.carries()) {
        conn.world.store.delete_letter(letter.id).await?;
        gone += 1;
    }
    conn.log(&format!("   {gone} letter(s) deleted"));
    send_list(conn, player).await?;
    Ok(Outcome::Answered)
}

/// Client 45 string name: characters whose name has it.
pub async fn search(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let wanted = message.reader().string()?.trim().to_lowercase();
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let most = conn.world.settings.mail.most_found.max(1);
    let mut w = AfcWriter::new();
    let mut found = 0;
    for summary in conn.world.store.all_characters().await? {
        if summary.id == player.data.id || !summary.name.to_lowercase().contains(&wanted) {
            continue;
        }
        let (base, location) = conn.world.live_one(summary.id).map_or((0, 0), |live| (live.base, live.location));
        w.int64(summary.id).string(&summary.name).int32(base).int32(location);
        found += 1;
        if found >= most {
            break;
        }
    }
    conn.send(msg::PLAYER_SEARCH, w.finish()).await?;
    conn.log(&format!("   -> 45 {found} character(s) match «{wanted}»"));
    Ok(Outcome::Answered)
}

async fn answer(conn: &Conn, code: i32) -> Result<Outcome> {
    let mut w = AfcWriter::new();
    w.int32(code);
    conn.send(msg::MAIL_SEND, w.finish()).await?;
    conn.log(&format!("   -> 1660 {code}"));
    Ok(Outcome::Answered)
}

/// Client 1660: string to, string subject, string text, long money, int n,
/// n × long item.
pub async fn send(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let mut r = message.reader();
    let (to, subject, body, money) = (r.string()?, r.string()?, r.string()?, r.int64()?.max(0));
    let count = r.int32()?.clamp(0, 16);
    let mut ids = Vec::new();
    for _ in 0..count {
        ids.push(r.int64()?);
    }
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let world = conn.world.clone();
    let settings = &world.settings;
    if player.data.level < settings.mail.min_level {
        return answer(conn, FAILED).await;
    }
    let wanted = to.trim().to_lowercase();
    let Some(recipient) = world.store.all_characters().await?.into_iter().find(|summary| summary.name.to_lowercase() == wanted) else {
        return answer(conn, NO_RECIPIENT).await;
    };
    if recipient.id == player.data.id {
        return answer(conn, TO_ONESELF).await;
    }
    let mut items = Vec::new();
    for id in &ids {
        match sendable(&world, &player.data, *id) {
            Some(item) if !items.iter().any(|found: &ItemEntry| found.id == item.id) => items.push(item),
            _ => return answer(conn, NO_ITEMS).await,
        }
    }
    let price = cost(&settings.mail, settings.vip.mail_cost_percent, vip::active(&player.data), money, items.len());
    if player.data.money < money + price {
        return answer(conn, NO_MONEY).await;
    }

    player.data.money -= money + price;
    player.data.items.retain(|item| !ids.contains(&item.id));
    conn.save(player).await?;
    let letter = Letter {
        id: world.store.next_id().await?,
        to: recipient.id,
        from: player.data.id,
        from_name: player.data.name.clone(),
        subject,
        body,
        money,
        sent_at: Utc::now().timestamp(),
        seen: false,
        items,
    };
    world.store.send_letter(&letter).await?;
    world.notify_mail(recipient.id);
    for id in &ids {
        inventory::send_removed(conn, *id).await?;
    }
    character::send_money(conn, player, -(money + price)).await?;
    conn.log(&format!("   letter {} to {} ({}), {} money, {} item(s), cost {price}", letter.id, recipient.name, recipient.id, money, letter.items.len()));
    answer(conn, SENT).await
}

/// Case 275: «Вам пришло письмо».
async fn notice(conn: &Conn) -> Result<()> {
    let mut w = AfcWriter::new();
    w.int32(0);
    conn.send(msg::MAIL_NOTICE, w.finish()).await?;
    conn.log("   -> 275 new mail");
    Ok(())
}

/// On entering the game: tells of letters not read yet.
pub async fn enter(conn: &Conn, player: &Player) -> Result<()> {
    conn.world.take_mail_notice(player.data.id);
    if conn.world.store.letters_for(player.data.id).await?.iter().any(|letter| !letter.seen) {
        notice(conn).await?;
    }
    Ok(())
}

/// In play: tells of a letter that came since.
pub async fn check(conn: &Conn, player: &Player) -> Result<()> {
    if conn.world.take_mail_notice(player.data.id) {
        notice(conn).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> MailSettings {
        MailSettings { min_level: 5, base_cost: 10, money_percent: 10.0, item_cost: 100, most_found: 20 }
    }

    #[test]
    fn a_letter_costs_what_the_client_shows_and_half_with_vip() {
        assert_eq!(cost(&settings(), 50.0, false, 0, 0), 10);
        assert_eq!(cost(&settings(), 50.0, false, 10_000, 2), 10 + 1000 + 200);
        assert_eq!(cost(&settings(), 50.0, true, 10_000, 2), 605);
    }

    #[test]
    fn the_attachment_line_names_money_and_items() {
        let item = |id| ItemEntry { id, bag: 0, cell: 0, count: 1, durability: 1.0, template: 1001, param: 0, parts: [0; 4] };
        let letter = Letter { money: 250, items: vec![item(7), item(9)], ..Default::default() };
        assert_eq!(attachment_line(&letter), "money=250;weapon=7;weapon=9");
        assert_eq!(attachment_line(&Letter::default()), "");
    }
}
