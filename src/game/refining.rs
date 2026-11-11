//! Refining («Переработка» in an item's menu): an item whose description has
//! `<refine>` breaks into other items, one piece at a time.

use anyhow::Result;
use rand::random;

use super::{Conn, Outcome, Session, active, inventory, perks, vitals};
use crate::afc::{AfcMessage, AfcWriter};
use crate::messages as msg;

/// The client's refine window states this price per piece.
const WORK_POINTS: f32 = 20.0;
/// «Рефактор»: each level adds its `val` in percent to every chance.
const REFACTOR: i32 = 133;

/// `<refine>`: comma-separated "item/count/chance", the chance in percent. The
/// client draws 100 as "всегда" and lower ones as "часто" down to "крайне редко".
fn outputs(xml: &str) -> Vec<(i32, i32, f64)> {
    let Some((_, rest)) = xml.split_once("<refine>") else {
        return Vec::new();
    };
    let text = rest.split('<').next().unwrap_or("");
    text.split(',')
        .filter_map(|one| {
            let mut fields = one.trim().split('/');
            let item = fields.next()?.parse().ok()?;
            let count = fields.next().and_then(|count| count.parse().ok()).unwrap_or(1);
            let chance = fields.next().and_then(|chance| chance.parse().ok()).unwrap_or(100.0);
            Some((item, count, chance))
        })
        .collect()
}

/// Each output comes up on its own chance plus the skill's bonus.
fn roll(outputs: &[(i32, i32, f64)], bonus: f64, mut percent: impl FnMut() -> f64) -> Vec<(i32, i32)> {
    outputs.iter().filter(|(_, _, chance)| percent() < chance + bonus).map(|(item, count, _)| (*item, *count)).collect()
}

async fn reply(conn: &Conn, text: &str) -> Result<Outcome> {
    let mut w = AfcWriter::new();
    w.string(text);
    conn.send(msg::REFINE, w.finish()).await?;
    conn.log(&format!("   -> {} {text}", msg::REFINE));
    Ok(Outcome::Answered)
}

/// Client: long item id, after the window's two-second bar. Reply case 290, one
/// key: "result=item/count,..." adds to the list in the window, "noresult" adds
/// nothing; both start the bar again for the next piece. "isend" closes the
/// window quietly, "nowpn", "noworkpoints" and "norefine" close it with an error.
/// So the window keeps going until the stack is gone or the work points run out.
pub async fn refine(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let id = message.reader().int64()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let world = conn.world.clone();

    let Some(template) = player.data.items.iter().find(|item| item.id == id).map(|item| item.template) else {
        // The last piece went on the previous round.
        return reply(conn, "isend").await;
    };
    let made = world.catalog.get(template).map(|item| outputs(&item.xml)).unwrap_or_default();
    if made.is_empty() {
        return reply(conn, "norefine").await;
    }
    let Some(vitals_now) = player.data.vitals.as_mut().filter(|vitals| vitals.work_points >= WORK_POINTS) else {
        return reply(conn, "noworkpoints").await;
    };
    vitals_now.work_points -= WORK_POINTS;

    inventory::use_piece(conn, player, id).await?;
    let mut changed = Vec::new();
    let mut got = Vec::new();
    let per_level = world.settings.skill_tree.get(REFACTOR).map_or(0.0, |node| node.effect[0]);
    let bonus = f64::from(perks::level_of(conn, player, REFACTOR)) * per_level;
    for (item, count) in roll(&made, bonus, || random::<f64>() * 100.0) {
        let Some(catalog_item) = world.catalog.get(item) else {
            conn.log(&format!("   refined item {item} is not in the catalog"));
            continue;
        };
        match inventory::give(conn, player, catalog_item, count).await? {
            Ok(entries) => {
                changed.extend(entries);
                got.push(format!("{item}/{count}"));
            }
            Err(error) => conn.log(&format!("   refined {item} x{count} lost: {}", error.status())),
        }
    }
    conn.save(player).await?;
    inventory::send_updates(conn, player.data.id, &changed).await?;
    vitals::send(conn, player).await?;
    conn.log(&format!("   refined {template} (item {id})"));
    let status = if got.is_empty() { "noresult".to_owned() } else { format!("result={}", got.join(",")) };
    let outcome = reply(conn, &status).await?;
    // After either reply the client opens the next bar on its own, even for the
    // last piece. Closing that window at once keeps it from running for nothing.
    if !player.data.items.iter().any(|item| item.id == id) {
        reply(conn, "isend").await?;
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outputs_are_read_from_the_description() {
        let xml = "<base><refine>1167/5/100,1206/1/100,1268/1/25</refine></base>";
        assert_eq!(outputs(xml), vec![(1167, 5, 100.0), (1206, 1, 100.0), (1268, 1, 25.0)]);
        assert!(outputs("<base><refine></refine></base>").is_empty());
        assert!(outputs("<base></base>").is_empty());
    }

    #[test]
    fn each_output_has_its_own_chance() {
        let made = [(1, 5, 100.0), (2, 1, 25.0)];
        assert_eq!(roll(&made, 0.0, || 99.9), vec![(1, 5)]);
        assert_eq!(roll(&made, 0.0, || 10.0), vec![(1, 5), (2, 1)]);
        // «Рефактор» 2 at 5% a level turns 25 into 35.
        assert_eq!(roll(&made, 0.0, || 30.0), vec![(1, 5)]);
        assert_eq!(roll(&made, 10.0, || 30.0), vec![(1, 5), (2, 1)]);
    }
}
