//! Gathering at the points of a base scene: stumps, burrows and logs with bait
//! in them, mushrooms, plants and rubbish. The points and the models are the
//! client's; what each gives is in `config/gathering.json`.

use anyhow::Result;
use rand::random;

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use super::{Conn, Outcome, Player, Session, active, inventory, travel, vitals};
use crate::afc::{AfcMessage, AfcWriter};
use crate::config::{GatherResource, GatheringSettings};
use crate::messages as msg;
use crate::world::GatherPoint;

/// Resource number of an empty point; the client then removes the model.
const EMPTY: i32 = 0;

fn pick<T: Copy>(choices: impl Iterator<Item = (T, f64)>, roll: f64) -> Option<T> {
    let choices: Vec<(T, f64)> = choices.filter(|(_, weight)| *weight > 0.0).collect();
    let mut left = roll * choices.iter().map(|(_, weight)| weight).sum::<f64>();
    for (value, weight) in choices {
        left -= weight;
        if left < 0.0 {
            return Some(value);
        }
    }
    None
}

fn respawn_delay(settings: &GatheringSettings) -> Duration {
    let [low, high] = settings.respawn_minutes;
    let minutes = low + (high - low).max(0.0) * random::<f64>();
    Duration::from_secs_f64(minutes.max(0.0) * 60.0)
}

fn grow(settings: &GatheringSettings, base: i32, kind: i32) -> i32 {
    settings
        .bases
        .get(&base)
        .and_then(|info| info.spawns.get(&kind))
        .and_then(|spawns| pick(spawns.iter().map(|(resource, weight)| (*resource, *weight)), random()))
        .unwrap_or(EMPTY)
}

/// The base's points now: the first visit fills them, and empty points whose
/// time has come grow something again.
fn current(settings: &GatheringSettings, all: &mut HashMap<i32, BTreeMap<i32, GatherPoint>>, base: i32) -> BTreeMap<i32, i32> {
    let now = Instant::now();
    let points = all.entry(base).or_insert_with(|| {
        let mut points = BTreeMap::new();
        for (kind, ids) in settings.bases.get(&base).map(|info| &info.points).into_iter().flatten() {
            for id in ids {
                let filled = random::<f64>() * 100.0 < settings.start_filled_percent;
                let resource = if filled { grow(settings, base, *kind) } else { EMPTY };
                points.insert(*id, GatherPoint { kind: *kind, resource, respawn_at: now + respawn_delay(settings) });
            }
        }
        points
    });
    for point in points.values_mut().filter(|point| point.resource == EMPTY && point.respawn_at <= now) {
        point.resource = grow(settings, base, point.kind);
        point.respawn_at = now + respawn_delay(settings);
    }
    points.iter().map(|(id, point)| (*id, point.resource)).collect()
}

fn points_now(conn: &Conn, base: i32) -> BTreeMap<i32, i32> {
    let mut all = conn.world.gather_points.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    current(&conn.world.settings.gathering, &mut all, base)
}

/// Points whose resource differs from what the client was last told.
fn changes(shown: &BTreeMap<i32, i32>, now: &BTreeMap<i32, i32>) -> Vec<(i32, i32)> {
    now.iter()
        .filter(|(id, resource)| shown.get(id).copied().unwrap_or(EMPTY) != **resource)
        .map(|(id, resource)| (*id, *resource))
        .collect()
}

/// Cases 161 and 162: until the end, int point, int resource (the model
/// `restype_N`), int work points, two triples (a quest task and the profession
/// that gives a bonus; none here) and a string of perks.
async fn send_points(conn: &Conn, id: i32, points: &[(i32, i32)]) -> Result<()> {
    let resources = &conn.world.settings.gathering.resources;
    let mut w = AfcWriter::new();
    for (point, resource) in points {
        let work = resources.get(resource).map_or(0.0, |info| info.work_points);
        w.int32(*point).int32(*resource).int32(work as i32);
        for _ in 0..6 {
            w.double(0.0);
        }
        w.string("");
    }
    conn.send(id, w.finish()).await
}

/// Client: its state every three seconds, sent only once a scene with the
/// player in it has loaded. A new scene gets every point (161, which clears
/// the old ones first); after that only the points that changed (162).
pub async fn show(session: &mut Session) -> Result<Outcome> {
    let Some(player) = session.player.as_mut() else {
        return Ok(Outcome::Answered);
    };
    let conn = &session.conn;
    let base = travel::current_base(&conn.world.settings, player);
    let now = points_now(conn, base);
    match &player.shown_points {
        None => {
            let filled: Vec<(i32, i32)> = now.iter().filter(|(_, r)| **r != EMPTY).map(|(p, r)| (*p, *r)).collect();
            send_points(conn, msg::GATHER_POINTS, &filled).await?;
            conn.log(&format!("   -> 161 base {base}: {} of {} gather points filled", filled.len(), now.len()));
        }
        Some(shown) => {
            let changed = changes(shown, &now);
            if !changed.is_empty() {
                send_points(conn, msg::GATHER_UPDATE, &changed).await?;
                conn.log(&format!("   -> 162 base {base}: {} gather point(s) changed", changed.len()));
            }
        }
    }
    player.shown_points = Some(now);
    Ok(Outcome::Answered)
}

async fn reply(conn: &Conn, id: i32, text: &str) -> Result<Outcome> {
    let mut w = AfcWriter::new();
    w.string(text);
    conn.send(id, w.finish()).await?;
    conn.log(&format!("   -> {id} {text}"));
    Ok(Outcome::Answered)
}

fn work_points(player: &Player) -> f32 {
    player.data.vitals.as_ref().map_or(0.0, |vitals| vitals.work_points)
}

/// Client: int point, when the player walks up to it. Reply case 1500: "ok=N"
/// starts an N-second bar, after which the client sends 1510; "none" (nothing
/// there), "nowp" (too few work points) and "noproff" show a red message.
pub async fn start(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let point = message.reader().int32()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let settings = &conn.world.settings;
    let base = travel::current_base(settings, player);
    let resource = points_now(conn, base).get(&point).copied().unwrap_or(EMPTY);
    player.harvesting = None;

    let Some(info) = settings.gathering.resources.get(&resource) else {
        return reply(conn, msg::HARVEST_START, "none=0").await;
    };
    if work_points(player) < info.work_points {
        return reply(conn, msg::HARVEST_START, "nowp=0").await;
    }
    if info.skill_level > 0 {
        conn.log(&format!("   {} needs Ремесло {}; skills are not modelled, so it is allowed", info.name, info.skill_level));
    }
    player.harvesting = Some((base, point));
    reply(conn, msg::HARVEST_START, &format!("ok={}", info.seconds)).await
}

/// Pieces of each item one gathering gives.
fn roll(info: &GatherResource, mut random: impl FnMut() -> f64) -> BTreeMap<i32, i32> {
    let mut found = BTreeMap::new();
    for _ in 0..info.rolls.max(1) {
        let Some(drop) = pick(info.drops.iter().map(|drop| (drop, drop.weight)), random()) else {
            continue;
        };
        let (low, high) = (drop.min.max(1), drop.max.max(drop.min.max(1)));
        let count = low + ((high - low + 1) as f64 * random()).floor().min((high - low) as f64) as i32;
        *found.entry(drop.item).or_insert(0) += count;
    }
    found
}

/// Client: int point, when the bar is full. Reply case 1510: "ok" plays a
/// sound; the items come as updates and case 125 (int item, int count, int
/// quality, repeated) before it, and the emptied point as 162.
pub async fn finish(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let point = message.reader().int32()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let world = conn.world.clone();
    let settings = &world.settings;
    let base = travel::current_base(settings, player);
    if player.harvesting.take() != Some((base, point)) {
        return reply(conn, msg::HARVEST_FINISH, "none=0").await;
    }

    let taken = {
        let mut all = world.gather_points.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        current(&settings.gathering, &mut all, base);
        all.get_mut(&base).and_then(|points| points.get_mut(&point)).and_then(|spot| {
            let resource = std::mem::replace(&mut spot.resource, EMPTY);
            spot.respawn_at = Instant::now() + respawn_delay(&settings.gathering);
            settings.gathering.resources.get(&resource)
        })
    };
    let Some(info) = taken else {
        return reply(conn, msg::HARVEST_FINISH, "none=0").await;
    };
    if work_points(player) < info.work_points {
        return reply(conn, msg::HARVEST_FINISH, "nowp=0").await;
    }
    if let Some(vitals) = player.data.vitals.as_mut() {
        vitals.work_points -= info.work_points;
    }

    let mut changed = Vec::new();
    let mut gained = AfcWriter::new();
    let mut names = Vec::new();
    for (template, count) in roll(info, random) {
        let Some(item) = world.catalog.get(template) else {
            conn.log(&format!("   gathered item {template} is not in the catalog"));
            continue;
        };
        match inventory::give(conn, player, item, count).await? {
            Ok(entries) => {
                changed.extend(entries);
                gained.int32(template).int32(count).int32(0);
                names.push(format!("{template} x{count}"));
            }
            Err(error) => conn.log(&format!("   gathered {template} x{count} lost: {}", error.status())),
        }
    }
    conn.save(player).await?;
    inventory::send_updates(conn, player.data.id, &changed).await?;
    vitals::send(conn, player).await?;
    if !names.is_empty() {
        conn.send(msg::ITEMS_GAINED, gained.finish()).await?;
    }
    send_points(conn, msg::GATHER_UPDATE, &[(point, EMPTY)]).await?;
    if let Some(shown) = player.shown_points.as_mut() {
        shown.insert(point, EMPTY);
    }
    conn.log(&format!("   gathered {} at base {base} point {point}: {}", info.name, names.join(", ")));
    reply(conn, msg::HARVEST_FINISH, "ok=0").await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{GatherBase, GatherDrop};

    fn drop(item: i32, weight: f64, min: i32, max: i32) -> GatherDrop {
        GatherDrop { item, weight, min, max }
    }

    fn settings() -> GatheringSettings {
        let base = GatherBase {
            points: BTreeMap::from([(1, vec![1, 2, 3]), (2, vec![9])]),
            spawns: BTreeMap::from([(1, BTreeMap::from([(101, 1.0)])), (2, BTreeMap::from([(105, 1.0)]))]),
        };
        GatheringSettings {
            respawn_minutes: [0.0, 0.0],
            start_filled_percent: 100.0,
            resources: BTreeMap::new(),
            bases: BTreeMap::from([(3, base)]),
        }
    }

    #[test]
    fn each_point_type_grows_its_own_things() {
        let mut all = HashMap::new();
        let points = current(&settings(), &mut all, 3);
        assert_eq!(points, BTreeMap::from([(1, 101), (2, 101), (3, 101), (9, 105)]));
        assert!(current(&settings(), &mut all, 8).is_empty());
    }

    #[test]
    fn an_emptied_point_grows_back_when_its_time_comes() {
        let mut all = HashMap::new();
        current(&settings(), &mut all, 3);
        let spot = all.get_mut(&3).unwrap().get_mut(&2).unwrap();
        spot.resource = EMPTY;
        spot.respawn_at = Instant::now() + Duration::from_secs(600);
        assert_eq!(current(&settings(), &mut all, 3)[&2], EMPTY);
        all.get_mut(&3).unwrap().get_mut(&2).unwrap().respawn_at = Instant::now();
        assert_eq!(current(&settings(), &mut all, 3)[&2], 101);
    }

    #[test]
    fn only_changed_points_are_sent_again() {
        let shown = BTreeMap::from([(1, 101), (2, 0), (3, 102)]);
        let now = BTreeMap::from([(1, 101), (2, 103), (3, 0)]);
        assert_eq!(changes(&shown, &now), vec![(2, 103), (3, 0)]);
    }

    #[test]
    fn drops_add_up_over_rolls_within_their_counts() {
        let info = GatherResource {
            name: String::new(),
            work_points: 1.0,
            seconds: 1.0,
            skill_level: 0,
            rolls: 3,
            drops: vec![drop(1, 1.0, 2, 4), drop(2, 1.0, 1, 1)],
        };
        let mut rolls = [0.1, 0.0, 0.1, 0.999, 0.9, 0.5].into_iter();
        let found = roll(&info, || rolls.next().unwrap());
        assert_eq!(found, BTreeMap::from([(1, 6), (2, 1)]));
    }
}
