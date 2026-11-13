//! The bio-generator in Убежище: biofuel in, reputation with the base out.
//!
//! A click on it sends 60 with the NPC's number, which is also its dialog file
//! `2.lng`. Node 1 says how much biofuel the character carries (`[cnt]`) and
//! offers to hand in all of it (answer 1), one piece (3) or ten (4); 2 leaves.
//! Answer 5, "Сдать единицу фарша", is in the file but in no guide, so it is
//! never offered.

use anyhow::Result;

use super::dialog::{COUNT_MARK, close, open, option, replace, text};
use super::{Conn, Outcome, Player, Session, active, inventory, reputation, travel};
use crate::afc::AfcMessage;
use crate::config::GeneratorSettings;

const OFFER: i32 = 1;

const ALL: i32 = 1;
const LEAVE: i32 = 2;
const ONE: i32 = 3;
const TEN: i32 = 4;

fn is_here(conn: &Conn, player: &Player, npc: i64) -> bool {
    let generator = &conn.world.settings.reputation.generator;
    npc == i64::from(generator.npc) && travel::current_base(&conn.world.settings, player) == generator.base
}

async fn offer(conn: &Conn, player: &mut Player, generator: &GeneratorSettings) -> Result<()> {
    let carried = inventory::pieces_carried(&player.data, generator.item);
    text(conn, generator.npc, OFFER).await?;
    replace(conn, COUNT_MARK, &carried.to_string()).await?;
    if carried >= 1 {
        option(conn, generator.npc, OFFER, ALL, 0).await?;
        option(conn, generator.npc, OFFER, ONE, 0).await?;
    }
    if carried >= 10 {
        option(conn, generator.npc, OFFER, TEN, 0).await?;
    }
    option(conn, generator.npc, OFFER, LEAVE, 0).await?;
    open(conn, player, generator.npc).await?;
    conn.log(&format!("   -> 70/72/74 generator, {carried} biofuel carried"));
    Ok(())
}

/// Client 60 for the generator. `None` leaves the NPC to the others.
pub async fn talk(session: &mut Session, message: &AfcMessage) -> Result<Option<Outcome>> {
    let npc = message.reader().int64()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    if !is_here(conn, player, npc) {
        return Ok(None);
    }
    offer(conn, player, &conn.world.settings.reputation.generator).await?;
    Ok(Some(Outcome::Answered))
}

/// Whether the dialog open is the generator's.
pub fn is_open(session: &Session) -> bool {
    let npc = session.conn.world.settings.reputation.generator.npc;
    session.player.as_ref().and_then(|player| player.dialog) == Some(npc)
}

/// Client 65 while the generator's dialog is open.
pub async fn answer(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let answer = message.reader().int32()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let world = conn.world.clone();
    let generator = &world.settings.reputation.generator;

    let carried = inventory::pieces_carried(&player.data, generator.item);
    let amount = match answer {
        ALL => carried,
        ONE => 1,
        TEN => 10,
        _ => {
            close(conn, player, generator.npc).await?;
            return Ok(Outcome::Answered);
        }
    };
    let Some((smaller, gone)) = (amount > 0).then(|| inventory::take_carried(&mut player.data, generator.item, amount)).flatten() else {
        conn.log(&format!("   generator: {amount} biofuel asked, {carried} carried"));
        offer(conn, player, generator).await?;
        return Ok(Outcome::Answered);
    };
    for id in gone {
        inventory::send_removed(conn, id).await?;
    }
    inventory::send_updates(conn, player.data.id, &smaller).await?;
    conn.save(player).await?;
    conn.log(&format!("   generator took {amount} biofuel"));
    reputation::add(conn, player, generator.faction, generator.points * i64::from(amount)).await?;
    offer(conn, player, generator).await?;
    Ok(Outcome::Answered)
}
