//! Game rules, one file per system. Handlers run on the connection's own task
//! and own the character they change.

mod board;
mod cards;
mod chat;
mod character;
mod crafting;
mod dialog;
mod doctor;
mod effects;
mod fishing;
mod fridge;
mod generator;
mod gathering;
mod inventory;
mod loot;
mod keepnet;
mod packages;
mod panels;
mod perks;
mod refining;
mod reputation;
mod scripts;
mod shop;
mod skills;
mod spas;
mod travel;
mod vip;
mod vitals;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Instant;

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
    pub bait: i32,
    pub bait_item: i64,
}

/// A character in play. `data` is what gets saved; the rest lasts one session.
pub struct Player {
    pub data: CharacterData,
    pub location: i32,
    /// The base the character is at or fishing from; location 0 alone does not say.
    pub base: i32,
    pub bites_by_rod: HashMap<i32, Bite>,
    /// Gather points as this client last heard of them; `None` after a scene change.
    pub shown_points: Option<BTreeMap<i32, i32>>,
    /// The base and point whose gathering bar is running.
    pub harvesting: Option<(i32, i32)>,
    pub landed: HashMap<i64, CaughtFish>,
    /// The dialog file whose window is open; the client's answers do not say.
    pub dialog: Option<i32>,
    pub last_tick: Instant,
    pub last_save: Instant,
    pub last_active: Instant,
    /// VIP skill experience earned online and not handed out yet.
    pub vip_skill_points: f64,
    pub last_vip_tick: Instant,
}

impl Player {
    fn new(data: CharacterData) -> Self {
        let now = Instant::now();
        Self {
            data,
            location: fishing::DEFAULT_LOCATION,
            base: 0,
            bites_by_rod: HashMap::new(),
            shown_points: None,
            harvesting: None,
            landed: HashMap::new(),
            dialog: None,
            last_tick: now,
            last_save: now,
            last_active: now,
            vip_skill_points: 0.0,
            last_vip_tick: now,
        }
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

/// Time passing for the character, if enough of it has.
pub async fn tick(session: &mut Session) -> Result<()> {
    vitals::tick(session).await?;
    vip::tick(session).await
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
        msg::WALK_ON => travel::walk_on(session, message).await,
        msg::TRAVEL_OPTIONS => travel::options(session, message).await,
        msg::BASE_LIST => travel::return_to_base(session).await,
        msg::TRAVEL_START => {
            session.conn.log("   map travel started");
            Ok(Outcome::Answered)
        }

        msg::SHOP_OPEN | msg::SHOP_LIST => shop::list(session, message).await,
        msg::SHOP_BUY => shop::buy(session, message).await,
        msg::ITEM_MOVE => inventory::move_item(session, message).await,
        msg::ITEM_SWAP => inventory::swap_items(session, message).await,
        msg::ITEM_MERGE => inventory::merge_items(session, message).await,
        msg::ITEMS_REMOVED => inventory::throw_away(session, message).await,
        msg::ITEM_BROKEN => inventory::broken(session, message).await,
        msg::LINE_TORN => inventory::line_torn(session, message).await,
        msg::TACKLE_LOST => inventory::tackle_lost(session, message).await,
        msg::TACKLE_NEWS => {
            session.conn.log("   tackle news: no other players to tell");
            Ok(Outcome::Answered)
        }

        msg::BITE => fishing::bite(session, message).await,
        msg::FISH_LANDED => fishing::landed(session, message).await,
        msg::FISH_KEPT | msg::FISH_KEPT_ALT => fishing::kept(session, message).await,

        msg::FISH_TO_SPARE | msg::FISH_TO_IN_USE => keepnet::move_between_lists(session, message).await,
        msg::FISH_RELEASE => keepnet::release(session, message).await,
        msg::FISH_SWAP_KEEPNETS => keepnet::swap_keepnets(session).await,
        msg::FRIDGE => fridge::open(session, message).await,
        msg::FRIDGE_PUT | msg::FRIDGE_TAKE => fridge::transfer(session, message).await,
        // The doctor talks; of the rest only the NPCs listed in fishMarket.buyers
        // trade fish, and the others' windows are still to do.
        msg::NPC_TALK => match doctor::talk(session, message).await? {
            Some(outcome) => Ok(outcome),
            None => match generator::talk(session, message).await? {
                Some(outcome) => Ok(outcome),
                None => match spas::talk(session, message).await? {
                    Some(outcome) => Ok(outcome),
                    None => keepnet::open_market(session, message).await,
                },
            },
        },
        msg::DIALOG_ANSWER if generator::is_open(session) => generator::answer(session, message).await,
        msg::DIALOG_ANSWER if spas::is_open(session) => spas::answer(session, message).await,
        msg::DIALOG_ANSWER => doctor::answer(session, message).await,
        msg::REPUTATION => reputation::tab(session).await,
        msg::CHARACTER_CARD => panels::card(session, message).await,
        msg::STATISTICS => cards::statistics(session).await,
        msg::RECORDS if matches!(message.reader().int32(), Ok(0)) => cards::records(session).await,
        id @ (msg::TITLES | msg::RECORDS | msg::ACHIEVEMENTS) => {
            panels::empty_tab(session, message, id).await
        }
        msg::PERKS => perks::window(session).await,
        msg::PERK_STUDY => skills::choose_study(session, message).await,
        msg::COURSE_STUDY => skills::study_course(session, message).await,
        msg::AUCTION => panels::auction(session).await,
        msg::ARTELS => panels::artels(session).await,
        id @ (msg::CRAFT | msg::CRAFT_STATION) => crafting::station(session, message, id).await,
        msg::RECIPE_LEARN => crafting::learn(session, message).await,
        msg::CRAFT_MAKE => crafting::make(session, message).await,
        msg::USER_STATE => gathering::show(session).await,
        msg::HARVEST_START => gathering::start(session, message).await,
        msg::HARVEST_FINISH => gathering::finish(session, message).await,
        msg::REFINE => refining::refine(session, message).await,
        msg::STORAGE => panels::storage(session).await,
        msg::BAG_PRICE => panels::bag_price(session, message).await,
        msg::BAG_BOUGHT => panels::buy_cells(session, message).await,
        msg::ITEM_SPLIT => inventory::split_item(session, message).await,
        msg::ITEM_USE => vitals::use_item(session, message).await,
        msg::FISH_PRICES => keepnet::prices(session, message).await,
        msg::FISH_SELL => keepnet::sell(session, message).await,
        msg::BOARD => board::open(session, message).await,
        msg::BOARD_REFUSE => board::refuse(session, message).await,
        msg::BOARD_TAKE => board::take(session, message).await,
        msg::VIP_BUY => vip::buy(session).await,

        _ => Ok(Outcome::Unanswered),
    }
}
