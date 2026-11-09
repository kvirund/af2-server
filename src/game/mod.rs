//! Game rules, one file per system. Handlers run on the connection's own task
//! and own the character they change.

mod character;
mod fishing;
mod inventory;
mod keepnet;
mod shop;
mod travel;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use tokio::sync::{Notify, mpsc};

use crate::afc::{self, AfcMessage, AfcWriter};
use crate::messages as msg;
use crate::photon;
use crate::storage::{CaughtFish, CharacterData};
use crate::world::World;

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Answered,
    Unanswered,
}

/// The sending and logging half of a connection. It is a separate field of
/// [`Session`] so a handler can use it while it holds the player.
pub struct Conn {
    pub number: u64,
    frames: mpsc::Sender<Vec<u8>>,
    pub world: Arc<World>,
}

impl Conn {
    pub fn new(number: u64, frames: mpsc::Sender<Vec<u8>>, world: Arc<World>) -> Self {
        Self { number, frames, world }
    }

    pub fn log(&self, text: &str) {
        self.world.log.line(&format!("[c{}] {text}", self.number));
    }

    pub async fn send_raw(&self, frame: Vec<u8>) -> Result<()> {
        self.frames.send(frame).await.context("the connection's writer has stopped")
    }

    pub async fn send(&self, id: i32, payload: Vec<u8>) -> Result<()> {
        self.send_raw(photon::event(0, &afc::outgoing_params(id, payload))).await
    }

    /// Many requests raise a modal black overlay and wait for the reply to lower
    /// it. Case 1310 lowers it and ignores a status it does not recognise.
    pub async fn clear_overlay(&self) -> Result<()> {
        let mut w = AfcWriter::new();
        w.string("none=0");
        self.send(msg::CLEAR_OVERLAY, w.finish()).await
    }

    pub async fn save(&self, player: &Player) -> Result<()> {
        self.world.store.save_character(&player.data).await
    }
}

/// A fish on the hook, waiting for the client to report it landed.
pub struct Bite {
    pub species: i32,
    pub grams: i32,
    pub size_class: i32,
}

/// A character in play. `data` is what gets saved; the rest lasts one session.
pub struct Player {
    pub data: CharacterData,
    pub location: i32,
    pub bites_by_rod: HashMap<i32, Bite>,
    pub landed: HashMap<i64, CaughtFish>,
}

impl Player {
    fn new(data: CharacterData) -> Self {
        Self { data, location: fishing::DEFAULT_LOCATION, bites_by_rod: HashMap::new(), landed: HashMap::new() }
    }
}

pub struct Account {
    pub id: i64,
    pub persona: String,
}

pub struct Session {
    pub conn: Conn,
    pub account: Option<Account>,
    pub player: Option<Player>,
    kick: Arc<Notify>,
}

impl Session {
    pub fn new(conn: Conn, kick: Arc<Notify>) -> Self {
        Self { conn, account: None, player: None, kick }
    }

    pub fn leave(&mut self) {
        if let Some(player) = self.player.take() {
            self.conn.world.release(player.data.id, self.conn.number);
        }
    }
}

/// The character in play. A borrow of the field alone leaves `conn` usable.
fn active(player: &mut Option<Player>) -> Result<&mut Player> {
    player.as_mut().ok_or_else(|| anyhow!("no character has entered the game"))
}

pub async fn handle(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    match message.id {
        msg::LOGIN => character::login(session, message).await,
        msg::CURRENCY => character::currency(session).await,
        msg::CHARACTER_LIST => character::list(session).await,
        msg::GO_TO_GAME => character::go_to_game(session, message).await,

        msg::TRAVEL | msg::TRAVEL_ARRIVE => travel::go(session, message).await,
        msg::TRAVEL_OPTIONS => travel::options(session, message).await,
        msg::TRAVEL_START => {
            session.conn.log("   map travel started");
            Ok(Outcome::Answered)
        }

        msg::SHOP_OPEN | msg::SHOP_LIST => shop::list(session, message).await,
        msg::SHOP_BUY => shop::buy(session, message).await,
        msg::ITEM_MOVE => inventory::move_item(session, message).await,
        msg::ITEM_SWAP => inventory::swap_items(session, message).await,

        msg::BITE => fishing::bite(session, message).await,
        msg::FISH_LANDED => fishing::landed(session, message).await,
        msg::FISH_KEPT | msg::FISH_KEPT_ALT => fishing::kept(session, message).await,

        msg::FISH_TO_SPARE | msg::FISH_TO_IN_USE => keepnet::move_between_lists(session, message).await,
        msg::FISH_RELEASE => keepnet::release(session, message).await,
        msg::FISH_SWAP_KEEPNETS => keepnet::swap_keepnets(session).await,
        // The original server decided what each NPC offered, and the buyer is
        // not told apart in the client, so every NPC buys fish for now.
        msg::NPC_TALK => keepnet::open_market(session, message).await,
        msg::FISH_PRICES => keepnet::prices(session, message).await,
        msg::FISH_SELL => keepnet::sell(session, message).await,

        _ => Ok(Outcome::Unanswered),
    }
}
