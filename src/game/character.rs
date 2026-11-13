//! Logging in, the character screen, entering the world, levels.

use anyhow::{Context, Result};
use chrono::Local;

use super::{Account, Conn, Outcome, Player, Session, cards, inventory, keepnet, panels, perks, skills, travel, vitals};
use crate::afc::{AfcMessage, AfcWriter};
use crate::messages as msg;
use crate::storage::{CharacterData, ItemEntry};

/// The client only lets a player enter the world when the login reply carries a
/// non-empty token, so a client without a Steam id still needs one.
const ANONYMOUS_KEY: &str = "local";

/// The first entry is the sex, 1 for a man; the rest pick the first palette entry.
const APPEARANCE: [u8; 17] = [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];


/// Client: long marker, string name, string password, string Steam id, string
/// device id, string persona, bool Steam login, int pin.
///
/// The Steam id is what the client says it is: nothing proves it yet.
pub async fn login(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let mut r = message.reader();
    r.int64()?;
    r.string()?;
    r.string()?;
    let steam_id = r.string()?;
    r.string()?;
    let persona = r.string()?;

    let key = if steam_id.is_empty() { ANONYMOUS_KEY } else { &steam_id };
    let id = session.conn.world.store.account_for(key).await?;
    session.account = Some(Account { id, persona });

    let mut w = AfcWriter::new();
    w.string("Ok").int32(id as i32).string(key).int32(0);
    session.conn.send(msg::LOGIN, w.finish()).await?;
    session.conn.log(&format!("   -> 90 login ok, account {id}"));
    Ok(Outcome::Answered)
}

/// Case 105: long credits, long premium, then the server's date and time.
pub async fn currency(session: &mut Session) -> Result<Outcome> {
    let character = &session.conn.world.settings.character;
    let (money, premium) = match &session.player {
        Some(player) => (player.data.money, player.data.premium),
        None => (character.starting_money, character.premium_money),
    };

    let mut w = AfcWriter::new();
    w.int64(money).int64(premium).date_time(Local::now().naive_local());
    session.conn.send(msg::CURRENCY, w.finish()).await?;
    session.conn.log(&format!("   -> 105 currency {money}/{premium}"));
    Ok(Outcome::Answered)
}

/// Case 110, then 111: entering the world is the server's call and nothing else
/// asks for it. There is no character creator yet, so the first visit makes one.
pub async fn list(session: &mut Session) -> Result<Outcome> {
    let (account, persona) = {
        let account = session.account.as_ref().context("character list before login")?;
        (account.id, account.persona.clone())
    };

    let world = session.conn.world.clone();
    let mut characters = world.store.list_characters(account).await?;
    if characters.is_empty() {
        create_first_character(&session.conn, account, &persona).await?;
        characters = world.store.list_characters(account).await?;
    }

    let start_base = world.settings.character.start_base;
    let mut w = AfcWriter::new();
    for character in &characters {
        w.int64(character.id).string(&character.name).int32(character.level);
        w.int32(0); // short costume id
        w.int32(start_base).int32(0); // base, location: 0 is standing on the base
        w.int64(0);
        w.int32(APPEARANCE.len() as i32).bytes(&APPEARANCE);
    }
    session.conn.send(msg::CHARACTER_LIST, w.finish()).await?;
    session.conn.send(msg::ENTER_WORLD, Vec::new()).await?;
    session.conn.log(&format!("   -> 110 {} character(s), 111 enter world", characters.len()));
    Ok(Outcome::Answered)
}

async fn create_first_character(conn: &Conn, account: i64, persona: &str) -> Result<()> {
    let world = &conn.world;
    let settings = &world.settings.character;

    let mut items = Vec::with_capacity(world.settings.starter_kit.len());
    for kit in &world.settings.starter_kit {
        let item = world.catalog.get(kit.item).context("starter kit item vanished")?;
        items.push(ItemEntry {
            id: world.store.next_id().await?,
            bag: kit.bag,
            cell: kit.cell,
            count: kit.count,
            durability: item.durability,
            template: item.id,
            param: 0,
        });
    }

    let name = if persona.is_empty() { &settings.name } else { persona };
    let character = CharacterData {
        account,
        name: name.to_owned(),
        level: settings.level,
        money: settings.starting_money,
        premium: settings.premium_money,
        items,
        ..Default::default()
    };
    let id = world.store.create_character(&character).await?;
    conn.log(&format!("   created character {id} \"{name}\""));
    Ok(())
}

/// Client: long character id, from the "В игру" button. The replies are ordered:
/// 150 indexes into the catalogue 145 builds, and the Base scene needs a current
/// base before it starts running.
pub async fn go_to_game(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let account = session.account.as_ref().context("entering the game before login")?.id;
    let character_id = message.reader().int64()?;

    let world = session.conn.world.clone();
    let Some(data) = world.store.load_character(character_id).await? else {
        session.conn.log(&format!("   no character {character_id}"));
        return Ok(Outcome::Unanswered);
    };
    if data.account != account {
        session.conn.log(&format!("   character {character_id} belongs to another account"));
        return Ok(Outcome::Unanswered);
    }

    session.leave();
    if world.claim(character_id, session.conn.number, session.kick.clone()) {
        session.conn.log("   took the character over from another connection");
    }
    let player = session.player.insert(Player::new(data));
    let (progression, level) = (&world.settings.progression, player.data.level);
    player.data.vitals.get_or_insert_with(|| vitals::full(progression, level));
    let kind_of = |template| world.catalog.get(template).map_or(0, |item| item.kind);
    let moved = inventory::move_old_backpack(&world.settings.bags, &mut player.data, kind_of);
    let info = |template| {
        world.catalog.get(template).map_or(inventory::StackInfo { kind: 0, stacks: false, max_count: 1 }, |item| {
            inventory::StackInfo { kind: item.kind, stacks: item.stacks, max_count: item.max_count }
        })
    };
    let separated = inventory::separate_shared_cells(&world.settings.bags, &mut player.data, info);
    let counted = cards::count_old_records(&mut player.data);
    if moved > 0 || separated > 0 || counted > 0 {
        session.conn.save(player).await?;
        session.conn.log(&format!(
            "   moved {moved} items from the old backpack onto pages, {separated} out of shared cells; {counted} old records counted as caught"
        ));
    }

    let start_base = world.settings.character.start_base;
    travel::send_base_list(&session.conn).await?;
    travel::send_base_details(&session.conn, start_base).await?;
    send_details(&session.conn, player, msg::CHARACTER_DETAILS).await?;
    if player.data.vip {
        keepnet::send_vip_icon(&session.conn).await?;
    }
    inventory::send_all(&session.conn, player).await?;
    perks::send(&session.conn, player).await?;
    skills::send_study(&session.conn, player).await?;
    keepnet::send_all(&session.conn, player).await?;
    session.conn.send(msg::LOAD_BASE, Vec::new()).await?;
    session.conn.log(&format!("   -> 145, 150, 100, 180, 174, 175, 151 entered as {character_id}"));
    Ok(Outcome::Answered)
}

/// Fields of `NEBJANKNJOG.BJJMPDCPDMN`, after the character id.
async fn send_details(conn: &Conn, player: &Player, message_id: i32) -> Result<()> {
    let settings = &conn.world.settings;
    let character = &player.data;
    let progression = &settings.progression;

    let mut w = AfcWriter::new();
    w.int64(character.id);
    w.string(&character.name);
    w.int32(1).int32(character.level).int32(0); // sex, level, unknown
    w.int64(character.experience).int64(progression.experience_needed(character.level, perks::effects(conn, player).level_cost));
    w.int64(character.money).int64(character.premium);
    w.string("");
    w.int32(settings.character.start_base).int32(0); // base, location
    w.int32(0).int64(0);

    // An int the client reads and never uses, then each scale as float value,
    // int maximum. Writing the maximum first shifted every maximum one scale
    // along and left water with a maximum of 0: an empty bar that never filled.
    w.int32(0);
    let current = character.vitals.clone().unwrap_or_else(|| vitals::full(progression, character.level));
    let maxima = vitals::for_client(vitals::limits(settings, &conn.world.catalog, character));
    for (max, value) in maxima.iter().zip(vitals::for_client(current.values)) {
        w.single(value).int32(*max as i32);
    }

    let bonuses = perks::levels(settings, &conn.world.catalog, character);
    for value in perks::attributes(&bonuses) {
        w.int32(value);
    }
    w.int32(0).int32(0).int32(0);
    w.single(0.0).single(0.0).single(0.0);
    let bags = &settings.bags;
    w.int32(bags.pages);
    for bag in 1..=bags.pages {
        w.int32(panels::cells_in(bags, character, bag));
    }
    w.int32(APPEARANCE.len() as i32).bytes(&APPEARANCE);
    conn.send(message_id, w.finish()).await
}

pub async fn send_update(conn: &Conn, player: &Player) -> Result<()> {
    send_details(conn, player, msg::CHARACTER_UPDATE).await
}

/// Case 155: long gained, long current, float skill share, long base, float
/// multiplier. Each new level is case 103: int level, long experience to the
/// next, two unread ints, then life, stamina, radiation, food and water gains.
pub async fn award_experience(conn: &Conn, player: &mut Player, experience: i32, multiplier: i32) -> Result<()> {
    let gained = i64::from(experience) * i64::from(multiplier);
    let cost = perks::effects(conn, player).level_cost;
    let reached = add_experience(&conn.world.settings.progression, &mut player.data, gained, cost);
    conn.save(player).await?;
    skills::give(conn, player, skills::share(&conn.world.settings.skills, gained)).await?;

    let progression = &conn.world.settings.progression;
    let mut w = AfcWriter::new();
    w.int64(gained)
        .int64(player.data.experience)
        .single(0.0)
        .int64(i64::from(experience))
        .single(multiplier as f32);
    conn.send(msg::EXPERIENCE_GAINED, w.finish()).await?;
    conn.log(&format!(
        "   -> 155 +{gained} exp, now {}/{} at level {}",
        player.data.experience,
        progression.experience_needed(player.data.level, cost),
        player.data.level
    ));

    for level in &reached {
        let mut w = AfcWriter::new();
        w.int32(*level).int64(progression.experience_needed(*level, cost)).int32(0).int32(0);
        for per_level in &progression.vitals_per_level {
            w.int32(*per_level);
        }
        conn.send(msg::LEVEL_UP, w.finish()).await?;
        conn.log(&format!("   -> 103 level {level}"));
    }

    if !reached.is_empty() {
        send_details(conn, player, msg::CHARACTER_UPDATE).await?;
    }
    Ok(())
}

/// Returns each level reached, in order.
fn add_experience(
    progression: &crate::config::ProgressionSettings,
    character: &mut CharacterData,
    amount: i64,
    cost: f64,
) -> Vec<i32> {
    let mut reached = Vec::new();
    character.experience += amount;
    while character.level < progression.max_level
        && character.experience >= progression.experience_needed(character.level, cost)
    {
        character.experience -= progression.experience_needed(character.level, cost);
        character.level += 1;
        reached.push(character.level);
    }
    reached
}

/// Case 21: long shown, long shown, long premium, long credits.
pub async fn send_money(conn: &Conn, player: &Player, change: i64) -> Result<()> {
    let mut w = AfcWriter::new();
    w.int64(0).int64(change).int64(player.data.premium).int64(player.data.money);
    conn.send(msg::MONEY_CHANGED, w.finish()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProgressionSettings;

    fn progression() -> ProgressionSettings {
        ProgressionSettings {
            max_level: 3,
            level_base: 10.0,
            level_exponent: 1.0,
            experience_per_fish: 1.0,
            experience_per_kg: 1.0,
            large_experience_factor: 1.0,
            trophy_experience_factor: 1.0,
            record_experience_factor: 3,
            base_vital: 100,
            vitals_per_level: vec![1; 5],
        }
    }

    #[test]
    fn experience_carries_over_levels() {
        let mut character = CharacterData { level: 1, ..Default::default() };
        // Level 1 needs 10, level 2 needs 20.
        assert_eq!(add_experience(&progression(), &mut character, 35, 1.0), vec![2, 3]);
        assert_eq!(character.level, 3);
        assert_eq!(character.experience, 5);
    }

    #[test]
    fn level_stops_at_the_cap() {
        let mut character = CharacterData { level: 3, ..Default::default() };
        assert!(add_experience(&progression(), &mut character, 1000, 1.0).is_empty());
        assert_eq!(character.level, 3);
    }
}
