//! The С.П.А.С. terminal: NPC 2 in the base scenes from the Карасиный пруд on
//! (in Убежище NPC 2 is the generator). Its dialog is the client's `3.lng`.
//!
//! The root (node 1) offers help (answer 2, node 2), currencies (5, node 4) and
//! patents (4, node 8). Currencies trade trophy fish from both keepnets for
//! Trophy Hunter's tokens (answer 8, node 6): all of them at once (10) or one
//! fish (14, the fish in the long, `[m_cnt]` the tokens). Patents are VIP for
//! 30, 7 or 1 day (17, 18, 19) at the ergs their lines name; short of ergs,
//! node 9. Records, unique fish, the auction, the closed-test prize and the
//! skill reset are in the file but not offered: the server has no world
//! records, no unique fish, no auction, and the rest belonged to the old server.

use anyhow::Result;

use super::dialog::{close, open, option, replace, text};
use super::fishing::TROPHY_CLASS;
use super::fridge::FIRST_LIST;
use super::{Conn, Outcome, Player, Session, active, chat, inventory, travel, vip};
use crate::afc::{AfcMessage, AfcWriter};
use crate::config::SpasSettings;
use crate::messages as msg;
use crate::storage::CaughtFish;

const ROOT: i32 = 1;
const HELP: i32 = 2;
const CURRENCIES: i32 = 4;
const TROPHIES: i32 = 6;
const PATENTS: i32 = 8;
const SHORT: i32 = 9;

const LEAVE: i32 = 1;
const ASK_HELP: i32 = 2;
const HELP_BACK: i32 = 3;
const ASK_PATENTS: i32 = 4;
const ASK_CURRENCIES: i32 = 5;
const CURRENCIES_BACK: i32 = 7;
const ASK_TROPHIES: i32 = 8;
const ALL_TROPHIES: i32 = 10;
const TROPHIES_BACK: i32 = 11;
const ONE_TROPHY: i32 = 14;
const PATENTS_BACK: i32 = 20;
const SHORT_BACK: i32 = 21;

const TOKENS_MARK: &str = "[m_cnt]";

fn is_here(conn: &Conn, player: &Player, npc: i64) -> bool {
    let spas = &conn.world.settings.spas;
    npc == i64::from(spas.npc) && spas.bases.contains(&travel::current_base(&conn.world.settings, player))
}

/// Whether the dialog open is the terminal's.
pub fn is_open(session: &Session) -> bool {
    let spas = &session.conn.world.settings.spas;
    spas.dialog > 0 && session.player.as_ref().and_then(|player| player.dialog) == Some(spas.dialog)
}

/// Tokens for one trophy: a few for any, one more for every so many grams.
pub fn tokens_for(spas: &SpasSettings, fish: &CaughtFish) -> i32 {
    spas.trophy_tokens + fish.grams / spas.grams_per_token.max(1)
}

fn is_trophy(fish: &CaughtFish) -> bool {
    fish.size_class == TROPHY_CLASS && fish.list < FIRST_LIST
}

async fn show(conn: &Conn, player: &mut Player, node: i32, answers: &[i32]) -> Result<()> {
    let file = conn.world.settings.spas.dialog;
    text(conn, file, node).await?;
    for answer in answers {
        option(conn, file, node, *answer, 0).await?;
    }
    open(conn, player, file).await
}

async fn root(conn: &Conn, player: &mut Player) -> Result<()> {
    show(conn, player, ROOT, &[ASK_CURRENCIES, ASK_PATENTS, ASK_HELP, LEAVE]).await
}

async fn trophies(conn: &Conn, player: &mut Player) -> Result<()> {
    let spas = &conn.world.settings.spas;
    let file = spas.dialog;
    let fish: Vec<CaughtFish> = player.data.fish.iter().filter(|fish| is_trophy(fish)).cloned().collect();
    text(conn, file, TROPHIES).await?;
    if fish.len() > 1 {
        option(conn, file, TROPHIES, ALL_TROPHIES, 0).await?;
    }
    // Case 78 changes the mark in the answers shown so far, so each fish gets
    // its own count when it is changed right after the fish's answer.
    for one in fish.iter().take(spas.max_listed) {
        option(conn, file, TROPHIES, ONE_TROPHY, one.id).await?;
        replace(conn, TOKENS_MARK, &tokens_for(spas, one).to_string()).await?;
    }
    option(conn, file, TROPHIES, TROPHIES_BACK, 0).await?;
    open(conn, player, file).await?;
    conn.log(&format!("   -> 70/72/74 terminal: {} trophies", fish.len()));
    Ok(())
}

/// Takes the fish and gives their tokens; `false` if the tokens do not fit.
async fn exchange(conn: &Conn, player: &mut Player, ids: &[i64]) -> Result<bool> {
    let spas = &conn.world.settings.spas;
    let tokens: i32 = player.data.fish.iter().filter(|fish| ids.contains(&fish.id)).map(|fish| tokens_for(spas, fish)).sum();
    let Some(item) = conn.world.catalog.get(spas.trophy_token) else {
        conn.log(&format!("   terminal: token {} is not in the catalog", spas.trophy_token));
        return Ok(false);
    };
    if tokens <= 0 {
        return Ok(true);
    }
    let changed = match inventory::give(conn, player, item, tokens).await? {
        Ok(changed) => changed,
        Err(error) => {
            conn.log(&format!("   terminal: {tokens} tokens: {}", error.status()));
            chat::system(conn, "Жетоны трофейщика не поместились в рюкзак: рыба осталась в садке.").await?;
            return Ok(false);
        }
    };
    player.data.fish.retain(|fish| !ids.contains(&fish.id));
    conn.save(player).await?;
    for id in ids {
        let mut w = AfcWriter::new();
        w.int64(*id);
        conn.send(msg::FISH_REMOVE, w.finish()).await?;
    }
    inventory::send_updates(conn, player.data.id, &changed).await?;
    let fish = ids.len();
    chat::system(conn, &format!("Терминал С.П.А.С.: трофеев сдано {fish}, получено жетонов трофейщика: {tokens}.")).await?;
    conn.log(&format!("   -> 173, 181 terminal took {fish} trophies for {tokens} tokens"));
    Ok(true)
}

async fn patent(conn: &Conn, player: &mut Player, answer: i32) -> Result<()> {
    let Some(patent) = conn.world.settings.spas.patents.iter().find(|patent| patent.answer == answer) else {
        return root(conn, player).await;
    };
    let (days, premium) = (patent.days, patent.premium);
    if !vip::grant(conn, player, days, premium).await? {
        conn.log(&format!("   terminal: VIP for {days} days costs {premium}, {} ergs", player.data.premium));
        return show(conn, player, SHORT, &[SHORT_BACK]).await;
    }
    chat::system(conn, &format!("Терминал С.П.А.С.: VIP-патент на {days} дн. куплен за {premium} эргов.")).await?;
    conn.log(&format!("   terminal sold VIP for {days} days"));
    root(conn, player).await
}

/// Client 60 for the terminal. `None` leaves the NPC to the others.
pub async fn talk(session: &mut Session, message: &AfcMessage) -> Result<Option<Outcome>> {
    let npc = message.reader().int64()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    if !is_here(conn, player, npc) {
        return Ok(None);
    }
    root(conn, player).await?;
    conn.log("   -> 70/72/74 terminal");
    Ok(Some(Outcome::Answered))
}

/// Client 65 while the terminal's dialog is open.
pub async fn answer(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let mut r = message.reader();
    let answer = r.int32()?;
    let param = r.int64()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let patents: Vec<i32> = conn.world.settings.spas.patents.iter().map(|patent| patent.answer).collect();
    match answer {
        ASK_HELP => show(conn, player, HELP, &[HELP_BACK]).await?,
        ASK_CURRENCIES => show(conn, player, CURRENCIES, &[ASK_TROPHIES, CURRENCIES_BACK]).await?,
        ASK_TROPHIES => trophies(conn, player).await?,
        ALL_TROPHIES | ONE_TROPHY => {
            let ids: Vec<i64> = player
                .data
                .fish
                .iter()
                .filter(|fish| is_trophy(fish) && (answer == ALL_TROPHIES || fish.id == param))
                .map(|fish| fish.id)
                .collect();
            exchange(conn, player, &ids).await?;
            trophies(conn, player).await?;
        }
        ASK_PATENTS => {
            let mut answers = patents;
            answers.push(PATENTS_BACK);
            show(conn, player, PATENTS, &answers).await?;
        }
        answer if patents.contains(&answer) => patent(conn, player, answer).await?,
        HELP_BACK | CURRENCIES_BACK | TROPHIES_BACK | PATENTS_BACK | SHORT_BACK => root(conn, player).await?,
        _ => {
            let file = conn.world.settings.spas.dialog;
            close(conn, player, file).await?;
        }
    }
    Ok(Outcome::Answered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fish(grams: i32, size_class: i32, list: i32) -> CaughtFish {
        CaughtFish { id: 1, species: 7, grams, size_class, bait: 1010, list, experience: 1, traits: Vec::new() }
    }

    #[test]
    fn heavier_trophies_give_more_tokens() {
        let spas = SpasSettings { trophy_tokens: 1, grams_per_token: 1000, ..Default::default() };
        assert_eq!(tokens_for(&spas, &fish(800, TROPHY_CLASS, 0)), 1);
        assert_eq!(tokens_for(&spas, &fish(3500, TROPHY_CLASS, 0)), 4);
    }

    #[test]
    fn only_trophies_in_the_keepnets_count() {
        assert!(is_trophy(&fish(3500, TROPHY_CLASS, 1)));
        assert!(!is_trophy(&fish(3500, TROPHY_CLASS, FIRST_LIST)));
        assert!(!is_trophy(&fish(3500, 1, 0)));
    }
}
