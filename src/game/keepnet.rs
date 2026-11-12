//! Fish the player kept: the keepnet window and the fish market. Prices were
//! decided on the shut-down server, so they come from `settings.json`.

use std::collections::BTreeMap;

use anyhow::Result;
use chrono::{Local, TimeDelta};

use super::{Conn, Outcome, Player, Session, active, inventory, travel};
use crate::afc::{AfcMessage, AfcWriter};
use crate::config::{FishMarketSettings, Species};
use crate::messages as msg;
use crate::storage::CaughtFish;

pub const IN_USE_LIST: i32 = 0;
const SPARE_LIST: i32 = 1;

/// In one of the two keepnets rather than on ice in a fridge.
pub fn in_keepnet(fish: &CaughtFish) -> bool {
    fish.list == IN_USE_LIST || fish.list == SPARE_LIST
}
const CLOTHING_BAG: i32 = 60;
const IN_USE_CELL: i32 = 6;
const SPARE_CELL: i32 = 8;
const TROPHY_CLASS: i32 = 2;
const LARGE_CLASS: i32 = 1;

/// The market's demand bar, kept full: there is no market to move it.
const FULL_DEMAND: f32 = 1.0;

/// The record `OIJEGJLCFCF.LIMJEKJAILL` reads, shared by cases 175 and 1130.
/// The third flag marks the species as a mutant; recipes check it.
pub fn write_fish(w: &mut AfcWriter, fish: &CaughtFish, mutants: &[i32]) {
    w.int64(fish.id).int32(fish.species).int32(fish.grams);
    w.int32(fish.size_class).int32(fish.bait).int32(fish.list);
    w.boolean(false).boolean(fish.size_class == TROPHY_CLASS).boolean(mutants.contains(&fish.species));
    w.string("").string("");
}

/// Case 174 empties the client's lists; case 175 appends records until the end.
pub async fn send_all(conn: &Conn, player: &Player) -> Result<()> {
    conn.send(msg::FISH_LIST_CLEAR, Vec::new()).await?;
    let mut w = AfcWriter::new();
    for fish in player.data.fish.iter().filter(|fish| in_keepnet(fish)) {
        write_fish(&mut w, fish, &conn.world.settings.fishing.mutant_species);
    }
    conn.send(msg::FISH_LIST, w.finish()).await
}

/// Client: long fish id, as 176 or 177. It has already moved the fish itself.
pub async fn move_between_lists(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let id = message.reader().int64()?;
    let list = if message.id == msg::FISH_TO_SPARE { SPARE_LIST } else { IN_USE_LIST };
    let player = active(&mut session.player)?;
    let conn = &session.conn;

    let Some(fish) = player.data.fish.iter_mut().find(|fish| fish.id == id) else {
        conn.log(&format!("   keepnet: unknown fish {id}"));
        return Ok(Outcome::Unanswered);
    };
    fish.list = list;
    conn.save(player).await?;
    conn.log(&format!("   fish {id} moved to keepnet list {list}"));
    Ok(Outcome::Answered)
}

/// Client: long fish id. It has already removed the fish from its own list.
pub async fn release(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let id = message.reader().int64()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;

    if player.landed.remove(&id).is_some() {
        conn.log(&format!("   fish {id} released on landing"));
        return Ok(Outcome::Answered);
    }

    let before = player.data.fish.len();
    player.data.fish.retain(|fish| fish.id != id);
    if player.data.fish.len() == before {
        conn.log(&format!("   release: unknown fish {id}"));
        return Ok(Outcome::Unanswered);
    }
    conn.save(player).await?;
    conn.log(&format!("   fish {id} released from the keepnet"));
    Ok(Outcome::Answered)
}

/// Client: int 0, from the keepnet window's swap button, which needs a keepnet
/// in both clothing cells. The client changes nothing itself, so send it all back.
pub async fn swap_keepnets(session: &mut Session) -> Result<Outcome> {
    let player = active(&mut session.player)?;
    let conn = &session.conn;

    let find = |cell: i32| {
        player.data.items.iter().find(|item| item.bag == CLOTHING_BAG && item.cell == cell).map(|item| item.id)
    };
    let (Some(in_use), Some(spare)) = (find(IN_USE_CELL), find(SPARE_CELL)) else {
        conn.log("   keepnet swap: needs keepnets in both cells");
        return Ok(Outcome::Unanswered);
    };

    if let Some(changed) = inventory::swap(&mut player.data, in_use, spare) {
        inventory::send_updates(conn, player.data.id, &changed).await?;
    }
    for fish in player.data.fish.iter_mut().filter(|fish| in_keepnet(fish)) {
        fish.list = if fish.list == IN_USE_LIST { SPARE_LIST } else { IN_USE_LIST };
    }
    conn.save(player).await?;
    send_all(conn, player).await?;
    conn.log("   -> 181, 174, 175 keepnets swapped");
    Ok(Outcome::Answered)
}

/// Ends the same way as a bought patent would, though `true` below keeps it on.
const VIP_DAYS: i64 = 365;
/// The client's flag for a bought VIP patent, which alone enables the market's
/// "prices" and "sell all" buttons.
const VIP_ICON: i32 = 20;

/// Case 260: until the end int icon, 3 x double for its tooltip, end date, bool
/// never expires. Has to follow case 100, which clears the icons.
pub async fn send_vip_icon(conn: &Conn) -> Result<()> {
    let mut w = AfcWriter::new();
    w.int32(VIP_ICON).double(0.0).double(0.0).double(0.0);
    w.date_time(Local::now().naive_local() + TimeDelta::days(VIP_DAYS));
    w.boolean(true);
    conn.send(msg::ACTIVE_ICONS, w.finish()).await
}

/// The rarer the species, the dearer it sells.
fn price_per_kg(market: &FishMarketSettings, largest_share: f64, species: &Species, size_class: i32) -> i32 {
    let rarity = (largest_share / species.share).sqrt();
    let size = match size_class {
        TROPHY_CLASS => market.trophy_price_factor,
        LARGE_CLASS => market.large_price_factor,
        _ => 1.0,
    };
    (market.base_price_per_kg * rarity * size).round() as i32
}

fn lot_total(grams: i32, price_per_kg: i32) -> i64 {
    (f64::from(grams) / 1000.0 * f64::from(price_per_kg)).round() as i64
}

/// Client: long NPC id, long 0. Reply case 1200: int shop, then until the end
/// int species, int size class, int count, int total grams, float demand, int
/// price per kg, long total.
pub async fn open_market(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let shop = message.reader().int64()? as i32;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let world = &conn.world;
    let base = travel::current_base(&world.settings, player);
    let buys = world.settings.fish_market.buyers.get(&base).is_some_and(|npcs| npcs.contains(&shop));
    if !buys {
        conn.log(&format!("   NPC {shop} at base {base} does not buy fish; its own window is not made yet"));
        return Ok(Outcome::Unanswered);
    }

    let mut lots: BTreeMap<(i32, i32), (i32, i32)> = BTreeMap::new();
    for fish in player.data.fish.iter().filter(|fish| fish.list == IN_USE_LIST) {
        let lot = lots.entry((fish.species, fish.size_class)).or_default();
        lot.0 += 1;
        lot.1 += fish.grams;
    }

    let mut w = AfcWriter::new();
    w.int32(shop);
    let largest = world.fish.largest_share();
    for ((species_id, size_class), (count, grams)) in &lots {
        let Some(species) = world.fish.get(*species_id) else {
            conn.log(&format!("   market: species {species_id} is not in fish.json"));
            continue;
        };
        let price = price_per_kg(&world.settings.fish_market, largest, species, *size_class);
        w.int32(*species_id).int32(*size_class).int32(*count).int32(*grams);
        w.single(FULL_DEMAND).int32(price).int64(lot_total(*grams, price));
    }
    conn.send(msg::FISH_MARKET, w.finish()).await?;
    conn.log(&format!("   -> 1200 fish market {shop}: {} lots", lots.len()));
    Ok(Outcome::Answered)
}

/// Client: int shop. Reply case 1205: int shop, int unknown, float unknown, float
/// top demand, then until the end int species, int price per kg, int average
/// price, float demand.
pub async fn prices(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let shop = message.reader().int32()?;
    let conn = &session.conn;
    let world = &conn.world;
    let largest = world.fish.largest_share();

    let mut w = AfcWriter::new();
    w.int32(shop).int32(0).single(FULL_DEMAND).single(FULL_DEMAND);
    let mut count = 0;
    for species in world.fish.all() {
        let price = price_per_kg(&world.settings.fish_market, largest, species, 0);
        let typical_grams = (species.small_grams + species.large_grams) / 2;
        w.int32(species.id).int32(price).int32(lot_total(typical_grams, price) as i32);
        w.single(FULL_DEMAND);
        count += 1;
    }
    conn.send(msg::FISH_PRICES, w.finish()).await?;
    conn.log(&format!("   -> 1205 prices for {count} species"));
    Ok(Outcome::Answered)
}

/// Client: int shop, int species, int size class. Reply: case 173 per fish sold
/// and case 1210 with three doubles: earned, bonus, new balance.
pub async fn sell(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let mut r = message.reader();
    r.int32()?;
    let (species_id, size_class) = (r.int32()?, r.int32()?);

    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let world = &conn.world;

    let matches = |fish: &CaughtFish| {
        fish.list == IN_USE_LIST && fish.species == species_id && fish.size_class == size_class
    };
    let sold: Vec<i64> = player.data.fish.iter().filter(|fish| matches(fish)).map(|fish| fish.id).collect();
    let grams: i32 = player.data.fish.iter().filter(|fish| matches(fish)).map(|fish| fish.grams).sum();

    let earned = match world.fish.get(species_id) {
        Some(species) if !sold.is_empty() => {
            let largest = world.fish.largest_share();
            lot_total(grams, price_per_kg(&world.settings.fish_market, largest, species, size_class))
        }
        _ => 0,
    };

    player.data.fish.retain(|fish| !matches(fish));
    player.data.money += earned;
    conn.save(player).await?;

    for id in &sold {
        let mut w = AfcWriter::new();
        w.int64(*id);
        conn.send(msg::FISH_REMOVE, w.finish()).await?;
    }

    let mut w = AfcWriter::new();
    w.double(earned as f64).double(0.0).double(player.data.money as f64);
    conn.send(msg::FISH_SELL, w.finish()).await?;
    conn.log(&format!("   -> 1210 sold {} of species {species_id} class {size_class} for {earned}", sold.len()));
    Ok(Outcome::Answered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn market() -> FishMarketSettings {
        FishMarketSettings { base_price_per_kg: 100.0, large_price_factor: 1.5, trophy_price_factor: 3.0, buyers: Default::default() }
    }

    fn species(share: f64) -> Species {
        Species {
            id: 1,
            name: String::new(),
            small_grams: 100,
            large_grams: 1000,
            trophy_grams: 5000,
            min_depth_cm: 0,
            max_depth_cm: 0,
            near_bottom: false,
            likes_depth: false,
            night_factor: 1.0,
            hardy: false,
            share,
            baits: Vec::new(),
        }
    }

    #[test]
    fn commonest_fish_sells_at_the_base_price() {
        assert_eq!(price_per_kg(&market(), 9.0, &species(9.0), 0), 100);
    }

    #[test]
    fn rarity_and_size_raise_the_price() {
        assert_eq!(price_per_kg(&market(), 9.0, &species(1.0), 0), 300);
        assert_eq!(price_per_kg(&market(), 9.0, &species(9.0), TROPHY_CLASS), 300);
    }

    #[test]
    fn lot_total_is_by_weight() {
        assert_eq!(lot_total(1500, 200), 300);
    }
}
