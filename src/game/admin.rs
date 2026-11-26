//! Changes asked for from outside the game: the web port takes them and the
//! character carries them out, through the connection that plays them or, when
//! nobody does, through a connection with no client on the other end.
//!
//! Every action here goes through the function the game itself uses, so the
//! character ends up in the state playing would have left them in and the
//! client is told about it the same way. Nothing in this module knows a rule of
//! its own.
//!
//! A character someone is playing is changed on their own connection: that
//! connection owns the save, and writing to the store behind it would be
//! written over on its next save. A character nobody is playing is loaded from
//! the store, changed and saved here, one such change at a time
//! (`World::begin_edit`); what the client would have been told goes nowhere.

use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{Notify, mpsc};

use super::{Conn, Player, Session, active, board, character, inventory, reputation};
use crate::world::{AdminAction, AdminJob, Worlds};

/// The connection number an offline change logs under; the game's own start at 1.
const EDIT_CONNECTION: u64 = 0;
/// Frames the drain takes at a time. A change answers in one go, so this only
/// has to be wide enough that nothing waits on it for long.
const DRAIN_FRAMES: usize = 64;

/// Carries out one job on the character this connection is playing.
pub async fn run(session: &mut Session, job: AdminJob) {
    let answer = match run_one(session, &job.action).await {
        Ok(told) => told,
        Err(error) => Err(format!("{error:#}")),
    };
    // The asker may have given up waiting; the change still stands.
    let _ = job.answer.send(answer);
}

/// Carries out one job on a character nobody is playing: loads them, changes
/// them with the same functions the game uses and saves them. The messages
/// those functions send go into a channel nobody reads, since there is no
/// client to read them.
///
/// Refuses when another change holds the character, when they are in the game
/// after all (that goes through their connection) and when the save has no such
/// character. A character who enters the game while the change runs is told of,
/// because their connection loaded them before this save and will write over it.
pub async fn offline(worlds: Arc<Worlds>, character: i64, action: &AdminAction) -> Result<Result<String, String>> {
    let world = worlds.current();
    let Some(_editing) = world.begin_edit(character) else {
        return Ok(Err(format!("персонаж {character} уже меняется, повтори")));
    };
    if world.live_one(character).is_some() {
        return Ok(Err(format!("персонаж {character} вошёл в игру, меняй его через соединение")));
    }
    let Some(data) = world.store.load_character(character).await? else {
        return Ok(Err(format!("персонажа {character} нет в сохранении")));
    };

    let (frames, mut drain) = mpsc::channel(DRAIN_FRAMES);
    let nowhere = tokio::spawn(async move { while drain.recv().await.is_some() {} });
    let conn = Conn::new(EDIT_CONNECTION, frames, worlds);
    let mut session = Session::new(conn, Arc::new(Notify::new()), mpsc::channel(1).0);
    session.player = Some(Player::new(data));

    let told = run_one(&mut session, action).await;
    drop(session);
    nowhere.await?;

    if world.live_one(character).is_some() {
        return Ok(Err(format!("персонаж {character} вошёл в игру во время правки: она не сохранилась, повтори")));
    }
    told
}

/// One action, logged as it lands: `Ok` what to say, `Err` why it was refused.
async fn run_one(session: &mut Session, action: &AdminAction) -> Result<Result<String, String>> {
    let told = apply(session, action).await;
    match &told {
        Ok(Ok(said)) => session.conn.log(&format!("   admin: {said}")),
        Ok(Err(refused)) => session.conn.log(&format!("   admin refused: {refused}")),
        Err(error) => session.conn.log(&format!("   admin failed: {error:#}")),
    }
    told
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
            Ok(Ok(format!("опыт {amount:+}, теперь {} на уровне {}", player.data.experience, player.data.level)))
        }
        AdminAction::Reputation { faction, points } => {
            reputation::add(conn, player, *faction, *points).await?;
            Ok(Ok(format!("репутация фракции {faction} {points:+}")))
        }
        AdminAction::Money { amount } => {
            // Taking away more than there is empties the purse; no character
            // in the game is ever in the red.
            player.data.money = (player.data.money + *amount).max(0);
            conn.save(player).await?;
            character::send_money(conn, player, *amount).await?;
            // Said in credits, the way the game and the pages count them.
            Ok(Ok(format!("деньги {:+.2}, теперь {:.2} кр.", *amount as f64 / 100.0, player.data.money as f64 / 100.0)))
        }
        AdminAction::Premium { amount } => {
            player.data.premium = (player.data.premium + *amount).max(0);
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
