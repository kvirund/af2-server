//! The server's web pages, on their own port (`server.webPort`, 0: none) so
//! access to them can be limited apart from the game's: who is in play, every
//! character, and a fishing advisor that works the odds out the game's way.
//!
//! Plain HTTP/1.1, GET only, one request per connection: `/` is the page,
//! `/api/online`, `/api/characters`, `/api/world` (bases, fish, baits, the
//! game's clock) and `/api/advisor` are its JSON. The advisor takes
//! `character`, `base` (a number or `all`), `time` (`day`, `night`, `both`),
//! `scope` (`mine`, `shop`, `all`), `tackle` (a rod's bag or `none`), `bait`,
//! `species`, `goal` (`money`, `experience`, `bites`, `trophies`), `bottom`
//! (cm, where no cast measured it) and `depths=all`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::game::advisor::{self, Forecast, Rod, Spot, Tackle};
use crate::game::fishing;
use crate::game::{LastCast, death, keepnet};
use crate::storage::CharacterData;
use crate::world::World;

const PAGE: &str = include_str!("web/index.html");
const MOST_REQUEST_BYTES: usize = 16 * 1024;
const JSON: &str = "application/json; charset=utf-8";
/// How long an admin change may take before the asker is told it did not land.
const ADMIN_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

pub async fn serve(listener: TcpListener, world: Arc<World>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let world = world.clone();
        tokio::spawn(async move {
            if let Err(error) = answer(stream, &world).await {
                world.log.line(&format!("web: {error:#}"));
            }
        });
    }
}

async fn answer(mut stream: TcpStream, world: &World) -> Result<()> {
    let mut request = Vec::new();
    let mut chunk = [0u8; 4096];
    while !request.windows(4).any(|end| end == b"\r\n\r\n") && request.len() < MOST_REQUEST_BYTES {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
    }
    // A POST body may trail the headers; read the rest of it before answering.
    let text = String::from_utf8_lossy(&request).into_owned();
    let length = header(&text, "content-length").and_then(|value| value.parse::<usize>().ok()).unwrap_or(0);
    let head_bytes = text.find("\r\n\r\n").map_or(request.len(), |at| at + 4);
    while request.len() < head_bytes + length.min(MOST_REQUEST_BYTES) {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
    }
    let whole = String::from_utf8_lossy(&request).into_owned();
    let body_text = whole.get(head_bytes..).unwrap_or("").to_string();

    let mut words = whole.lines().next().unwrap_or("").split(' ');
    let (status, kind, body) = match (words.next(), words.next()) {
        (Some("GET"), Some(target)) => route(world, target).await,
        (Some("POST"), Some(target)) => post(world, target, &whole, &body_text).await,
        _ => ("405 Method Not Allowed", "text/plain; charset=utf-8", "GET or POST".to_string()),
    };
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(reply.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

/// One request header's value, by its lowercase name.
fn header<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request
        .lines()
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.split_once(':'))
        .find(|(key, _)| key.trim().eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim())
}

/// The secret that unlocks the admin API. `AF2_ADMIN_TOKEN` comes first so a
/// real one need never be written into `config/settings.json`, which is in the
/// repository and goes out with every release.
fn admin_token(world: &World) -> String {
    match std::env::var("AF2_ADMIN_TOKEN") {
        Ok(token) if !token.trim().is_empty() => token,
        _ => world.settings.server.admin_token.clone(),
    }
}

/// Changes, as against the pages' read-only GETs. The token keeps a page open
/// in a browser from sending these behind the player's back: localhost is no
/// wall, any site can POST to it. An empty `server.adminToken` turns the whole
/// thing off, which is how it ships.
async fn post(world: &World, target: &str, request: &str, body: &str) -> (&'static str, &'static str, String) {
    let plain = "text/plain; charset=utf-8";
    let wanted = admin_token(world);
    let wanted = wanted.trim();
    if wanted.is_empty() {
        return ("404 Not Found", plain, "админский доступ выключен: задай AF2_ADMIN_TOKEN".into());
    }
    if header(request, "x-admin-token") != Some(wanted) {
        world.log.line(&format!("web: admin request to {target} without the token"));
        return ("403 Forbidden", plain, "нужен заголовок X-Admin-Token".into());
    }
    let (path, args) = query(target);
    match path {
        "/api/admin/do" => admin_do(world, &args, body).await,
        _ => ("404 Not Found", plain, "not found".into()),
    }
}

/// Hands one change to the connection playing the character and waits for it.
async fn admin_do(world: &World, args: &HashMap<&str, &str>, body: &str) -> (&'static str, &'static str, String) {
    let plain = "text/plain; charset=utf-8";
    let Some(character) = args.get("character").and_then(|value| value.parse::<i64>().ok()) else {
        return ("400 Bad Request", plain, "нужен ?character=<id>".into());
    };
    let action: crate::world::AdminAction = match serde_json::from_str(body) {
        Ok(action) => action,
        Err(error) => return ("400 Bad Request", plain, format!("тело запроса: {error}")),
    };
    let Some(channel) = world.admin_channel(character) else {
        let online = world.online_characters();
        return (
            "409 Conflict",
            plain,
            format!("персонаж {character} не в игре, менять его некому; сейчас играют: {online:?}"),
        );
    };
    let (answer, wait) = tokio::sync::oneshot::channel();
    let job = crate::world::AdminJob { action, answer };
    if channel.send(job).await.is_err() {
        return ("409 Conflict", plain, "соединение персонажа закрылось".into());
    }
    match tokio::time::timeout(ADMIN_WAIT, wait).await {
        Ok(Ok(Ok(said))) => ("200 OK", plain, said),
        Ok(Ok(Err(refused))) => ("422 Unprocessable Entity", plain, refused),
        Ok(Err(_)) => ("409 Conflict", plain, "соединение персонажа закрылось".into()),
        Err(_) => ("504 Gateway Timeout", plain, "персонаж не ответил вовремя".into()),
    }
}

fn query(target: &str) -> (&str, HashMap<&str, &str>) {
    let (path, rest) = target.split_once('?').unwrap_or((target, ""));
    (path, rest.split('&').filter_map(|pair| pair.split_once('=')).collect())
}

async fn route(world: &World, target: &str) -> (&'static str, &'static str, String) {
    let (path, args) = query(target);
    let json = |result: Result<String>| match result {
        Ok(body) => ("200 OK", JSON, body),
        Err(error) => ("500 Internal Server Error", "text/plain; charset=utf-8", format!("{error:#}")),
    };
    match path {
        "/" | "/index.html" => ("200 OK", "text/html; charset=utf-8", PAGE.to_string()),
        "/api/online" => json(serde_json::to_string(&online(world)).map_err(Into::into)),
        "/api/characters" => json(characters(world).await),
        "/api/world" => json(serde_json::to_string(&reference(world)).map_err(Into::into)),
        "/api/advisor" => json(advise(world, advice_query(&args)).await),
        _ => ("404 Not Found", "text/plain; charset=utf-8", "not found".into()),
    }
}

fn base_name(world: &World, base: i32) -> String {
    world.settings.bases.get(&base).map_or_else(|| format!("база {base}"), |found| found.name.clone())
}

fn location_name(world: &World, location: i32) -> String {
    if location == 0 {
        return "на базе".into();
    }
    world.settings.location_names.get(&location).cloned().unwrap_or_else(|| format!("локация {location}"))
}

fn item_name(world: &World, template: i32) -> String {
    world.catalog.get(template).map(|item| item.name.clone()).filter(|name| !name.is_empty()).unwrap_or_else(|| format!("предмет {template}"))
}

fn played(character: &CharacterData) -> i64 {
    i64::from(character.quest_values.get(death::PLAYED_KEY).copied().unwrap_or(0))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CastRow {
    location: i32,
    location_name: String,
    bait_name: String,
    #[serde(flatten)]
    cast: LastCast,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OnlineRow {
    id: i64,
    name: String,
    level: i32,
    base_name: String,
    location_name: String,
    session_seconds: i64,
    played_seconds: i64,
    dead: bool,
    money: f64,
    keepnet: usize,
    casts: Vec<CastRow>,
}

fn online(world: &World) -> Vec<OnlineRow> {
    let now = Utc::now().timestamp();
    world
        .live()
        .into_iter()
        .map(|live| OnlineRow {
            id: live.data.id,
            name: live.data.name.clone(),
            level: live.data.level,
            base_name: base_name(world, live.base),
            location_name: location_name(world, live.location),
            session_seconds: now - live.entered_at,
            played_seconds: played(&live.data),
            dead: live.dead,
            money: live.data.money as f64 / 100.0,
            keepnet: live.data.fish.iter().filter(|fish| keepnet::in_keepnet(fish)).count(),
            casts: live
                .casts
                .iter()
                .map(|(location, cast)| CastRow { location: *location, location_name: location_name(world, *location), bait_name: item_name(world, cast.bait), cast: *cast })
                .collect(),
        })
        .collect()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CharacterRow {
    id: i64,
    name: String,
    level: i32,
    experience: i64,
    money: f64,
    premium: i64,
    vip: bool,
    played_seconds: i64,
    online: bool,
    keepnet: usize,
    records: usize,
}

async fn characters(world: &World) -> Result<String> {
    let mut rows = Vec::new();
    for summary in world.store.all_characters().await? {
        let live = world.live_one(summary.id);
        let data = match &live {
            Some(live) => Some(live.data.clone()),
            None => world.store.load_character(summary.id).await?,
        };
        let Some(data) = data else {
            continue;
        };
        rows.push(CharacterRow {
            id: data.id,
            name: data.name.clone(),
            level: data.level,
            experience: data.experience,
            money: data.money as f64 / 100.0,
            premium: data.premium,
            vip: data.vip,
            played_seconds: played(&data),
            online: live.is_some(),
            keepnet: data.fish.iter().filter(|fish| keepnet::in_keepnet(fish)).count(),
            records: data.records.len(),
        });
    }
    Ok(serde_json::to_string(&rows)?)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LocationRef {
    id: i32,
    name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BaseRef {
    id: i32,
    name: String,
    min_level: i32,
    locations: Vec<LocationRef>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SpeciesRef {
    id: i32,
    name: String,
    small_grams: i32,
    large_grams: i32,
    trophy_grams: i32,
    /// The heaviest a trophy grows.
    top_grams: f64,
    min_depth_cm: i32,
    max_depth_cm: i32,
    near_bottom: bool,
    likes_depth: bool,
    night_factor: f64,
    /// Credits per kg: ordinary, large, trophy.
    price_per_kg: [f64; 3],
    /// Empty: any bait. Leaves out the baits no fish of this species can ever
    /// stay on, which is not the same as the ones only its big fish stay on.
    baits: Vec<i32>,
    /// For those of `baits` that do not hold the species' ordinary fish, the
    /// size class they start holding at: 1 large and up, 2 trophies only.
    bait_from_class: BTreeMap<i32, i32>,
    locations: Vec<i32>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BaitRef {
    id: i32,
    name: String,
    kind: i32,
    /// Credits for one purchase of `pack` pieces, 0 if not sold for credits.
    price: f64,
    pack: i32,
    level: i32,
    /// Bases whose shops sell it.
    sold_at: Vec<i32>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Clock {
    game_hour: f64,
    night: bool,
    game_hours_per_real_hour: f64,
    night_start: f64,
    night_end: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Reference {
    bases: Vec<BaseRef>,
    species: Vec<SpeciesRef>,
    baits: Vec<BaitRef>,
    clock: Clock,
    bite_request_seconds: f64,
    near_bottom_cm: f64,
    default_bottom_cm: f64,
}

fn clock(world: &World) -> Clock {
    Clock {
        game_hour: advisor::game_hour(world),
        night: advisor::is_night(world),
        game_hours_per_real_hour: world.settings.fishing.game_hours_per_real_hour,
        night_start: advisor::NIGHT_START_HOUR,
        night_end: advisor::NIGHT_END_HOUR,
    }
}

/// What the pages name and look up: bases and their locations, fish, baits.
fn reference(world: &World) -> Reference {
    let settings = &world.settings;
    let bases: Vec<BaseRef> = settings
        .bases
        .iter()
        .map(|(id, base)| BaseRef {
            id: *id,
            name: base_name(world, *id),
            min_level: base.min_level,
            locations: base.locations.iter().filter(|location| **location != 0).map(|location| LocationRef { id: *location, name: location_name(world, *location) }).collect(),
        })
        .collect();
    let largest = world.fish.largest_share();
    let market = &settings.fish_market;
    let mut species: Vec<SpeciesRef> = world
        .fish
        .all()
        .map(|one| {
            let per_kg = market.base_price_per_kg * (largest / one.share).sqrt();
            let mut locations: Vec<i32> = world.fish.by_location.iter().filter(|(_, ids)| ids.contains(&one.id)).map(|(location, _)| *location).collect();
            locations.sort_unstable();
            SpeciesRef {
                id: one.id,
                name: one.name.clone(),
                small_grams: one.small_grams,
                large_grams: one.large_grams,
                trophy_grams: one.trophy_grams,
                top_grams: f64::from(one.trophy_grams) * settings.fishing.top_weight_factor,
                min_depth_cm: one.min_depth_cm,
                max_depth_cm: one.max_depth_cm,
                near_bottom: one.near_bottom,
                likes_depth: one.likes_depth,
                night_factor: one.night_factor,
                price_per_kg: [per_kg, per_kg * market.large_price_factor, per_kg * market.trophy_price_factor].map(|price| price.round() / 100.0),
                // Baits a fish of this species can stay on. The client lets
                // anything lighter than the bait's size go, so a bait even the
                // species' heaviest never reaches is no bait of theirs at all.
                baits: one
                    .baits
                    .iter()
                    .copied()
                    .filter(|bait| {
                        world.catalog.get(*bait).is_some_and(|item| fishing::bait_possible(&settings.fishing, &item.xml, one))
                    })
                    .collect(),
                bait_from_class: one
                    .baits
                    .iter()
                    .copied()
                    .filter_map(|bait| {
                        let item = world.catalog.get(bait)?;
                        let class = fishing::bait_from_class(&settings.fishing, &item.xml, one);
                        (class > 0 && class < 3).then_some((bait, class))
                    })
                    .collect(),
                locations,
            }
        })
        .collect();
    species.sort_by(|a, b| a.name.cmp(&b.name));
    let mut baits: Vec<BaitRef> = advisor::known_baits(world)
        .into_iter()
        .map(|item| BaitRef {
            id: item.id,
            name: item_name(world, item.id),
            kind: item.kind,
            price: item.cost as f64 / 100.0,
            pack: item.shop_count.max(1),
            level: advisor::level_of(item),
            sold_at: bases.iter().map(|base| base.id).filter(|base| advisor::buyable(world, *base, i32::MAX, item.id)).collect(),
        })
        .collect();
    baits.sort_by(|a, b| a.name.cmp(&b.name));
    Reference {
        bases,
        species,
        baits,
        clock: clock(world),
        bite_request_seconds: advisor::BITE_REQUEST_SECONDS,
        near_bottom_cm: settings.fishing.near_bottom_cm,
        default_bottom_cm: DEFAULT_BOTTOM_CM,
    }
}

/// Where the bait is taken to lie without a cast to measure the place.
const DEFAULT_BOTTOM_CM: f64 = 200.0;
/// Rows the advisor sends at most.
const MOST_ROWS: usize = 400;

#[derive(Clone, Copy, PartialEq)]
enum Scope {
    /// The baits the character carries.
    Mine,
    /// Those and what the base's shops sell the character.
    Shop,
    /// Every bait some fish takes.
    All,
}

#[derive(Clone, Copy, PartialEq)]
enum Goal {
    Money,
    Experience,
    Bites,
    Trophies,
}

struct AdviceQuery {
    character: Option<i64>,
    /// None: every base.
    base: Option<Option<i32>>,
    /// The phases to work out: day, night or both; None: as it is now.
    nights: Option<Vec<bool>>,
    bottom: f64,
    scope: Scope,
    /// Some(None): no tackle limit; Some(bag): that rod's; None: the best rod for the bait.
    tackle: Option<Option<i32>>,
    bait: Option<i32>,
    species: Option<i32>,
    goal: Goal,
    every_depth: bool,
}

fn advice_query(args: &HashMap<&str, &str>) -> AdviceQuery {
    let text = |key: &str| args.get(key).copied().unwrap_or("");
    let number = |key: &str| text(key).parse::<i64>().ok();
    AdviceQuery {
        character: number("character"),
        base: match text("base") {
            "all" => Some(None),
            other => other.parse().ok().map(Some),
        },
        nights: match text("time") {
            "day" => Some(vec![false]),
            "night" => Some(vec![true]),
            "both" => Some(vec![false, true]),
            _ => None,
        },
        bottom: text("bottom").parse::<f64>().ok().filter(|bottom| (10.0..=3000.0).contains(bottom)).unwrap_or(DEFAULT_BOTTOM_CM),
        scope: match text("scope") {
            "mine" => Scope::Mine,
            "all" => Scope::All,
            _ => Scope::Shop,
        },
        tackle: match text("tackle") {
            "none" => Some(None),
            other => other.parse().ok().map(Some),
        },
        bait: number("bait").map(|bait| bait as i32),
        species: number("species").map(|species| species as i32),
        goal: match text("goal") {
            "experience" => Goal::Experience,
            "bites" => Goal::Bites,
            "trophies" => Goal::Trophies,
            _ => Goal::Money,
        },
        every_depth: text("depths") == "all",
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ForecastRow {
    /// Depth and bottom come from the character's last cast there.
    measured: bool,
    /// No rod of the character's takes the bait; worked out without a limit.
    no_rod: bool,
    /// What the tackle holds, none without a limit.
    limit_grams: Option<f64>,
    #[serde(flatten)]
    forecast: Forecast,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Who {
    id: i64,
    name: String,
    level: i32,
    money: f64,
    vip: bool,
    online: bool,
    base: i32,
    location: i32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Advice {
    character: Who,
    rods: Vec<Rod>,
    /// Bait template to pieces carried.
    owned: BTreeMap<i32, i32>,
    /// The base worked out, none for every base.
    base: Option<i32>,
    nights: Vec<bool>,
    clock: Clock,
    rows: Vec<ForecastRow>,
}

fn score(goal: Goal, species: Option<i32>, forecast: &Forecast) -> f64 {
    let per_bite = match (species, goal) {
        (Some(species), _) => forecast.share_of(species),
        (None, Goal::Money) => forecast.money,
        (None, Goal::Experience) => forecast.experience,
        (None, Goal::Bites) => 1.0,
        (None, Goal::Trophies) => forecast.trophies,
    };
    forecast.chance * per_bite
}

async fn advise(world: &World, query: AdviceQuery) -> Result<String> {
    let id = match query.character {
        Some(id) => id,
        None => match world.live().first() {
            Some(live) => live.data.id,
            None => world.store.all_characters().await?.first().map(|summary| summary.id).ok_or_else(|| anyhow::anyhow!("no characters"))?,
        },
    };
    let live = world.live_one(id);
    let data = match &live {
        Some(live) => live.data.clone(),
        None => world.store.load_character(id).await?.ok_or_else(|| anyhow::anyhow!("no character {id}"))?,
    };
    let here = live.as_ref().map_or(1, |live| live.base).max(1);
    let base = query.base.unwrap_or(Some(here));
    let nights = query.nights.clone().unwrap_or_else(|| vec![advisor::is_night(world)]);
    let rods = advisor::rods(world, &data);
    let owned = advisor::baits_of(world, &data);
    let all_baits = advisor::known_baits(world);

    let bases: Vec<i32> = match base {
        Some(base) => vec![base],
        None => world.settings.bases.keys().copied().collect(),
    };
    let mut rows = Vec::new();
    for base in bases {
        let baits: Vec<(i32, i32)> = all_baits
            .iter()
            .filter(|item| query.bait.is_none_or(|wanted| wanted == item.id))
            .filter(|item| match query.scope {
                Scope::Mine => owned.contains_key(&item.id),
                Scope::Shop => owned.contains_key(&item.id) || advisor::buyable(world, base, data.level, item.id),
                Scope::All => true,
            })
            .map(|item| (item.id, item.kind))
            .collect();
        let locations = world.settings.bases.get(&base).map(|found| found.locations.clone()).unwrap_or_default();
        for location in locations.into_iter().filter(|location| *location != 0) {
            if query.species.is_some_and(|species| !world.fish.by_location.get(&location).is_some_and(|ids| ids.contains(&species))) {
                continue;
            }
            let cast = live.as_ref().filter(|live| live.base == base).and_then(|live| live.casts.get(&location)).copied();
            let bottom = cast.map_or(query.bottom, |cast| cast.bottom);
            let depths = advisor::depths(world, location, bottom);
            for (bait, bait_kind) in &baits {
                if !advisor::bites_at(world, location, *bait) {
                    continue;
                }
                let fitting: Vec<&Rod> = rods.iter().filter(|rod| advisor::takes(rod.kind, *bait_kind)).collect();
                let (tackle, no_rod) = match query.tackle {
                    Some(None) => (Tackle::NONE, false),
                    Some(Some(bag)) => match rods.iter().find(|rod| rod.bag == bag) {
                        Some(rod) if advisor::takes(rod.kind, *bait_kind) => (Tackle::of(rod), false),
                        _ => continue,
                    },
                    None => match fitting.iter().max_by(|a, b| a.limit_grams.unwrap_or(f64::INFINITY).total_cmp(&b.limit_grams.unwrap_or(f64::INFINITY))) {
                        Some(rod) => (Tackle::of(rod), false),
                        None => (Tackle::NONE, true),
                    },
                };
                let limit = tackle.landed.is_finite().then_some(tackle.landed);
                for night in &nights {
                    let mut found: Vec<Forecast> = depths
                        .iter()
                        .map(|depth| {
                            let spot = Spot { base, location, depth: *depth, bottom, night: *night };
                            advisor::forecast(world, &data, spot, *bait, tackle)
                        })
                        .filter(|forecast| forecast.chance > 0.0)
                        .filter(|forecast| query.species.is_none_or(|species| forecast.share_of(species) > 0.0))
                        .collect();
                    if !query.every_depth {
                        found.sort_by(|a, b| score(query.goal, query.species, b).total_cmp(&score(query.goal, query.species, a)));
                        found.truncate(1);
                    }
                    rows.extend(found.into_iter().map(|forecast| ForecastRow { measured: cast.is_some(), no_rod, limit_grams: limit, forecast }));
                }
            }
        }
    }
    rows.sort_by(|a, b| score(query.goal, query.species, &b.forecast).total_cmp(&score(query.goal, query.species, &a.forecast)));
    rows.truncate(MOST_ROWS);
    let advice = Advice {
        character: Who {
            id: data.id,
            name: data.name.clone(),
            level: data.level,
            money: data.money as f64 / 100.0,
            vip: data.vip,
            online: live.is_some(),
            base: here,
            location: live.as_ref().map_or(0, |live| live.location),
        },
        rods,
        owned,
        base,
        nights,
        clock: clock(world),
        rows,
    };
    Ok(serde_json::to_string(&advice)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_are_found_whatever_their_case() {
        let request = "POST /api/admin/do HTTP/1.1\r\nHost: x\r\nX-Admin-Token:  hunter2 \r\nContent-Length: 7\r\n\r\nbody...";
        assert_eq!(header(request, "x-admin-token"), Some("hunter2"));
        assert_eq!(header(request, "content-length"), Some("7"));
        assert_eq!(header(request, "authorization"), None);
        // Past the blank line it is the body, not a header.
        assert_eq!(header("GET / HTTP/1.1\r\n\r\nX-Admin-Token: no\r\n", "x-admin-token"), None);
    }

    #[test]
    fn an_admin_action_reads_as_the_client_writes_it() {
        use crate::world::AdminAction;
        let read = |text| serde_json::from_str::<AdminAction>(text).expect("the action");
        assert!(matches!(read(r#"{"do":"give","template":1180,"count":3}"#), AdminAction::Give { template: 1180, count: 3 }));
        // Count is optional and means one.
        assert!(matches!(read(r#"{"do":"give","template":1270}"#), AdminAction::Give { template: 1270, count: 1 }));
        assert!(matches!(read(r#"{"do":"experience","amount":64}"#), AdminAction::Experience { amount: 64 }));
        assert!(matches!(read(r#"{"do":"reputation","faction":1,"points":40}"#), AdminAction::Reputation { faction: 1, points: 40 }));
        assert!(matches!(read(r#"{"do":"finish_task","task":409}"#), AdminAction::FinishTask { task: 409 }));
        assert!(matches!(read(r#"{"do":"drop_task","task":409}"#), AdminAction::DropTask { task: 409 }));
        assert!(serde_json::from_str::<AdminAction>(r#"{"do":"delete_everything"}"#).is_err());
        assert!(serde_json::from_str::<AdminAction>(r#"{"do":"give"}"#).is_err(), "a template is required");
    }

    #[test]
    fn the_query_gives_its_numbers() {
        let (path, args) = query("/api/advisor?character=3&night=1");
        assert_eq!(path, "/api/advisor");
        assert_eq!(args.get("character"), Some(&"3"));
        assert_eq!(args.get("night"), Some(&"1"));
        assert_eq!(query("/").0, "/");
    }
}
