//! Builds `data/recipes.json` from the player's own copy of the game, for the
//! same reason as `items.rs`.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use regex::Regex;
use regex::bytes::Regex as BytesRegex;
use serde::Serialize;

#[derive(Debug, PartialEq, Serialize)]
struct Recipe {
    id: i32,
    station: i32,
    show: i32,
    category: i32,
    kind: i32,
    default: bool,
    result: [f64; 3],
    cost: [f64; 3],
    time: i32,
    crit_chance: f64,
    crit_result: [f64; 3],
    profession: i32,
    profession_level: i32,
    ingredients: Vec<[f64; 6]>,
    max_parts: i32,
    components: [[f64; 3]; 4],
    passive: [f64; 3],
    kg_count: [f64; 3],
}

/// The fields of `a/b/c`; the game leaves unset ones out.
fn numbers<const N: usize>(text: &str) -> [f64; N] {
    let mut values = [0.0; N];
    for (value, field) in values.iter_mut().zip(text.split('/')) {
        *value = field.trim().replace(',', ".").parse().unwrap_or(0.0);
    }
    values
}

fn craft_text(assets: &[u8]) -> Result<String> {
    let craft = BytesRegex::new(r"(?s)<craft>.*?</craft>").expect("valid pattern");
    let Some(found) = craft.find_iter(assets).find(|found| found.as_bytes().windows(7).any(|w| w == b"<stanok")) else {
        bail!("в resources.assets нет рецептов");
    };
    let comments = Regex::new(r"(?s)<!--.*?-->").expect("valid pattern");
    Ok(comments.replace_all(&String::from_utf8_lossy(found.as_bytes()), "").into_owned())
}

struct Tags {
    result: Regex,
    cost: Regex,
    time: Regex,
    crit_chance: Regex,
    crit_result: Regex,
    profession: Regex,
    profession_level: Regex,
    show: Regex,
    ingredient: Regex,
    max_parts: Regex,
    components: [Regex; 4],
    passive: Regex,
    kg_count: Regex,
}

impl Tags {
    fn new() -> Self {
        let tag = |name: &str| Regex::new(&format!(r"(?s)<{name}>(.*?)</{name}>")).expect("valid pattern");
        Self {
            result: tag("result_id"),
            cost: tag("craft_cost"),
            time: tag("craft_time"),
            crit_chance: tag("krit_shance"),
            crit_result: tag("result_id_krit"),
            profession: tag("req_prof"),
            profession_level: tag("profy_minlevel"),
            show: tag("showid"),
            ingredient: tag("ingr"),
            max_parts: tag("maxres"),
            components: [tag("componentA"), tag("componentB"), tag("componentC"), tag("componentD")],
            passive: tag("passive"),
            kg_count: tag("kgcount"),
        }
    }
}

fn found<'a>(regex: &Regex, text: &'a str) -> &'a str {
    regex.captures(text).and_then(|found| found.get(1)).map_or("", |found| found.as_str())
}

fn int(regex: &Regex, text: &str) -> i32 {
    found(regex, text).trim().parse().unwrap_or(0)
}

fn extract(assets: &[u8]) -> Result<Vec<Recipe>> {
    let station = Regex::new(r#"(?s)<stanok id="(\d+)"[^>]*>(.*?)</stanok>"#).expect("valid pattern");
    let recipe = Regex::new(r"(?s)<recept\b([^>]*)>(.*?)</recept>").expect("valid pattern");
    let attribute = |name: &str| Regex::new(&format!(r#"\b{name}="(\d+)""#)).expect("valid pattern");
    let (id, category, kind) = (attribute("id"), attribute("category"), attribute("type"));
    let tags = Tags::new();

    let text = craft_text(assets)?;
    let mut recipes = Vec::new();
    for bench in station.captures_iter(&text) {
        let station_id: i32 = bench[1].parse().context("номер станка")?;
        for entry in recipe.captures_iter(&bench[2]) {
            let (attributes, body) = (&entry[1], &entry[2]);
            recipes.push(Recipe {
                id: int(&id, attributes),
                station: station_id,
                show: int(&tags.show, body),
                category: int(&category, attributes),
                kind: int(&kind, attributes),
                default: body.contains("<isDefault>1</isDefault>"),
                result: numbers(found(&tags.result, body)),
                cost: numbers(found(&tags.cost, body)),
                time: int(&tags.time, body),
                crit_chance: found(&tags.crit_chance, body).trim().parse().unwrap_or(0.0),
                crit_result: numbers(found(&tags.crit_result, body)),
                profession: int(&tags.profession, body),
                profession_level: int(&tags.profession_level, body),
                ingredients: tags.ingredient.captures_iter(body).map(|one| numbers(&one[1])).collect(),
                max_parts: int(&tags.max_parts, body),
                components: tags.components.each_ref().map(|tag| numbers(found(tag, body))),
                passive: numbers(found(&tags.passive, body)),
                kg_count: numbers(found(&tags.kg_count, body)),
            });
        }
    }
    Ok(recipes)
}

/// A file without this field was made by an older version and is made again.
const CURRENT_FIELD: &str = "\"max_parts\"";

/// Does nothing once the file exists: it only depends on the installed game.
pub fn ensure_extracted(game: &Path, data_dir: &Path) -> Result<()> {
    let output = data_dir.join("recipes.json");
    if fs::read_to_string(&output).is_ok_and(|text| text.contains(CURRENT_FIELD)) {
        return Ok(());
    }

    let assets_path = game.join("AFCGame_Data").join("resources.assets");
    let assets = fs::read(&assets_path)
        .with_context(|| format!("не удалось прочитать {}", assets_path.display()))?;
    let recipes = extract(&assets)?;

    fs::create_dir_all(data_dir)?;
    fs::write(&output, serde_json::to_string(&recipes)?)?;
    println!("Рецепты извлечены из игры: {}.", recipes.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASSETS: &[u8] = br#"junk<craft><improve/><stanok id="3">
        <recept id="142" category="400"><!--<isDefault>1</isDefault>-->
            <result_id>1012/20/0</result_id>
            <ingredients><ingr>1035/1/0</ingr><ingr>0/2/1/300/3000</ingr></ingredients>
            <craft_time>5</craft_time><craft_cost>1/0/5</craft_cost>
            <result_id_krit>1012/30/0</result_id_krit><krit_shance>10</krit_shance>
        </recept></stanok><stanok id="4">
        <recept id="486" category="510" type="1"><result_id>1335/1/0</result_id><maxres>12</maxres>
            <componentA>1153/1/0.3</componentA> # comment <componentB>1154/1/0.2</componentB>
            <!--<componentD>1086/2/10</componentD>--><passive>1334/1/0</passive>
            <craft_cost>2700/0/500</craft_cost></recept>
        <recept id="315" category="150"><result_id>1257/1/0</result_id><kgcount>1000/3500/0</kgcount></recept>
        </stanok><stanok id="1000">
        <recept id="7" category="50" type="1"><isDefault>1</isDefault><showid>3</showid>
            <result_id>1/1/0</result_id><craft_cost>0/0/0</craft_cost></recept>
        </stanok></craft>junk"#;

    #[test]
    fn extracts_every_field() {
        let recipes = extract(ASSETS).unwrap();
        assert_eq!(recipes.len(), 4);
        assert_eq!(recipes[0], Recipe {
            id: 142,
            station: 3,
            show: 0,
            category: 400,
            kind: 0,
            default: false,
            result: [1012.0, 20.0, 0.0],
            cost: [1.0, 0.0, 5.0],
            time: 5,
            crit_chance: 10.0,
            crit_result: [1012.0, 30.0, 0.0],
            profession: 0,
            profession_level: 0,
            ingredients: vec![[1035.0, 1.0, 0.0, 0.0, 0.0, 0.0], [0.0, 2.0, 1.0, 300.0, 3000.0, 0.0]],
            max_parts: 0,
            components: [[0.0; 3]; 4],
            passive: [0.0; 3],
            kg_count: [0.0; 3],
        });
    }

    #[test]
    fn extracts_the_constructor_and_the_weight_count() {
        let recipes = extract(ASSETS).unwrap();
        let built = &recipes[1];
        assert_eq!((built.id, built.kind, built.max_parts), (486, 1, 12));
        assert_eq!(built.components, [[1153.0, 1.0, 0.3], [1154.0, 1.0, 0.2], [0.0; 3], [0.0; 3]]);
        assert_eq!(built.passive, [1334.0, 1.0, 0.0]);
        assert_eq!(recipes[2].kg_count, [1000.0, 3500.0, 0.0]);
    }

    #[test]
    fn reads_the_optional_marks() {
        let recipes = extract(ASSETS).unwrap();
        assert_eq!((recipes[3].station, recipes[3].show, recipes[3].kind, recipes[3].default), (1000, 3, 1, true));
    }

    #[test]
    fn assets_without_recipes_are_an_error() {
        assert!(extract(b"nothing <craft></craft>").is_err());
    }
}
