//! NPC quest chains (`config/quests.json`): Василич, Жека, Игнат, Лиза, the
//! spinners' man at the lake, Кардан. Their texts are the client's dialog files;
//! what each step asks and gives is written in those texts where they say it,
//! and made up where they do not (see the config's comments).
//!
//! The tasks go into the client's task list with the board's records (cases
//! 675, 680, 670), under the quest categories of QTask.xml (below 1000). A
//! chain's place is the quest value `quest_<name>` (steps rewarded) and
//! `quest_<name>_on` (the current step's tasks taken).

use anyhow::Result;
use chrono::Utc;

use super::board::{self, FINISHED, PROGRESS, REMOVE, TAKEN};
use super::dialog::{COUNT_MARK, close, open, option, replace, text};
use super::fishing::TROPHY_CLASS;
use super::{Conn, Outcome, Player, Session, active, character, chat, crafting, inventory, keepnet, reputation, travel};
use crate::afc::{AfcMessage, AfcWriter};
use crate::config::{QuestChain, QuestPage, QuestReward, QuestSettings, QuestStep, QuestTask, TaskKind};
use crate::messages as msg;
use crate::storage::{BoardTask, CaughtFish, CharacterData};

const FISH: i32 = 1;
const ITEM_MADE: i32 = 4;
const ITEM_HELD: i32 = 5;
/// The boat's state page and its marks.
const BOAT_FUEL_MARK: &str = "[cntp]";
const BOAT_BIOFUEL_MARK: &str = "[cntf]";
const BOAT_TOKENS_MARK: &str = "[cntt]";
const BOAT_FUEL: &str = "boat_fuel";
const COST_MARK: &str = "[cost]";

fn step_key(chain: &QuestChain) -> String {
    format!("quest_{}", chain.name)
}

fn taken_key(chain: &QuestChain) -> String {
    format!("quest_{}_on", chain.name)
}

fn value(character: &CharacterData, key: &str) -> i32 {
    character.quest_values.get(key).copied().unwrap_or(0)
}

/// How many steps of the chain are rewarded.
fn step_of(character: &CharacterData, chain: &QuestChain) -> usize {
    value(character, &step_key(chain)).max(0) as usize
}

fn finished(settings: &QuestSettings, character: &CharacterData, name: &str) -> bool {
    settings.chains.iter().find(|chain| chain.name == name).is_some_and(|chain| step_of(character, chain) >= chain.steps.len())
}

fn category_of(chain: &QuestChain, task: &QuestTask) -> i32 {
    if task.category > 0 { task.category } else { chain.category }
}

/// The config line behind a task in the list.
fn spec_of<'a>(settings: &'a QuestSettings, task: &BoardTask) -> Option<&'a QuestTask> {
    settings.chains.iter().flat_map(|chain| {
        chain.steps.iter().flat_map(move |step| step.tasks.iter().map(move |spec| (chain, spec)))
    })
    .find(|(chain, spec)| category_of(chain, spec) == task.category && spec.task == task.task)
    .map(|(_, spec)| spec)
}

fn is_quest(task: &BoardTask) -> bool {
    task.category < board::FIRST_BOARD_CATEGORY
}

/// The record for the client's list. Items go in `species` with kind 4 or 5.
fn record(chain: &QuestChain, spec: &QuestTask, id: i64) -> BoardTask {
    let (kind, species) = match spec.kind {
        TaskKind::Make => (ITEM_MADE, spec.item),
        TaskKind::Have => (ITEM_HELD, spec.item),
        _ => (FISH, spec.species),
    };
    BoardTask {
        id,
        base: spec.base,
        category: category_of(chain, spec),
        task: spec.task,
        kind,
        species,
        bait: spec.bait,
        target: spec.count.max(1),
        min_grams: spec.min_grams,
        max_grams: spec.max_grams,
        ..Default::default()
    }
}

/// What a fish adds to a task.
fn counts_for(spec: &QuestTask, fish: &CaughtFish, base: i32, location: i32, rod_kind: Option<i32>) -> i32 {
    let fits = spec.kind == TaskKind::Catch
        && (spec.species == 0 || spec.species == fish.species)
        && (spec.bait == 0 || spec.bait == fish.bait)
        && fish.grams >= spec.min_grams
        && (spec.max_grams == 0 || fish.grams <= spec.max_grams)
        && (spec.base == 0 || spec.base == base)
        && (spec.location == 0 || spec.location == location)
        && (!spec.trophy || fish.size_class == TROPHY_CLASS)
        && (spec.rod_kind == 0 || rod_kind == Some(spec.rod_kind));
    match (fits, spec.weighed) {
        (false, _) => 0,
        (true, true) => fish.grams,
        (true, false) => 1,
    }
}

/// Sends the changed lines and their events: 5 progress, 4 done.
async fn send_changed(conn: &Conn, changed: &[BoardTask]) -> Result<()> {
    if changed.is_empty() {
        return Ok(());
    }
    board::send_tasks(conn, msg::TASK_UPDATES, &changed.iter().collect::<Vec<_>>()).await?;
    let events: Vec<(i32, i64)> =
        changed.iter().map(|task| (if task.progress >= task.target { FINISHED } else { PROGRESS }, task.id)).collect();
    board::send_events(conn, &events).await
}

/// Adds to every quest line `amount` says, and finishes chains that wait for nothing.
async fn advance_lines(conn: &Conn, player: &mut Player, amount: impl Fn(&QuestTask, &BoardTask) -> i32) -> Result<()> {
    let settings = &conn.world.settings.quests;
    let mut changed = Vec::new();
    for task in player.data.board_tasks.iter_mut().filter(|task| is_quest(task) && task.progress < task.target) {
        let Some(spec) = spec_of(settings, task) else { continue };
        let added = amount(spec, task);
        if added > 0 {
            task.progress = (task.progress + added).min(task.target);
            changed.push(task.clone());
        }
    }
    if changed.is_empty() {
        return Ok(());
    }
    conn.save(player).await?;
    send_changed(conn, &changed).await?;
    for task in &changed {
        conn.log(&format!("   -> 680 quest task {}/{}: {}/{}", task.category, task.task, task.progress, task.target));
    }
    settle(conn, player).await
}

/// A fish put in the keepnet "for tasks".
pub async fn fish_kept(conn: &Conn, player: &mut Player, fish: &CaughtFish, rod_kind: Option<i32>) -> Result<()> {
    let base = travel::current_base(&conn.world.settings, player);
    let location = player.location;
    advance_lines(conn, player, |spec, _| counts_for(spec, fish, base, location, rod_kind)).await
}

/// Things made at a station.
pub async fn item_made(conn: &Conn, player: &mut Player, template: i32, count: i32) -> Result<()> {
    advance_lines(conn, player, |spec, _| if spec.kind == TaskKind::Make && spec.item == template { count } else { 0 }).await
}

/// Biofuel poured into the generator.
pub async fn poured(conn: &Conn, player: &mut Player, count: i32) -> Result<()> {
    advance_lines(conn, player, |spec, _| if spec.kind == TaskKind::Pour { count } else { 0 }).await
}

/// "Получить" lines: done once the items are carried.
async fn check_held(conn: &Conn, player: &mut Player) -> Result<()> {
    let carried: Vec<(i32, i32)> = player
        .data
        .board_tasks
        .iter()
        .filter(|task| is_quest(task) && task.kind == ITEM_HELD)
        .map(|task| (task.species, inventory::pieces_carried(&player.data, task.species)))
        .collect();
    advance_lines(conn, player, |spec, task| {
        if spec.kind != TaskKind::Have {
            return 0;
        }
        let held = carried.iter().find(|(item, _)| *item == spec.item).map_or(0, |(_, n)| *n);
        (held.min(task.target) - task.progress).max(0)
    })
    .await
}

fn lines_of<'a>(character: &'a CharacterData, chain: &'a QuestChain, step: &'a QuestStep) -> impl Iterator<Item = &'a BoardTask> + 'a {
    character
        .board_tasks
        .iter()
        .filter(move |task| step.tasks.iter().any(|spec| category_of(chain, spec) == task.category && spec.task == task.task))
}

fn tasks_done(character: &CharacterData, chain: &QuestChain, step: &QuestStep) -> bool {
    step.tasks.is_empty() || (lines_of(character, chain, step).count() == step.tasks.len() && lines_of(character, chain, step).all(|task| task.progress >= task.target))
}

/// Whether the step may begin: the chain it waits for is finished ("name"),
/// or has so many steps rewarded ("name:N").
fn may_begin(settings: &QuestSettings, character: &CharacterData, step: &QuestStep) -> bool {
    step.requires.as_ref().is_none_or(|wanted| match wanted.split_once(':') {
        Some((name, steps)) => settings
            .chains
            .iter()
            .find(|chain| chain.name == name)
            .is_some_and(|chain| step_of(character, chain) >= steps.parse::<usize>().unwrap_or(usize::MAX)),
        None => finished(settings, character, wanted),
    })
}

/// Puts the step's lines in the list.
async fn take(conn: &Conn, player: &mut Player, chain_index: usize) -> Result<()> {
    let settings = &conn.world.settings.quests;
    let chain = &settings.chains[chain_index];
    let step = &chain.steps[step_of(&player.data, chain)];
    player.data.quest_values.insert(taken_key(chain), 1);
    let mut added = Vec::new();
    for spec in &step.tasks {
        let line = record(chain, spec, conn.world.store.next_id().await?);
        player.data.board_tasks.retain(|task| !(task.category == line.category && task.task == line.task));
        player.data.board_tasks.push(line.clone());
        added.push(line);
    }
    conn.save(player).await?;
    if !added.is_empty() {
        board::send_tasks(conn, msg::TASK_UPDATES, &added.iter().collect::<Vec<_>>()).await?;
        board::send_events(conn, &added.iter().map(|task| (TAKEN, task.id)).collect::<Vec<_>>()).await?;
    }
    conn.log(&format!("   -> 680 quest {} step {}: {} task(s)", chain.name, step_of(&player.data, chain), added.len()));
    Ok(())
}

/// Whether the reward's items fit.
pub(super) fn reward_fits(conn: &Conn, player: &Player, reward: &QuestReward) -> bool {
    let world = &conn.world;
    reward.items.iter().all(|[item, count]| {
        world.catalog.get(*item).is_some_and(|item| inventory::plan_pieces_of(&world.settings.bags, &player.data, item, (*count).max(1), 0).is_ok())
    })
}

/// Gives the reward, drops the step's lines and moves on.
async fn reward(conn: &Conn, player: &mut Player, chain_index: usize) -> Result<()> {
    let world = conn.world.clone();
    let chain = &world.settings.quests.chains[chain_index];
    let index = step_of(&player.data, chain);
    let step = &chain.steps[index];
    let gone: Vec<i64> = lines_of(&player.data, chain, step).map(|task| task.id).collect();
    player.data.board_tasks.retain(|task| !gone.contains(&task.id));
    board::send_events(conn, &gone.iter().map(|id| (REMOVE, *id)).collect::<Vec<_>>()).await?;
    player.data.quest_values.insert(step_key(chain), index as i32 + 1);
    player.data.quest_values.insert(taken_key(chain), 0);
    conn.save(player).await?;

    let said = give(conn, player, &step.reward).await?;
    if !said.is_empty() {
        chat::system(conn, &format!("Задание выполнено: {}.", said.join(", "))).await?;
    }
    conn.log(&format!("   quest {} step {} rewarded: {}", chain.name, index, said.join(", ")));
    Ok(())
}

/// Gives a reward and says what it was, piece by piece.
pub(super) async fn give(conn: &Conn, player: &mut Player, reward: &QuestReward) -> Result<Vec<String>> {
    let world = conn.world.clone();
    for flag in &reward.flags {
        player.data.quest_values.insert(flag.clone(), 1);
    }
    let mut said = Vec::new();
    for [template, count] in &reward.items {
        let Some(item) = world.catalog.get(*template) else { continue };
        match inventory::give(conn, player, item, (*count).max(1)).await? {
            Ok(changed) => {
                inventory::send_updates(conn, player.data.id, &changed).await?;
                said.push(if *count > 1 { format!("{} x{count}", item.name) } else { item.name.clone() });
            }
            Err(error) => conn.log(&format!("   quest reward {template}: {}", error.status())),
        }
    }
    for recipe in &reward.recipes {
        crafting::add_uses(&mut player.data.known_recipes, *recipe, -1);
    }
    if !reward.recipes.is_empty() {
        said.push(format!("рецептов: {}", reward.recipes.len()));
    }
    for [faction, points] in &reward.reputation {
        reputation::add(conn, player, *faction as i32, *points).await?;
        said.push(format!("{points} репутации"));
    }
    if reward.money > 0 {
        player.data.money += reward.money;
        character::send_money(conn, player, reward.money).await?;
    }
    if reward.premium > 0 {
        player.data.premium += reward.premium;
        character::send_money(conn, player, 0).await?;
        said.push(format!("{} эргов", reward.premium));
    }
    if reward.experience > 0 {
        character::award_experience(conn, player, reward.experience, 1.0).await?;
        said.push(format!("{} опыта", reward.experience));
    }
    conn.save(player).await?;
    Ok(said)
}

/// Takes the steps that need no word and rewards the ones done without a
/// page, in every chain, until nothing moves.
async fn settle(conn: &Conn, player: &mut Player) -> Result<()> {
    let world = conn.world.clone();
    let settings = &world.settings.quests;
    loop {
        let mut moved = false;
        for (index, chain) in settings.chains.iter().enumerate() {
            let Some(step) = chain.steps.get(step_of(&player.data, chain)) else { continue };
            if !may_begin(settings, &player.data, step) {
                continue;
            }
            let taken = value(&player.data, &taken_key(chain)) > 0;
            if !taken && step.offer.is_none() {
                take(conn, player, index).await?;
                moved = true;
            } else if taken && step.done.is_none() && tasks_done(&player.data, chain, step) {
                reward(conn, player, index).await?;
                moved = true;
            }
        }
        if !moved {
            return Ok(());
        }
    }
}

async fn page(conn: &Conn, player: &mut Player, file: i32, page: QuestPage) -> Result<()> {
    text(conn, file, page.node).await?;
    if page.answer > 0 {
        option(conn, file, page.node, page.answer, 0).await?;
    }
    if page.decline > 0 {
        option(conn, file, page.node, page.decline, 0).await?;
    }
    open(conn, player, file).await
}

/// Fish with the trait in the keepnets.
fn delivered(character: &CharacterData, trait_id: i32) -> Vec<i64> {
    character.fish.iter().filter(|fish| keepnet::in_keepnet(fish) && fish.traits.contains(&trait_id)).map(|fish| fish.id).collect()
}

/// What the chain's NPC says now.
async fn show_chain(conn: &Conn, player: &mut Player, chain_index: usize) -> Result<()> {
    let world = conn.world.clone();
    let settings = &world.settings.quests;
    let chain = &settings.chains[chain_index];
    let file = chain.dialog;
    let Some(step) = chain.steps.get(step_of(&player.data, chain)).filter(|step| may_begin(settings, &player.data, step)) else {
        return page(conn, player, file, chain.idle).await;
    };
    let taken = value(&player.data, &taken_key(chain)) > 0;
    if !taken {
        return match step.offer {
            Some(offer) => page(conn, player, file, offer).await,
            None => page(conn, player, file, chain.idle).await,
        };
    }
    if !tasks_done(&player.data, chain, step) {
        return page(conn, player, file, step.waiting.unwrap_or(chain.idle)).await;
    }
    let Some(done) = step.done else {
        return page(conn, player, file, chain.idle).await;
    };
    if step.deliver_trait > 0 {
        text(conn, file, done.node).await?;
        for fish in delivered(&player.data, step.deliver_trait) {
            option(conn, file, done.node, done.answer, fish).await?;
        }
        if done.decline > 0 {
            option(conn, file, done.node, done.decline, 0).await?;
        }
        return open(conn, player, file).await;
    }
    page(conn, player, file, done).await
}

async fn show_menu(conn: &Conn, player: &mut Player, chain_index: usize) -> Result<()> {
    let chain = &conn.world.settings.quests.chains[chain_index];
    let Some(menu) = chain.menu.as_ref() else {
        return show_chain(conn, player, chain_index).await;
    };
    let file = chain.dialog;
    text(conn, file, menu.node).await?;
    option(conn, file, menu.node, menu.quests, 0).await?;
    if let Some(tokens) = &menu.tokens {
        option(conn, file, menu.node, tokens.answer, 0).await?;
    }
    option(conn, file, menu.node, menu.leave, 0).await?;
    open(conn, player, file).await
}

async fn show_tokens(conn: &Conn, player: &mut Player, chain_index: usize) -> Result<()> {
    let chain = &conn.world.settings.quests.chains[chain_index];
    let (Some(menu), file) = (chain.menu.as_ref(), chain.dialog) else { return Ok(()) };
    let Some(tokens) = &menu.tokens else { return Ok(()) };
    let carried = inventory::pieces_carried(&player.data, tokens.item);
    text(conn, file, tokens.node).await?;
    replace(conn, COUNT_MARK, &carried.to_string()).await?;
    if carried > 0 {
        option(conn, file, tokens.node, tokens.one, 0).await?;
        option(conn, file, tokens.node, tokens.all, 0).await?;
        replace(conn, COST_MARK, &(i64::from(carried) * tokens.points).to_string()).await?;
    }
    option(conn, file, tokens.node, menu.leave, 0).await?;
    open(conn, player, file).await
}

fn chain_for(settings: &QuestSettings, file: i32) -> Option<usize> {
    settings.chains.iter().position(|chain| chain.dialog == file)
}

/// Client 60 for an NPC with a quest dialog. `None` leaves it to the others.
pub async fn talk(session: &mut Session, message: &AfcMessage) -> Result<Option<Outcome>> {
    let npc = message.reader().int64()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let world = conn.world.clone();
    let settings = &world.settings.quests;
    let base = travel::current_base(&world.settings, player);
    if let Some(boat) = settings.boat.as_ref().filter(|boat| boat.base == base && i64::from(boat.npc) == npc) {
        text(conn, boat.dialog, 1).await?;
        option(conn, boat.dialog, 1, 2, 0).await?;
        option(conn, boat.dialog, 1, 5, 0).await?;
        open(conn, player, boat.dialog).await?;
        return Ok(Some(Outcome::Answered));
    }
    let Some(file) = settings.npcs.iter().find(|one| one.base == base && i64::from(one.npc) == npc).map(|one| one.dialog) else {
        return Ok(None);
    };
    // Talking is itself the task of "Поговорить с ...".
    advance_lines(conn, player, |spec, _| i32::from(spec.kind == TaskKind::Talk && spec.dialog == file)).await?;
    check_held(conn, player).await?;
    settle(conn, player).await?;
    match chain_for(settings, file) {
        Some(index) => show_menu(conn, player, index).await?,
        None => conn.log(&format!("   NPC {npc} at base {base}: dialog {file} has no chain")),
    }
    conn.log(&format!("   -> 70/72/74 NPC {npc} at base {base}, dialog {file}"));
    Ok(Some(Outcome::Answered))
}

/// Whether the open dialog is a quest NPC's or the boat's.
pub fn is_open(session: &Session) -> bool {
    let settings = &session.conn.world.settings.quests;
    let Some(file) = session.player.as_ref().and_then(|player| player.dialog) else {
        return false;
    };
    settings.npcs.iter().any(|npc| npc.dialog == file) || settings.boat.as_ref().is_some_and(|boat| boat.dialog == file)
}

/// Client 65 in a quest dialog.
pub async fn answer(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let mut r = message.reader();
    let answer = r.int32()?;
    let param = r.int64()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let world = conn.world.clone();
    let settings = &world.settings.quests;
    let Some(file) = player.dialog else {
        return Ok(Outcome::Unanswered);
    };
    if settings.boat.as_ref().is_some_and(|boat| boat.dialog == file) {
        boat_answer(conn, player, answer).await?;
        return Ok(Outcome::Answered);
    }
    let Some(index) = chain_for(settings, file) else {
        close(conn, player, file).await?;
        return Ok(Outcome::Answered);
    };
    let chain = &settings.chains[index];
    if let Some(menu) = &chain.menu {
        if answer == menu.quests {
            show_chain(conn, player, index).await?;
            return Ok(Outcome::Answered);
        }
        if let Some(tokens) = menu.tokens.as_ref().filter(|tokens| [tokens.answer, tokens.one, tokens.all].contains(&answer)) {
            if answer != tokens.answer {
                let carried = inventory::pieces_carried(&player.data, tokens.item);
                let amount = if answer == tokens.one { carried.min(1) } else { carried };
                if let Some((smaller, gone)) = (amount > 0).then(|| inventory::take_carried(&mut player.data, tokens.item, amount)).flatten() {
                    for id in gone {
                        inventory::send_removed(conn, id).await?;
                    }
                    inventory::send_updates(conn, player.data.id, &smaller).await?;
                    conn.save(player).await?;
                    reputation::add(conn, player, tokens.faction, tokens.points * i64::from(amount)).await?;
                    conn.log(&format!("   quest NPC took {amount} tokens"));
                }
            }
            show_tokens(conn, player, index).await?;
            return Ok(Outcome::Answered);
        }
    }

    let step_index = step_of(&player.data, chain);
    let step = chain.steps.get(step_index).filter(|step| may_begin(settings, &player.data, step));
    let taken = value(&player.data, &taken_key(chain)) > 0;
    match step {
        Some(step) if !taken && step.offer.is_some_and(|offer| offer.answer == answer) => {
            take(conn, player, index).await?;
            if step.tasks.is_empty() {
                show_chain(conn, player, index).await?;
            } else {
                close(conn, player, file).await?;
            }
        }
        Some(step) if taken && step.done.is_some_and(|done| done.answer == answer) && tasks_done(&player.data, chain, step) => {
            if step.deliver_trait > 0 {
                let Some(fish) = player.data.fish.iter().find(|fish| fish.id == param && fish.traits.contains(&step.deliver_trait)).map(|fish| fish.id) else {
                    close(conn, player, file).await?;
                    return Ok(Outcome::Answered);
                };
                player.data.fish.retain(|kept| kept.id != fish);
                let mut w = AfcWriter::new();
                w.int64(fish);
                conn.send(msg::FISH_REMOVE, w.finish()).await?;
            }
            if !reward_fits(conn, player, &step.reward) {
                match chain.full {
                    Some(full) => page(conn, player, file, full).await?,
                    None => {
                        chat::system(conn, "Награда не помещается в рюкзак.").await?;
                        close(conn, player, file).await?;
                    }
                }
                return Ok(Outcome::Answered);
            }
            reward(conn, player, index).await?;
            settle(conn, player).await?;
            // The next step's question or reward page comes at once; a step
            // already under way leaves the window closed.
            let next = chain.steps.get(step_of(&player.data, chain)).filter(|step| may_begin(settings, &player.data, step));
            let talks_on = next.is_some_and(|next| {
                let taken = value(&player.data, &taken_key(chain)) > 0;
                (!taken && next.offer.is_some()) || (taken && next.done.is_some() && tasks_done(&player.data, chain, next))
            });
            if talks_on {
                show_chain(conn, player, index).await?;
            } else {
                close(conn, player, file).await?;
            }
        }
        _ => close(conn, player, file).await?,
    }
    Ok(Outcome::Answered)
}

/// Whether the map may not take the character to this location (the boat's).
pub fn closed_on_map(settings: &QuestSettings, base: i32, location: i32) -> bool {
    settings.boat.as_ref().is_some_and(|boat| boat.base == base && boat.location == location)
}

async fn boat_state(conn: &Conn, player: &mut Player) -> Result<()> {
    let Some(boat) = conn.world.settings.quests.boat.as_ref() else { return Ok(()) };
    let file = boat.dialog;
    text(conn, file, 2).await?;
    replace(conn, BOAT_FUEL_MARK, &value(&player.data, BOAT_FUEL).to_string()).await?;
    replace(conn, BOAT_BIOFUEL_MARK, &inventory::pieces_carried(&player.data, boat.fuel_item).to_string()).await?;
    replace(conn, BOAT_TOKENS_MARK, &inventory::pieces_carried(&player.data, boat.token).to_string()).await?;
    for answer in [3, 4, 5] {
        option(conn, file, 2, answer, 0).await?;
    }
    open(conn, player, file).await
}

/// 11.lng: 1 go (shows the state), 2 state, 3 fuel up, 4 go for a token, 5 leave.
async fn boat_answer(conn: &Conn, player: &mut Player, answer: i32) -> Result<()> {
    let world = conn.world.clone();
    let Some(boat) = world.settings.quests.boat.as_ref() else { return Ok(()) };
    let fuel = value(&player.data, BOAT_FUEL);
    let pay = |player: &mut Player, item: i32| inventory::take_carried(&mut player.data, item, 1);
    match answer {
        1 | 2 => boat_state(conn, player).await,
        3 => {
            if fuel + boat.fuel_percent <= 100
                && let Some((smaller, gone)) = pay(player, boat.fuel_item)
            {
                for id in gone {
                    inventory::send_removed(conn, id).await?;
                }
                inventory::send_updates(conn, player.data.id, &smaller).await?;
                player.data.quest_values.insert(BOAT_FUEL.into(), fuel + boat.fuel_percent);
                conn.save(player).await?;
            }
            boat_state(conn, player).await
        }
        4 => {
            if fuel < boat.trip_percent {
                chat::system(conn, "Катер не заправлен.").await?;
                return boat_state(conn, player).await;
            }
            let Some((smaller, gone)) = pay(player, boat.token) else {
                chat::system(conn, "Нужен жетон кладовщика.").await?;
                return boat_state(conn, player).await;
            };
            for id in gone {
                inventory::send_removed(conn, id).await?;
            }
            inventory::send_updates(conn, player.data.id, &smaller).await?;
            player.data.quest_values.insert(BOAT_FUEL.into(), fuel - boat.trip_percent);
            conn.save(player).await?;
            close(conn, player, boat.dialog).await?;
            conn.log(&format!("   boat trip at {}", Utc::now().timestamp()));
            travel::teleport(conn, player, boat.base, boat.location).await
        }
        _ => close(conn, player, boat.dialog).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fish(species: i32, grams: i32, size_class: i32, bait: i32) -> CaughtFish {
        CaughtFish { id: 1, species, grams, size_class, bait, list: 0, experience: 1, traits: Vec::new() }
    }

    #[test]
    fn a_fish_counts_where_and_how_the_task_says() {
        let on_bait = QuestTask { task: 0, count: 10, bait: 1018, location: 4, ..Default::default() };
        assert_eq!(counts_for(&on_bait, &fish(77, 300, 0, 1018), 1, 4, None), 1);
        assert_eq!(counts_for(&on_bait, &fish(77, 300, 0, 1018), 1, 3, None), 0);
        assert_eq!(counts_for(&on_bait, &fish(77, 300, 0, 1197), 1, 4, None), 0);
        let trophy = QuestTask { species: 95, trophy: true, rod_kind: 3, ..Default::default() };
        assert_eq!(counts_for(&trophy, &fish(95, 5000, TROPHY_CLASS, 1), 4, 42, Some(3)), 1);
        assert_eq!(counts_for(&trophy, &fish(95, 5000, TROPHY_CLASS, 1), 4, 42, Some(1)), 0);
        assert_eq!(counts_for(&trophy, &fish(95, 3000, 1, 1), 4, 42, Some(3)), 0);
        let weight = QuestTask { species: 37, weighed: true, ..Default::default() };
        assert_eq!(counts_for(&weight, &fish(37, 800, 0, 1), 6, 61, None), 800);
        let made = QuestTask { kind: TaskKind::Make, item: 1167, ..Default::default() };
        assert_eq!(counts_for(&made, &fish(37, 800, 0, 1), 6, 61, None), 0);
    }

    #[test]
    fn the_config_has_one_line_per_task_text() {
        let text = std::fs::read_to_string("config/quests.json").expect("config/quests.json");
        let settings: QuestSettings = json_comments_parse(&text);
        let mut seen = std::collections::HashSet::new();
        for chain in &settings.chains {
            for step in &chain.steps {
                for spec in &step.tasks {
                    assert!(seen.insert((category_of(chain, spec), spec.task)), "{} {}/{}", chain.name, category_of(chain, spec), spec.task);
                }
                if let Some(wanted) = &step.requires {
                    let name = wanted.split(':').next().unwrap_or_default();
                    assert!(settings.chains.iter().any(|chain| chain.name == name), "{name}");
                }
            }
        }
    }

    fn json_comments_parse(text: &str) -> QuestSettings {
        use std::io::Read;
        let mut clean = String::new();
        json_comments::StripComments::new(text.as_bytes()).read_to_string(&mut clean).expect("comments");
        serde_json::from_str(&clean).expect("quests.json")
    }
}
