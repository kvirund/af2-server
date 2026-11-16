//! The board of tasks at a base.
//!
//! A click on the board sends 700 with a tab: 1000 income, 1010 resources,
//! 1020 experience and reputation, 1100 daily, 1110 tournaments. Case 700 back
//! opens the window: int tasks taken, int most at once, date of new offers,
//! then the offers until the end (see [`write_offer`]). «Принять» sends 710
//! with the offer; the answer is one `key=value` line: `ok=1`, or `no_task`,
//! `no_repa`, `no_get`, `big_count`, `no_time_period`. «Отказаться» sends 705
//! with the tab and gives up its tasks.
//!
//! Taken tasks are the client's top-right list: 675 replaces it, 680 adds or
//! changes tasks (see [`write_task`]), 670 tells what happened to them, and
//! 672 takes a fish a task used out of the keepnet. Every task is to catch
//! fish at the base it was taken at, kept with «в садок» (1135), not «Без
//! квеста» (1136); the reward comes when it is done.

use std::collections::BTreeSet;

use anyhow::Result;
use chrono::{DateTime, Local, NaiveDateTime, Utc};
use rand::random;

use super::fishing::{LARGE_CLASS, TROPHY_CLASS, experience_for};
use super::{Conn, Outcome, Player, Session, active, character, chat, inventory, keepnet, reputation, tournaments, travel};
use crate::afc::{AfcMessage, AfcWriter};
use crate::config::{BoardSettings, FishTable, Settings, Species};
use crate::messages as msg;
use crate::storage::{BoardTask, CaughtFish, CharacterData};
use crate::world::{BoardOffer, BoardOffers};

/// Categories below this are NPC quests (`quests.rs`), not the board's.
pub(super) const FIRST_BOARD_CATEGORY: i32 = 1000;
const INCOME: i32 = 1000;
const RESOURCES: i32 = 1010;
const EXPERIENCE: i32 = 1020;
const DAILY: i32 = 1100;
const TOURNAMENTS: i32 = 1110;

/// Task texts in the client's QTask.xml, the same for the four tabs.
const COUNT: i32 = 1;
const COUNT_FROM: i32 = 2;
const TROPHIES: i32 = 4;
const WEIGHT_FROM: i32 = 5;
const WEIGHT: i32 = 6;
const WEIGHT_ON_BAIT: i32 = 7;

/// What a task asks for: the client reads 4 and 5 as items, anything else as fish.
const FISH: i32 = 1;
const GOING: i32 = 0;
const DONE: i32 = 1;

/// Commands of case 670.
const GIVE_UP_TAB: i32 = 1;
pub(super) const REMOVE: i32 = 2;
pub(super) const TAKEN: i32 = 3;
pub(super) const FINISHED: i32 = 4;
pub(super) const PROGRESS: i32 = 5;

/// Names in QTask.xml `qdynamic`.
const TITLE_CONTROL: i32 = 1;
const TITLE_INVOICE: i32 = 2;
const TITLE_FREE: i32 = 3;
const TITLE_TROPHY: i32 = 4;
const TITLE_LAB: i32 = 8;
const TITLE_EXTREME: i32 = 9;

/// The last daily task taken, Unix seconds.
const DAILY_TAKEN_AT: &str = "board_daily_at";

fn now_seconds() -> i64 {
    Utc::now().timestamp()
}

fn local(at: i64) -> NaiveDateTime {
    DateTime::from_timestamp(at, 0).map_or_else(|| Local::now().naive_local(), |at| at.with_timezone(&Local).naive_local())
}

/// The "Общий вес" names the bases of QTask.xml have.
fn total_weight_title(base: i32) -> i32 {
    match base {
        1 => 14,
        6 => 15,
        3 => 16,
        4 => 17,
        7 => 22,
        8 => 26,
        _ => TITLE_FREE,
    }
}

fn kinds(category: i32) -> &'static [i32] {
    match category {
        INCOME => &[COUNT, COUNT_FROM, WEIGHT_FROM, WEIGHT],
        DAILY => &[COUNT, COUNT_FROM, WEIGHT, WEIGHT_ON_BAIT],
        _ => &[COUNT, COUNT_FROM, TROPHIES, WEIGHT_FROM, WEIGHT, WEIGHT_ON_BAIT],
    }
}

fn is_weighed(task: i32) -> bool {
    matches!(task, WEIGHT_FROM | WEIGHT | WEIGHT_ON_BAIT)
}

fn pick<T: Copy>(roll: f64, from: &[T]) -> T {
    from[((roll * from.len() as f64) as usize).min(from.len() - 1)]
}

fn between(roll: f64, range: [i32; 2]) -> i32 {
    range[0] + (roll * f64::from(range[1] - range[0] + 1)) as i32
}

fn quality_of(weights: &[f64], roll: f64) -> i32 {
    let total: f64 = weights.iter().sum();
    let mut left = roll * total;
    for (quality, weight) in weights.iter().enumerate() {
        left -= weight;
        if left < 0.0 {
            return quality as i32;
        }
    }
    weights.len().saturating_sub(1) as i32
}

/// The species of a base's locations, rarer ones less often.
fn species_at<'a>(settings: &Settings, fish: &'a FishTable, base: i32) -> Vec<&'a Species> {
    let ids: BTreeSet<i32> = settings
        .bases
        .get(&base)
        .into_iter()
        .flat_map(|found| found.locations.iter())
        .filter_map(|location| fish.by_location.get(location))
        .flatten()
        .copied()
        .collect();
    ids.into_iter().filter_map(|id| fish.get(id)).collect()
}

fn pick_species<'a>(pool: &[&'a Species], roll: f64) -> &'a Species {
    let total: f64 = pool.iter().map(|species| species.share).sum();
    let mut left = roll * total;
    for species in pool {
        left -= species.share;
        if left < 0.0 {
            return species;
        }
    }
    pool[pool.len() - 1]
}

/// What resource tasks on this base's board may give.
fn resource_items(board: &BoardSettings, base: i32) -> Vec<i32> {
    let mut items = board.resource_items.clone();
    items.extend(board.base_resource_items.get(&base).into_iter().flatten());
    items
}

/// One offer: the fish, what of it, and the reward, which grows with the
/// worth and experience of the fish asked for.
fn make_offer(settings: &Settings, fish: &FishTable, base: i32, category: i32, ends_at: i64, roll: &mut impl FnMut() -> f64) -> Option<BoardOffer> {
    let board = &settings.board;
    let pool = species_at(settings, fish, base);
    if pool.is_empty() {
        return None;
    }
    let species = pick_species(&pool, roll());
    let mut task = pick(roll(), kinds(category));
    if task == WEIGHT_ON_BAIT && species.baits.is_empty() {
        task = WEIGHT;
    }
    let quality = quality_of(&board.quality_weights, roll());
    let size = 1.0 + board.quality_percent / 100.0 * f64::from(quality);
    let (min_grams, typical, size_class) = match task {
        COUNT_FROM | WEIGHT_FROM => (species.large_grams, (species.large_grams + species.trophy_grams) / 2, LARGE_CLASS),
        TROPHIES => (species.trophy_grams, species.trophy_grams + species.trophy_grams / 10, TROPHY_CLASS),
        _ => (0, (species.small_grams + species.large_grams) / 2, 0),
    };
    let count = if task == TROPHIES {
        between(roll(), board.trophies) + quality / 2
    } else {
        let range = if is_weighed(task) { board.weight_count } else { board.count };
        (f64::from(between(roll(), range)) * size).round() as i32
    }
    .max(1);
    let target = if is_weighed(task) { ((count * typical) / 100 * 100).max(100) } else { count };
    let bait = if task == WEIGHT_ON_BAIT { pick(roll(), &species.baits) } else { 0 };

    let worth = keepnet::value_of(fish, &settings.fish_market, species, count * typical, size_class) as f64;
    let experience = f64::from(count * experience_for(&settings.progression, typical, size_class, 1.0));
    let mut reward = Vec::new();
    match category {
        INCOME => reward.push(format!("money={}", (worth * board.income_percent / 100.0 * size).round() as i64)),
        RESOURCES => {
            let items = resource_items(board, base);
            if !items.is_empty() {
                reward.push(format!("wpn={}/{}", pick(roll(), &items), quality + 1));
            }
        }
        EXPERIENCE => {
            reward.push(format!("exp={}", (experience * board.experience_percent / 100.0 * size).round() as i64));
            if let Some(faction) = reputation::base_faction(&settings.reputation, base) {
                reward.push(format!("repfrac={faction}/{}", board.reputation_points * i64::from(quality + 1)));
            }
        }
        DAILY => {
            reward.push(format!("money={}", (worth * board.daily_income_percent / 100.0).round() as i64));
            reward.push(format!("exp={}", (experience * board.daily_experience_percent / 100.0).round() as i64));
        }
        _ => {}
    }
    let title = match task {
        COUNT => TITLE_CONTROL,
        COUNT_FROM => TITLE_INVOICE,
        TROPHIES => TITLE_TROPHY,
        WEIGHT_FROM => TITLE_EXTREME,
        WEIGHT_ON_BAIT => TITLE_LAB,
        _ => total_weight_title(base),
    };
    let reputation = board.quality_reputation.get(quality as usize).copied().unwrap_or(0);
    Some(BoardOffer {
        id: 0,
        task: BoardTask {
            kind: FISH,
            id: 0,
            base,
            category,
            task,
            quality,
            title,
            species: species.id,
            bait,
            target,
            min_grams,
            max_grams: 0,
            progress: 0,
            reward: reward.join(";"),
            ends_at: if category == DAILY { ends_at } else { 0 },
        },
        reputation,
        copies: between(roll(), board.copies).max(1),
        takers: Default::default(),
    })
}

fn refresh_minutes(board: &BoardSettings, category: i32) -> i64 {
    if category == DAILY { board.daily_refresh_minutes } else { board.refresh_minutes }
}

/// New offers for a tab, when its old ones have run out.
fn refresh(conn: &Conn, offers: &mut BoardOffers, base: i32, category: i32, now: i64) {
    if now < offers.refresh_at {
        return;
    }
    let settings = &conn.world.settings;
    let board = &settings.board;
    let ends_at = now + refresh_minutes(board, category).max(1) * 60;
    let how_many = if category == DAILY { board.daily_offers } else { board.offers };
    let mut roll = random::<f64>;
    offers.refresh_at = ends_at;
    offers.offers = (0..how_many)
        .filter_map(|_| make_offer(settings, &conn.world.fish, base, category, ends_at, &mut roll))
        .map(|mut offer| {
            offer.id = conn.world.next_offer_id();
            offer
        })
        .collect();
}

/// Offer record of case 700.
fn write_offer(w: &mut AfcWriter, offer: &BoardOffer, ends_at: i64) {
    let task = &offer.task;
    w.int64(offer.id).int32(task.task).int32(task.category).int32(FISH).int32(task.quality).int32(offer.reputation).int32(task.title);
    w.int32(offer.copies - offer.takers.len() as i32).int32(offer.takers.len() as i32).int32(offer.copies);
    w.date_time(local(ends_at));
    w.string(&task.reward);
    w.int32(task.base).int32(0).int32(task.bait).int32(task.target).int32(task.species).int32(0);
    w.int32(task.min_grams).int32(task.max_grams);
}

/// Task record of cases 675 and 680.
pub(super) fn write_task(w: &mut AfcWriter, task: &BoardTask) {
    let status = if task.progress >= task.target { DONE } else { GOING };
    let kind = if task.kind == 0 { FISH } else { task.kind };
    w.int64(task.id).int32(kind).int32(task.category).int32(task.task).int32(status);
    w.int32(task.species).int32(task.bait).int32(task.target).int32(task.min_grams).int32(task.max_grams);
    w.int32(task.base).int32(0).int32(task.progress);
    w.boolean(task.ends_at > 0);
    w.date_time(local(if task.ends_at > 0 { task.ends_at } else { now_seconds() }));
    w.string(&task.reward);
}

pub(super) async fn send_tasks(conn: &Conn, id: i32, tasks: &[&BoardTask]) -> Result<()> {
    let mut w = AfcWriter::new();
    for task in tasks {
        write_task(&mut w, task);
    }
    conn.send(id, w.finish()).await
}

pub(super) async fn send_events(conn: &Conn, events: &[(i32, i64)]) -> Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    let mut w = AfcWriter::new();
    for (command, id) in events {
        w.int32(*command).int64(*id);
    }
    conn.send(msg::TASK_EVENTS, w.finish()).await
}

/// Case 15 `replev_<base>`: the base reputation level the board's offers are
/// checked against in the client.
pub async fn send_reputation_levels(conn: &Conn, player: &Player) -> Result<()> {
    let settings = &conn.world.settings.reputation;
    let mut w = AfcWriter::new();
    for (base, faction) in &settings.base_factions {
        w.string(&format!("replev_{base}")).int64(i64::from(reputation::level_with(settings, &player.data, *faction)));
    }
    conn.send(msg::USER_PARAMS, w.finish()).await
}

/// Drops the tasks whose time is up; the client hides them by itself.
async fn drop_expired(conn: &Conn, player: &mut Player) -> Result<()> {
    let now = now_seconds();
    let gone: Vec<i64> = player.data.board_tasks.iter().filter(|task| task.ends_at > 0 && task.ends_at <= now).map(|task| task.id).collect();
    if gone.is_empty() {
        return Ok(());
    }
    player.data.board_tasks.retain(|task| !gone.contains(&task.id));
    conn.save(player).await?;
    send_events(conn, &gone.iter().map(|id| (REMOVE, *id)).collect::<Vec<_>>()).await?;
    conn.log(&format!("   tasks {gone:?} ran out of time"));
    Ok(())
}

/// On entering the game: the task list and the reputation levels.
pub async fn enter(conn: &Conn, player: &mut Player) -> Result<()> {
    drop_expired(conn, player).await?;
    send_reputation_levels(conn, player).await?;
    send_tasks(conn, msg::TASKS, &player.data.board_tasks.iter().collect::<Vec<_>>()).await?;
    conn.log(&format!("   -> 15 replev, 675 {} task(s)", player.data.board_tasks.len()));
    super::quests::refresh(conn, player).await
}

/// Client 700 int tab.
pub async fn open(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let category = message.reader().int32()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    if category == TOURNAMENTS {
        return tournaments::tab(conn, player).await;
    }
    drop_expired(conn, player).await?;
    let settings = &conn.world.settings;
    let base = travel::current_base(settings, player);
    let taken = player.data.board_tasks.iter().filter(|task| task.category == category).count();
    let mut w = AfcWriter::new();
    let shown = {
        let mut boards = conn.world.boards.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let offers = boards.entry((base, category)).or_default();
        refresh(conn, offers, base, category, now_seconds());
        w.int32(taken as i32).int32(settings.board.max_taken as i32);
        w.date_time(local(offers.refresh_at));
        // What a player has taken leaves their board; others still see it with one copy fewer.
        let mut shown = 0;
        for offer in offers.offers.iter().filter(|offer| !offer.takers.contains(&player.data.id)) {
            write_offer(&mut w, offer, offers.refresh_at);
            shown += 1;
            let task = &offer.task;
            conn.log(&format!(
                "     offer {}: text {}, colour {}, rep {}, species {}, target {} from {} g, bait {}, {}",
                offer.id, task.task, task.quality, offer.reputation, task.species, task.target, task.min_grams, task.bait, task.reward
            ));
        }
        shown
    };
    conn.send(msg::BOARD, w.finish()).await?;
    conn.log(&format!("   -> 700 board of base {base}, tab {category}: {shown} offers, {taken} taken"));
    Ok(Outcome::Answered)
}

async fn answer(conn: &Conn, text: &str) -> Result<Outcome> {
    let mut w = AfcWriter::new();
    w.string(text);
    conn.send(msg::BOARD_TAKE, w.finish()).await?;
    conn.log(&format!("   -> 710 {text}"));
    Ok(Outcome::Answered)
}

/// Why a character may not take an offer, as the client's key.
fn refusal(board: &BoardSettings, character: &CharacterData, offer: &BoardOffer, level: i32, now: i64) -> Option<&'static str> {
    let category = offer.task.category;
    if offer.takers.contains(&character.id) {
        return Some("big_count");
    }
    if level < offer.reputation {
        return Some("no_repa");
    }
    if offer.takers.len() as i32 >= offer.copies || character.board_tasks.iter().filter(|task| task.category == category).count() >= board.max_taken {
        return Some("no_get");
    }
    let last_daily = i64::from(character.quest_values.get(DAILY_TAKEN_AT).copied().unwrap_or(0));
    if category == DAILY && now - last_daily < board.daily_cooldown_minutes * 60 {
        return Some("no_time_period");
    }
    None
}

/// Client 710 long offer.
pub async fn take(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let offer_id = message.reader().int64()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let settings = &conn.world.settings;
    let base = travel::current_base(settings, player);
    let level = reputation::base_faction(&settings.reputation, base).map_or(0, |faction| reputation::level_with(&settings.reputation, &player.data, faction));
    let now = now_seconds();
    let taken = {
        let mut boards = conn.world.boards.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let found = boards
            .iter_mut()
            .filter(|((at, _), offers)| *at == base && now < offers.refresh_at)
            .find_map(|(_, offers)| offers.offers.iter_mut().find(|offer| offer.id == offer_id));
        match found {
            None => Err("no_task"),
            Some(offer) => match refusal(&settings.board, &player.data, offer, level, now) {
                Some(reason) => Err(reason),
                None => {
                    offer.takers.insert(player.data.id);
                    Ok(offer.task.clone())
                }
            },
        }
    };
    let mut task = match taken {
        Ok(task) => task,
        Err(reason) => return answer(conn, reason).await,
    };
    task.id = conn.world.store.next_id().await?;
    if task.category == DAILY {
        player.data.quest_values.insert(DAILY_TAKEN_AT.into(), now as i32);
    }
    player.data.board_tasks.push(task.clone());
    conn.save(player).await?;
    answer(conn, "ok=1").await?;
    send_tasks(conn, msg::TASK_UPDATES, &[&task]).await?;
    send_events(conn, &[(TAKEN, task.id)]).await?;
    conn.log(&format!("   -> 680, 670 task {} taken: tab {}, text {}, species {}, target {}", task.id, task.category, task.task, task.species, task.target));
    Ok(Outcome::Answered)
}

/// Client 705 int tab: «Отказаться» gives up every task of the tab.
pub async fn refuse(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let category = message.reader().int32()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    if category < FIRST_BOARD_CATEGORY {
        // NPC quests are not given up; the list comes again whole.
        send_tasks(conn, msg::TASKS, &player.data.board_tasks.iter().collect::<Vec<_>>()).await?;
        conn.log(&format!("   -> 675 quest category {category} is not given up"));
        return Ok(Outcome::Answered);
    }
    let before = player.data.board_tasks.len();
    player.data.board_tasks.retain(|task| task.category != category);
    conn.save(player).await?;
    send_events(conn, &[(GIVE_UP_TAB, i64::from(category))]).await?;
    conn.log(&format!("   -> 670 gave up {} task(s) of tab {category}", before - player.data.board_tasks.len()));
    Ok(Outcome::Answered)
}

/// What a fish adds to a task, 0 when it does not fit.
fn counts_for(task: &BoardTask, fish: &CaughtFish) -> i32 {
    let fits = (task.species == 0 || task.species == fish.species)
        && fish.grams >= task.min_grams
        && (task.max_grams == 0 || fish.grams <= task.max_grams)
        && (task.task != TROPHIES || fish.size_class == TROPHY_CLASS)
        && (task.task != WEIGHT_ON_BAIT || fish.bait == task.bait);
    match (fits, is_weighed(task.task)) {
        (false, _) => 0,
        (true, true) => fish.grams,
        (true, false) => 1,
    }
}

/// A fish put in the keepnet for tasks. Every task of the base it fits
/// counts it; all but the daily ones take it out of the keepnet.
pub async fn fish_kept(conn: &Conn, player: &mut Player, fish: &CaughtFish) -> Result<()> {
    drop_expired(conn, player).await?;
    let base = travel::current_base(&conn.world.settings, player);
    let mut changed = Vec::new();
    let mut take_fish = false;
    for task in player.data.board_tasks.iter_mut().filter(|task| task.category >= FIRST_BOARD_CATEGORY && task.base == base && task.progress < task.target) {
        let added = counts_for(task, fish);
        if added > 0 {
            task.progress = (task.progress + added).min(task.target);
            take_fish |= task.category != DAILY;
            changed.push(task.clone());
        }
    }
    if changed.is_empty() {
        return Ok(());
    }
    if take_fish {
        player.data.fish.retain(|kept| kept.id != fish.id);
        let mut w = AfcWriter::new();
        w.int64(fish.id);
        conn.send(msg::TASK_FISH, w.finish()).await?;
    }
    conn.save(player).await?;
    send_tasks(conn, msg::TASK_UPDATES, &changed.iter().collect::<Vec<_>>()).await?;
    for task in &changed {
        if task.progress < task.target {
            send_events(conn, &[(PROGRESS, task.id)]).await?;
            conn.log(&format!("   -> 680, 670 task {}: {}/{}", task.id, task.progress, task.target));
            continue;
        }
        send_events(conn, &[(FINISHED, task.id)]).await?;
        player.data.board_tasks.retain(|kept| kept.id != task.id);
        reward(conn, player, task).await?;
        send_events(conn, &[(REMOVE, task.id)]).await?;
    }
    conn.save(player).await
}

/// Gives what a done task's reward line says and says so in the chat.
async fn reward(conn: &Conn, player: &mut Player, task: &BoardTask) -> Result<()> {
    let mut said = Vec::new();
    for (key, value) in task.reward.split(';').filter_map(|part| part.split_once('=')) {
        match key {
            "money" => {
                let money: i64 = value.parse().unwrap_or(0);
                player.data.money += money;
                conn.save(player).await?;
                character::send_money(conn, player, money).await?;
                said.push(format!("{},{:02} кр.", money / 100, money % 100));
            }
            "reals" => {
                let reals: i64 = value.parse().unwrap_or(0);
                player.data.premium += reals;
                conn.save(player).await?;
                character::send_money(conn, player, 0).await?;
                said.push(format!("{reals} эрг."));
            }
            "exp" => {
                let experience: i32 = value.parse().unwrap_or(0);
                character::award_experience(conn, player, experience, 1.0).await?;
                said.push(format!("{experience} опыта"));
            }
            "repfrac" => {
                if let Some((faction, points)) = value.split_once('/').and_then(|(f, p)| Some((f.parse().ok()?, p.parse().ok()?))) {
                    reputation::add(conn, player, faction, points).await?;
                    said.push(format!("{points} репутации"));
                }
            }
            "wpn" => {
                let Some((template, count)) = value.split_once('/').and_then(|(t, c)| Some((t.parse::<i32>().ok()?, c.parse::<i32>().ok()?))) else {
                    continue;
                };
                let Some(item) = conn.world.catalog.get(template) else {
                    continue;
                };
                match inventory::give(conn, player, item, count).await? {
                    Ok(changed) => {
                        inventory::send_updates(conn, player.data.id, &changed).await?;
                        conn.save(player).await?;
                        said.push(format!("предмет x{count}"));
                    }
                    Err(error) => {
                        conn.log(&format!("   task reward item {template} x{count}: {}", error.status()));
                        said.push("предмет не поместился в рюкзак".into());
                    }
                }
            }
            _ => {}
        }
    }
    conn.log(&format!("   task {} done: {}", task.id, task.reward));
    chat::system(conn, &format!("Задание с доски выполнено: {}.", said.join(", "))).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(task: i32) -> BoardTask {
        BoardTask { task, species: 7, target: 5, ..Default::default() }
    }

    fn fish(grams: i32, size_class: i32, bait: i32) -> CaughtFish {
        CaughtFish { id: 1, species: 7, grams, size_class, bait, list: 0, experience: 1, traits: Vec::new() }
    }

    #[test]
    fn a_fish_counts_for_the_tasks_it_fits() {
        assert_eq!(counts_for(&task(COUNT), &fish(300, 0, 1010)), 1);
        assert_eq!(counts_for(&BoardTask { species: 8, ..task(COUNT) }, &fish(300, 0, 1010)), 0);
        assert_eq!(counts_for(&BoardTask { species: 0, ..task(WEIGHT) }, &fish(300, 0, 1010)), 300);
        assert_eq!(counts_for(&BoardTask { min_grams: 500, ..task(COUNT_FROM) }, &fish(300, 0, 1010)), 0);
        assert_eq!(counts_for(&BoardTask { min_grams: 500, ..task(WEIGHT_FROM) }, &fish(600, 1, 1010)), 600);
        assert_eq!(counts_for(&task(TROPHIES), &fish(600, 1, 1010)), 0);
        assert_eq!(counts_for(&task(TROPHIES), &fish(900, 2, 1010)), 1);
        assert_eq!(counts_for(&BoardTask { bait: 1012, ..task(WEIGHT_ON_BAIT) }, &fish(300, 0, 1010)), 0);
        assert_eq!(counts_for(&BoardTask { bait: 1010, ..task(WEIGHT_ON_BAIT) }, &fish(300, 0, 1010)), 300);
    }

    #[test]
    fn colours_follow_their_weights() {
        let weights = [50.0, 25.0, 13.0, 8.0, 4.0];
        assert_eq!(quality_of(&weights, 0.0), 0);
        assert_eq!(quality_of(&weights, 0.6), 1);
        assert_eq!(quality_of(&weights, 0.999), 4);
        assert_eq!(between(0.0, [3, 8]), 3);
        assert_eq!(between(0.999, [3, 8]), 8);
    }

    #[test]
    fn lake_boards_add_their_own_rewards() {
        let board = BoardSettings {
            resource_items: vec![1244],
            base_resource_items: [(4, vec![1314, 1330])].into_iter().collect(),
            ..Default::default()
        };
        assert_eq!(resource_items(&board, 4), vec![1244, 1314, 1330]);
        assert_eq!(resource_items(&board, 6), vec![1244]);
    }

    #[test]
    fn a_taken_offer_cannot_be_taken_again() {
        let board = BoardSettings { max_taken: 3, daily_cooldown_minutes: 40, ..Default::default() };
        let mut character = CharacterData { id: 5, ..Default::default() };
        let mut offer = BoardOffer { id: 1, task: BoardTask { category: INCOME, ..Default::default() }, reputation: 1, copies: 2, takers: Default::default() };
        assert_eq!(refusal(&board, &character, &offer, 0, 10_000), Some("no_repa"));
        assert_eq!(refusal(&board, &character, &offer, 1, 10_000), None);
        offer.takers.insert(5);
        assert_eq!(refusal(&board, &character, &offer, 1, 10_000), Some("big_count"));
        offer.takers = [6, 7].into_iter().collect();
        assert_eq!(refusal(&board, &character, &offer, 1, 10_000), Some("no_get"));
        offer.takers.clear();
        offer.task.category = DAILY;
        character.quest_values.insert(DAILY_TAKEN_AT.into(), 9_000);
        assert_eq!(refusal(&board, &character, &offer, 1, 10_000), Some("no_time_period"));
        assert_eq!(refusal(&board, &character, &offer, 1, 20_000), None);
    }
}
