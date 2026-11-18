//! Changes asked for from outside the game: the web port takes them, the
//! connection that plays the character carries them out.
//!
//! Every action here goes through the function the game itself uses, so the
//! character ends up in the state playing would have left them in and the
//! client is told about it the same way. Nothing in this module knows a rule of
//! its own, and nothing writes to the save behind the game's back: a character
//! nobody is playing cannot be changed at all, because the connection that
//! later picks them up would write over it.

use anyhow::Result;

use super::{Session, active, board, character, inventory, reputation};
use crate::world::{AdminAction, AdminJob};

/// Carries out one job on the character this connection is playing.
pub async fn run(session: &mut Session, job: AdminJob) {
    let told = apply(session, &job.action).await;
    let answer = match told {
        Ok(Ok(said)) => {
            session.conn.log(&format!("   admin: {said}"));
            Ok(said)
        }
        Ok(Err(refused)) => {
            session.conn.log(&format!("   admin refused: {refused}"));
            Err(refused)
        }
        Err(error) => {
            let text = format!("{error:#}");
            session.conn.log(&format!("   admin failed: {text}"));
            Err(text)
        }
    };
    // The asker may have given up waiting; the change still stands.
    let _ = job.answer.send(answer);
}

async fn apply(session: &mut Session, action: &AdminAction) -> Result<Result<String, String>> {
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    match action {
        AdminAction::Give { template, count } => {
            let count = (*count).max(1);
            let Some(item) = conn.world.catalog.get(*template) else {
                return Ok(Err(format!("предмета {template} нет в каталоге")));
            };
            match inventory::give(conn, player, item, count).await? {
                Ok(changed) => {
                    inventory::send_updates(conn, player.data.id, &changed).await?;
                    conn.save(player).await?;
                    Ok(Ok(format!("выдано {} x{count}", item.name)))
                }
                Err(error) => Ok(Err(format!("{} x{count} не поместилось: {}", item.name, error.status()))),
            }
        }
        AdminAction::Experience { amount } => {
            character::award_experience(conn, player, *amount, 1.0).await?;
            Ok(Ok(format!("опыт +{amount}, теперь {} на уровне {}", player.data.experience, player.data.level)))
        }
        AdminAction::Reputation { faction, points } => {
            reputation::add(conn, player, *faction, *points).await?;
            Ok(Ok(format!("репутация фракции {faction} +{points}")))
        }
        AdminAction::Money { amount } => {
            player.data.money += *amount;
            conn.save(player).await?;
            character::send_money(conn, player, *amount).await?;
            Ok(Ok(format!("деньги {:+}, теперь {}", amount, player.data.money)))
        }
        AdminAction::Premium { amount } => {
            player.data.premium += *amount;
            conn.save(player).await?;
            character::send_money(conn, player, 0).await?;
            Ok(Ok(format!("эрги {:+}, теперь {}", amount, player.data.premium)))
        }
        AdminAction::FinishTask { task } => finish_task(session, *task).await,
        AdminAction::DropTask { task } => drop_task(session, *task).await,
    }
}

/// Pays a task's reward and takes it off the list, as counting the last fish
/// toward it would have.
async fn finish_task(session: &mut Session, id: i64) -> Result<Result<String, String>> {
    let player = active(&mut session.player)?;
    let Some(index) = player.data.board_tasks.iter().position(|task| task.id == id) else {
        return Ok(Err(format!("задания {id} у персонажа нет")));
    };
    let task = player.data.board_tasks.remove(index);
    let conn = &session.conn;
    board::pay_and_close(conn, player, &task).await?;
    conn.save(player).await?;
    Ok(Ok(format!("задание {id} засчитано, выдано «{}»", task.reward)))
}

/// Takes a task off the list and pays nothing.
async fn drop_task(session: &mut Session, id: i64) -> Result<Result<String, String>> {
    let player = active(&mut session.player)?;
    let Some(index) = player.data.board_tasks.iter().position(|task| task.id == id) else {
        return Ok(Err(format!("задания {id} у персонажа нет")));
    };
    let task = player.data.board_tasks.remove(index);
    let conn = &session.conn;
    board::close_without_reward(conn, &task).await?;
    conn.save(player).await?;
    Ok(Ok(format!("задание {id} снято без награды")))
}
