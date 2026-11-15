//! The С.П.А.С. terminal: NPC 2 in the base scenes from the Пустошь on, NPC 3
//! in Убежище (where NPC 2 is the generator). Its dialog is the client's `3.lng`.
//!
//! The root (node 1) offers help (answer 2, node 2), currencies (5, node 4),
//! patents (4, node 8) and, from level 25, the free reset course once (31,
//! node 13: take it 32, later 33). Currencies trade fish from both keepnets
//! for tokens: trophies for Trophy Hunter's (answer 8, node 6: all at once 10,
//! or one fish 14, the fish in the long, `[m_cnt]` the tokens), the
//! character's own record fish for Record Holder's (9, node 7, one fish 12)
//! and, once Лиза has opened it, fish with traits for Explorer's (29, node 12,
//! one fish 30). Patents are VIP for 30, 7 or 1 day (17, 18, 19) at the ergs
//! their lines name; short of ergs, node 9. The auction and the closed-test
//! prize are in the file but not offered.

use anyhow::Result;

use super::dialog::{close, open, option, replace, text};
use super::fishing::TROPHY_CLASS;
use super::fridge::FIRST_LIST;
use super::{Conn, Outcome, Player, Session, active, chat, inventory, travel, vip};
use crate::afc::{AfcMessage, AfcWriter};
use crate::config::SpasSettings;
use crate::messages as msg;
use crate::storage::{CaughtFish, CharacterData};

const ROOT: i32 = 1;
const HELP: i32 = 2;
const CURRENCIES: i32 = 4;
const PATENTS: i32 = 8;
const SHORT: i32 = 9;
const RESET: i32 = 13;

const LEAVE: i32 = 1;
const ASK_HELP: i32 = 2;
const HELP_BACK: i32 = 3;
const ASK_PATENTS: i32 = 4;
const ASK_CURRENCIES: i32 = 5;
const CURRENCIES_BACK: i32 = 7;
const PATENTS_BACK: i32 = 20;
const SHORT_BACK: i32 = 21;
const ASK_RESET: i32 = 31;
const TAKE_RESET: i32 = 32;
const RESET_LATER: i32 = 33;
/// Quest value: the free reset course has been given.
const RESET_TAKEN: &str = "spas_reset_course";

const TOKENS_MARK: &str = "[m_cnt]";

/// Which fish go for which tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Trade {
    Trophies,
    Records,
    Unique,
}

/// A trade's answers: the currencies line, its node, one fish, all of them
/// (0: not offered) and back.
struct Page {
    ask: i32,
    node: i32,
    one: i32,
    all: i32,
    back: i32,
}

impl Trade {
    const ALL: [Trade; 3] = [Trade::Trophies, Trade::Records, Trade::Unique];

    fn page(self) -> Page {
        match self {
            Trade::Trophies => Page { ask: 8, node: 6, one: 14, all: 10, back: 11 },
            Trade::Records => Page { ask: 9, node: 7, one: 12, all: 0, back: 13 },
            Trade::Unique => Page { ask: 29, node: 12, one: 30, all: 0, back: 16 },
        }
    }

    fn token(self, spas: &SpasSettings) -> i32 {
        match self {
            Trade::Trophies => spas.trophy_token,
            Trade::Records => spas.record_token,
            Trade::Unique => spas.unique_token,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Trade::Trophies => "трофейщика",
            Trade::Records => "рекордсмена",
            Trade::Unique => "исследователя",
        }
    }

    fn tokens_for(self, spas: &SpasSettings, fish: &CaughtFish) -> i32 {
        match self {
            Trade::Trophies => tokens_for(spas, fish),
            Trade::Records => spas.record_tokens,
            Trade::Unique => spas.unique_tokens,
        }
    }

    /// Whether the fish, in a keepnet, goes for this trade's tokens.
    fn takes(self, character: &CharacterData, fish: &CaughtFish) -> bool {
        fish.list < FIRST_LIST
            && match self {
                Trade::Trophies => fish.size_class == TROPHY_CLASS,
                Trade::Records => character.records.get(&fish.species).is_some_and(|record| record.grams == fish.grams),
                Trade::Unique => !fish.traits.is_empty(),
            }
    }

    fn offered(self, spas: &SpasSettings, character: &CharacterData) -> bool {
        self.token(spas) > 0
            && (self != Trade::Unique || character.quest_values.get(&spas.unique_flag).is_some_and(|value| *value > 0))
    }
}

fn is_here(conn: &Conn, player: &Player, npc: i64) -> bool {
    let spas = &conn.world.settings.spas;
    spas.npcs.get(&travel::current_base(&conn.world.settings, player)).is_some_and(|here| i64::from(*here) == npc)
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

async fn show(conn: &Conn, player: &mut Player, node: i32, answers: &[i32]) -> Result<()> {
    let file = conn.world.settings.spas.dialog;
    text(conn, file, node).await?;
    for answer in answers {
        option(conn, file, node, *answer, 0).await?;
    }
    open(conn, player, file).await
}

fn reset_offered(spas: &SpasSettings, character: &CharacterData) -> bool {
    spas.reset_course > 0 && character.level >= spas.reset_level && !character.quest_values.contains_key(RESET_TAKEN)
}

async fn root(conn: &Conn, player: &mut Player) -> Result<()> {
    let mut answers = vec![ASK_CURRENCIES, ASK_PATENTS];
    if reset_offered(&conn.world.settings.spas, &player.data) {
        answers.push(ASK_RESET);
    }
    answers.extend([ASK_HELP, LEAVE]);
    show(conn, player, ROOT, &answers).await
}

async fn currencies(conn: &Conn, player: &mut Player) -> Result<()> {
    let spas = &conn.world.settings.spas;
    let mut answers: Vec<i32> =
        Trade::ALL.iter().filter(|trade| trade.offered(spas, &player.data)).map(|trade| trade.page().ask).collect();
    answers.push(CURRENCIES_BACK);
    show(conn, player, CURRENCIES, &answers).await
}

async fn fish_page(conn: &Conn, player: &mut Player, trade: Trade) -> Result<()> {
    let spas = &conn.world.settings.spas;
    let file = spas.dialog;
    let page = trade.page();
    let fish: Vec<CaughtFish> = player.data.fish.iter().filter(|fish| trade.takes(&player.data, fish)).cloned().collect();
    text(conn, file, page.node).await?;
    if page.all > 0 && fish.len() > 1 {
        option(conn, file, page.node, page.all, 0).await?;
    }
    // Case 78 changes the mark in the answers shown so far, so each fish gets
    // its own count when it is changed right after the fish's answer.
    for one in fish.iter().take(spas.max_listed) {
        option(conn, file, page.node, page.one, one.id).await?;
        replace(conn, TOKENS_MARK, &trade.tokens_for(spas, one).to_string()).await?;
    }
    option(conn, file, page.node, page.back, 0).await?;
    open(conn, player, file).await?;
    conn.log(&format!("   -> 70/72/74 terminal: {} fish for {trade:?}", fish.len()));
    Ok(())
}

/// Takes the fish and gives their tokens; `false` if the tokens do not fit.
async fn exchange(conn: &Conn, player: &mut Player, trade: Trade, ids: &[i64]) -> Result<bool> {
    let spas = &conn.world.settings.spas;
    let tokens: i32 =
        player.data.fish.iter().filter(|fish| ids.contains(&fish.id)).map(|fish| trade.tokens_for(spas, fish)).sum();
    let token = trade.token(spas);
    let Some(item) = conn.world.catalog.get(token) else {
        conn.log(&format!("   terminal: token {token} is not in the catalog"));
        return Ok(false);
    };
    if tokens <= 0 {
        return Ok(true);
    }
    let changed = match inventory::give(conn, player, item, tokens).await? {
        Ok(changed) => changed,
        Err(error) => {
            conn.log(&format!("   terminal: {tokens} tokens: {}", error.status()));
            chat::system(conn, "Жетоны не поместились в рюкзак: рыба осталась в садке.").await?;
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
    let name = trade.name();
    chat::system(conn, &format!("Терминал С.П.А.С.: рыбы сдано {fish}, получено жетонов {name}: {tokens}.")).await?;
    conn.log(&format!("   -> 173, 181 terminal took {fish} fish for {tokens} tokens {token}"));
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

/// Gives the free reset course once, then back to the root.
async fn reset_course(conn: &Conn, player: &mut Player) -> Result<()> {
    let spas = &conn.world.settings.spas;
    let course = conn.world.catalog.get(spas.reset_course);
    if let (true, Some(item)) = (reset_offered(spas, &player.data), course) {
        match inventory::give(conn, player, item, 1).await? {
            Ok(changed) => {
                player.data.quest_values.insert(RESET_TAKEN.into(), 1);
                conn.save(player).await?;
                inventory::send_updates(conn, player.data.id, &changed).await?;
                chat::system(conn, &format!("Терминал С.П.А.С.: получен «{}».", item.name)).await?;
                conn.log(&format!("   terminal gave the reset course {}", spas.reset_course));
            }
            Err(error) => {
                conn.log(&format!("   terminal: reset course: {}", error.status()));
                chat::system(conn, "Курс не помещается в рюкзак.").await?;
            }
        }
    }
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
    let spas = &conn.world.settings.spas;
    let patents: Vec<i32> = spas.patents.iter().map(|patent| patent.answer).collect();
    let offered: Vec<Trade> = Trade::ALL.into_iter().filter(|trade| trade.offered(spas, &player.data)).collect();
    if let Some(trade) = offered.iter().copied().find(|trade| [trade.page().ask, trade.page().one, trade.page().all].contains(&answer)) {
        let page = trade.page();
        if answer != page.ask {
            let ids: Vec<i64> = player
                .data
                .fish
                .iter()
                .filter(|fish| trade.takes(&player.data, fish) && (answer == page.all || fish.id == param))
                .map(|fish| fish.id)
                .collect();
            exchange(conn, player, trade, &ids).await?;
        }
        fish_page(conn, player, trade).await?;
        return Ok(Outcome::Answered);
    }
    let backs: Vec<i32> = Trade::ALL.iter().map(|trade| trade.page().back).collect();
    match answer {
        ASK_HELP => show(conn, player, HELP, &[HELP_BACK]).await?,
        ASK_CURRENCIES => currencies(conn, player).await?,
        ASK_RESET => show(conn, player, RESET, &[TAKE_RESET, RESET_LATER]).await?,
        TAKE_RESET => reset_course(conn, player).await?,
        ASK_PATENTS => {
            let mut answers = patents;
            answers.push(PATENTS_BACK);
            show(conn, player, PATENTS, &answers).await?;
        }
        answer if patents.contains(&answer) => patent(conn, player, answer).await?,
        answer if backs.contains(&answer) => currencies(conn, player).await?,
        HELP_BACK | CURRENCIES_BACK | PATENTS_BACK | SHORT_BACK | RESET_LATER => root(conn, player).await?,
        _ => {
            let file = spas.dialog;
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
        let character = CharacterData::default();
        assert!(Trade::Trophies.takes(&character, &fish(3500, TROPHY_CLASS, 1)));
        assert!(!Trade::Trophies.takes(&character, &fish(3500, TROPHY_CLASS, FIRST_LIST)));
        assert!(!Trade::Trophies.takes(&character, &fish(3500, 1, 0)));
    }

    #[test]
    fn records_and_unique_fish_are_trades_of_their_own() {
        let mut character = CharacterData::default();
        character.records.insert(7, crate::storage::Record { grams: 3500, location: 1, bait: 1010 });
        assert!(Trade::Records.takes(&character, &fish(3500, 1, 0)));
        assert!(!Trade::Records.takes(&character, &fish(3400, 1, 0)));
        let marked = CaughtFish { traits: vec![108], ..fish(3500, 1, 0) };
        assert!(Trade::Unique.takes(&character, &marked));
        assert!(!Trade::Unique.takes(&character, &fish(3500, 1, 0)));
        let spas = SpasSettings { unique_token: 1180, unique_flag: "spas_unique".into(), ..Default::default() };
        assert!(!Trade::Unique.offered(&spas, &character));
        character.quest_values.insert("spas_unique".into(), 1);
        assert!(Trade::Unique.offered(&spas, &character));
    }

    #[test]
    fn the_reset_course_is_given_once_from_its_level() {
        let spas = SpasSettings { reset_course: 1407, reset_level: 25, ..Default::default() };
        let mut character = CharacterData { level: 24, ..Default::default() };
        assert!(!reset_offered(&spas, &character));
        character.level = 25;
        assert!(reset_offered(&spas, &character));
        character.quest_values.insert(RESET_TAKEN.into(), 1);
        assert!(!reset_offered(&spas, &character));
    }
}
