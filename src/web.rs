//! The server's web pages, on their own port (`server.webPort`, 0: none) so
//! access to them can be limited apart from the game's: who is in play, every
//! character, and a fishing advisor that works the odds out the game's way.
//!
//! Plain HTTP/1.1, GET only, one request per connection: `/` is the page,
//! `/api/online`, `/api/characters` and `/api/advisor?character=ID&base=N&night=0|1`
//! are its JSON.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::game::advisor::{self, Forecast};
use crate::game::{LastCast, death, keepnet};
use crate::storage::CharacterData;
use crate::world::World;

const PAGE: &str = include_str!("web/index.html");
const MOST_REQUEST_BYTES: usize = 16 * 1024;
const JSON: &str = "application/json; charset=utf-8";

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
    let head = String::from_utf8_lossy(&request);
    let mut words = head.lines().next().unwrap_or("").split(' ');
    let (status, kind, body) = match (words.next(), words.next()) {
        (Some("GET"), Some(target)) => route(world, target).await,
        _ => ("405 Method Not Allowed", "text/plain; charset=utf-8", "GET only".to_string()),
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
        "/api/advisor" => {
            let number = |key: &str| args.get(key).and_then(|value| value.parse::<i64>().ok());
            match number("character") {
                Some(id) => json(advise(world, id, number("base").map(|base| base as i32), number("night").map(|night| night != 0)).await),
                None => ("400 Bad Request", "text/plain; charset=utf-8", "character=ID".into()),
            }
        }
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
struct ForecastRow {
    location_name: String,
    bait_name: String,
    #[serde(flatten)]
    forecast: Forecast,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Advice {
    character: String,
    online: bool,
    base: i32,
    base_name: String,
    bases: Vec<(i32, String)>,
    night: bool,
    rows: Vec<ForecastRow>,
}

async fn advise(world: &World, id: i64, base: Option<i32>, night: Option<bool>) -> Result<String> {
    let live = world.live_one(id);
    let data = match &live {
        Some(live) => live.data.clone(),
        None => world.store.load_character(id).await?.ok_or_else(|| anyhow::anyhow!("no character {id}"))?,
    };
    let base = base.or(live.as_ref().map(|live| live.base).filter(|base| *base > 0)).unwrap_or(1);
    let night = night.unwrap_or_else(|| advisor::is_night(world));
    let baits = advisor::baits_of(world, &data);
    let locations = world.settings.bases.get(&base).map(|found| found.locations.clone()).unwrap_or_default();
    let mut rows = Vec::new();
    for location in locations.into_iter().filter(|location| *location != 0) {
        let cast = live.as_ref().and_then(|live| live.casts.get(&location));
        for bait in &baits {
            let forecast = advisor::forecast(world, &data, base, location, *bait, cast, night);
            if forecast.chance > 0.0 {
                rows.push(ForecastRow { location_name: location_name(world, location), bait_name: item_name(world, *bait), forecast });
            }
        }
    }
    let advice = Advice {
        character: data.name.clone(),
        online: live.is_some(),
        base,
        base_name: base_name(world, base),
        bases: world.settings.bases.keys().map(|base| (*base, base_name(world, *base))).collect(),
        night,
        rows,
    };
    Ok(serde_json::to_string(&advice)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_query_gives_its_numbers() {
        let (path, args) = query("/api/advisor?character=3&night=1");
        assert_eq!(path, "/api/advisor");
        assert_eq!(args.get("character"), Some(&"3"));
        assert_eq!(args.get("night"), Some(&"1"));
        assert_eq!(query("/").0, "/");
    }
}
