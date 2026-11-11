//! Builds `data/skills.json` from the player's own copy of the game, for the
//! same reason as `items.rs`: the tree of skills, which sub-skill belongs to
//! which skill, what a level gives and what it costs.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use regex::Regex;
use regex::bytes::Regex as BytesRegex;
use serde::Serialize;

#[derive(Debug, PartialEq, Serialize)]
struct Skill {
    id: i32,
    /// 0 for a main skill.
    parent: i32,
    /// `val`: what one level gives.
    effect: [f64; 3],
    /// `tech`: points to the next level are a + b * (c + level).
    cost: [f64; 3],
}

fn numbers(text: &str) -> [f64; 3] {
    let mut values = [0.0; 3];
    for (value, field) in values.iter_mut().zip(text.split('/')) {
        *value = field.trim().replace(',', ".").parse().unwrap_or(0.0);
    }
    values
}

fn tree_text(assets: &[u8]) -> Result<String> {
    let perks = BytesRegex::new(r"(?s)<perks>.*?</perks>").expect("valid pattern");
    let Some(found) = perks.find_iter(assets).find(|found| found.as_bytes().windows(6).any(|w| w == b"<rperk")) else {
        bail!("в resources.assets нет дерева навыков");
    };
    Ok(String::from_utf8_lossy(found.as_bytes()).into_owned())
}

fn extract(assets: &[u8]) -> Result<Vec<Skill>> {
    let node = Regex::new(r"<(/?)(rperk|perk)\b([^>]*?)(/?)>").expect("valid pattern");
    let attribute = |name: &str| Regex::new(&format!(r#"\b{name}="([^"]*)""#)).expect("valid pattern");
    let (id, effect, cost) = (attribute("id"), attribute("val"), attribute("tech"));
    let value = |regex: &Regex, text: &str| regex.captures(text).map_or(String::new(), |found| found[1].to_owned());

    let text = tree_text(assets)?;
    let mut skills = Vec::new();
    let mut open_root = 0;
    for tag in node.captures_iter(&text) {
        let (closing, name, attributes, self_closing) = (&tag[1] == "/", &tag[2], &tag[3], &tag[4] == "/");
        if closing {
            if name == "rperk" {
                open_root = 0;
            }
            continue;
        }
        let skill_id: i32 = value(&id, attributes).parse().context("номер навыка")?;
        let parent = if name == "rperk" { 0 } else { open_root };
        skills.push(Skill {
            id: skill_id,
            parent,
            effect: numbers(&value(&effect, attributes)),
            cost: numbers(&value(&cost, attributes)),
        });
        if name == "rperk" && !self_closing {
            open_root = skill_id;
        }
    }
    Ok(skills)
}

/// Does nothing once the file exists: it only depends on the installed game.
pub fn ensure_extracted(game: &Path, data_dir: &Path) -> Result<()> {
    let output = data_dir.join("skills.json");
    if output.exists() {
        return Ok(());
    }

    let assets_path = game.join("AFCGame_Data").join("resources.assets");
    let assets = fs::read(&assets_path)
        .with_context(|| format!("не удалось прочитать {}", assets_path.display()))?;
    let skills = extract(&assets)?;

    fs::create_dir_all(data_dir)?;
    fs::write(&output, serde_json::to_string(&skills)?)?;
    println!("Навыки извлечены из игры: {}.", skills.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASSETS: &[u8] = br#"junk<perks><perk id="1" name="localised"/></perks>junk<?xml version="1.0"?>
        <perks>
        <rperk id="100" name="A" val="1/0/0" tech="1000/1000/1">
            <perk id="101" name="B" val="3/0/0" tech="500/500/2"/>
        </rperk>
        <rperk id="140" name="C" val="2/0/0" tech="1000/2/1"/>
        <perk id="9" val="1/0/0" tech="1/1/1"/>
        </perks>junk"#;

    #[test]
    fn sub_skills_name_their_parent() {
        let skills = extract(ASSETS).unwrap();
        let parents: Vec<(i32, i32)> = skills.iter().map(|skill| (skill.id, skill.parent)).collect();
        assert_eq!(parents, vec![(100, 0), (101, 100), (140, 0), (9, 0)]);
        assert_eq!(skills[1].effect, [3.0, 0.0, 0.0]);
        assert_eq!(skills[1].cost, [500.0, 500.0, 2.0]);
    }

    #[test]
    fn assets_without_the_tree_are_an_error() {
        assert!(extract(b"<perks><perk id=\"1\"/></perks>").is_err());
    }
}
