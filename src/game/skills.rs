//! Studying skills, after the player guides: a course («Курс ...») opens its
//! skill at level 1 and the skill's sub-skills at 0. One skill at a time is on
//! study and gets a share of all experience; a sub-skill never passes its
//! skill, no level goes above the top, and all levels together stay under a
//! limit that grows with the character. The points a level costs come from
//! the game's own tree (`data/skills.json`).

use std::collections::btree_map::Entry;

use anyhow::Result;

use super::{Conn, Outcome, Player, Session, active, inventory, perks};
use crate::afc::{AfcMessage, AfcWriter};
use crate::config::{SkillSettings, SkillTree};
use crate::messages as msg;
use crate::storage::{CharacterData, Skill};

/// What the client sends from the skills window when the limit is reached.
const OVER_LIMIT: i32 = -1;
/// A course's skill is the first number of its `<basePar>`.
const COURSE_KIND: i32 = 34;

pub fn points_to_next(tree: &SkillTree, skills: &SkillSettings, perk: i32, level: i32) -> f64 {
    let Some(node) = tree.get(perk) else {
        return 0.0;
    };
    if level >= skills.max_level {
        return 0.0;
    }
    node.cost[0] + node.cost[1] * (node.cost[2] + f64::from(level))
}

pub fn used(character: &CharacterData) -> i32 {
    character.skills.values().map(|skill| skill.level).sum()
}

pub fn limit(settings: &SkillSettings, level: i32) -> i32 {
    settings.limit_base + settings.limit_per_level * level
}

/// Puts a skill on study, or says why not with the client's own words.
fn choose(tree: &SkillTree, skills: &SkillSettings, character: &mut CharacterData, perk: i32) -> Result<(), &'static str> {
    let Some(skill) = character.skills.get(&perk) else {
        return Err("no_perk");
    };
    if skill.level >= skills.max_level {
        return Err("max_level");
    }
    if let Some(parent) = tree.get(perk).map(|node| node.parent).filter(|parent| *parent != 0) {
        let root = character.skills.get(&parent).map_or(0, |skill| skill.level);
        if skill.level >= root {
            return Err("low_root_level");
        }
    }
    character.studying = perk;
    Ok(())
}

/// Points to the skill on study. Returns the levels it reached; points past a
/// level it may not take yet wait at the threshold.
fn gain(tree: &SkillTree, skills: &SkillSettings, character: &mut CharacterData, points: f64) -> Vec<i32> {
    let perk = character.studying;
    let parent = tree.get(perk).map_or(0, |node| node.parent);
    let root = (parent != 0).then(|| character.skills.get(&parent).map_or(0, |skill| skill.level));
    let (mut used, limit) = (used(character), limit(skills, character.level));
    let max = skills.max_level;
    let Some(skill) = character.skills.get_mut(&perk) else {
        return Vec::new();
    };

    skill.experience += points;
    let mut reached = Vec::new();
    while skill.level < max {
        let need = points_to_next(tree, skills, perk, skill.level);
        if skill.experience < need {
            break;
        }
        if used >= limit || root.is_some_and(|root| skill.level >= root) {
            skill.experience = need;
            break;
        }
        skill.experience -= need;
        skill.level += 1;
        used += 1;
        reached.push(skill.level);
    }
    if skill.level >= max {
        skill.experience = 0.0;
        character.studying = 0;
    }
    reached
}

#[derive(Debug, PartialEq)]
enum Course {
    /// The skill is new: it and its sub-skills are now in the window.
    Opened(Vec<i32>),
    Points(f64),
    /// The course is spent for nothing, as in the original game.
    Nothing,
}

fn read_course(tree: &SkillTree, settings: &SkillSettings, character: &mut CharacterData, perk: i32) -> Course {
    if let Entry::Vacant(new) = character.skills.entry(perk) {
        new.insert(Skill { level: 1, experience: 0.0 });
        let mut opened = vec![perk];
        for child in tree.children(perk) {
            character.skills.entry(child.id).or_default();
            opened.push(child.id);
        }
        return Course::Opened(opened);
    }
    if character.studying == perk {
        return Course::Points(settings.course_experience);
    }
    let at_top = character.skills[&perk].level >= settings.max_level;
    if at_top && tree.get(character.studying).is_some_and(|node| node.parent == perk) {
        return Course::Points(settings.course_sub_experience);
    }
    Course::Nothing
}

fn tag_number(xml: &str, tag: &str) -> Option<i32> {
    let (_, rest) = xml.split_once(&format!("<{tag}>"))?;
    rest.split(['<', '/']).next()?.trim().parse().ok()
}

/// `<reqPerk>skill/level/0</reqPerk>`, if the course has one.
fn required_perk(xml: &str) -> Option<(i32, i32)> {
    let (_, rest) = xml.split_once("<reqPerk>")?;
    let mut fields = rest.split('<').next()?.split('/');
    Some((fields.next()?.trim().parse().ok()?, fields.next()?.trim().parse().ok()?))
}

/// Case 15: the character's named numbers, repeated string key and long value.
/// The skills window marks the one in `teech_perk` as being studied.
pub async fn send_study(conn: &Conn, player: &Player) -> Result<()> {
    let mut w = AfcWriter::new();
    w.string("teech_perk").int64(i64::from(player.data.studying));
    conn.send(msg::USER_PARAMS, w.finish()).await?;
    conn.log(&format!("   -> 15 teech_perk {}", player.data.studying));
    Ok(())
}

async fn refuse(conn: &Conn, text: &str) -> Result<Outcome> {
    let mut w = AfcWriter::new();
    w.string(text);
    conn.send(msg::PERK_STUDY, w.finish()).await?;
    conn.log(&format!("   -> 1405 {text}"));
    Ok(Outcome::Answered)
}

/// Client 1405: int perk from «Поставить на изучение», or -1 when the limit
/// is reached. Reply 1405 is an error string ("no_perk", "max_level",
/// "low_root_level"); on success the window comes again with the new mark.
pub async fn choose_study(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let perk = message.reader().int32()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    if perk == OVER_LIMIT {
        conn.log("   skills limit reached; the client has said so itself");
        return Ok(Outcome::Answered);
    }
    let settings = &conn.world.settings;
    if let Err(reason) = choose(&settings.skill_tree, &settings.skills, &mut player.data, perk) {
        return refuse(conn, reason).await;
    }
    conn.save(player).await?;
    send_study(conn, player).await?;
    perks::window(session).await
}

/// Client 275: long item, after the bar of «Изучить» on a course. The client
/// leaves the course in its bag, so the server takes it away.
pub async fn study_course(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let id = message.reader().int64()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let world = conn.world.clone();
    let settings = &world.settings;

    let Some(template) = player.data.items.iter().find(|item| item.id == id).map(|item| item.template) else {
        conn.log(&format!("   course: character has no item {id}"));
        return Ok(Outcome::Unanswered);
    };
    let Some(item) = world.catalog.get(template).filter(|item| item.kind == COURSE_KIND) else {
        conn.log(&format!("   course: item {template} is not a course"));
        return Ok(Outcome::Unanswered);
    };
    let Some(perk) = tag_number(&item.xml, "basePar").filter(|perk| settings.skill_tree.get(*perk).is_some()) else {
        conn.log(&format!("   course: item {template} names no skill"));
        return Ok(Outcome::Unanswered);
    };
    // The client has no words for these two; "низкий уровень корневого навыка" is the nearest.
    if tag_number(&item.xml, "minimalLevel").is_some_and(|level| player.data.level < level) {
        return refuse(conn, "low_root_level").await;
    }
    if required_perk(&item.xml).is_some_and(|(needed, level)| perks::level_of(conn, player, needed) < level) {
        return refuse(conn, "low_root_level").await;
    }

    inventory::use_piece(conn, player, id).await?;
    match read_course(&settings.skill_tree, &settings.skills, &mut player.data, perk) {
        Course::Opened(opened) => {
            conn.save(player).await?;
            perks::announce(conn, player, &opened[..1], true).await?;
            perks::announce(conn, player, &opened[1..], false).await?;
            perks::send(conn, player).await?;
        }
        Course::Points(points) => {
            conn.log(&format!("   course {template}: +{points} points to skill {}", player.data.studying));
            give(conn, player, points).await?;
        }
        Course::Nothing => conn.log(&format!("   course {template}: skill {perk} is known and not on study")),
    }
    Ok(Outcome::Answered)
}

/// Skill points to whatever is on study, telling the client what changed.
pub async fn give(conn: &Conn, player: &mut Player, points: f64) -> Result<()> {
    let perk = player.data.studying;
    if perk == 0 || points <= 0.0 {
        return Ok(());
    }
    let settings = &conn.world.settings;
    let reached = gain(&settings.skill_tree, &settings.skills, &mut player.data, points);
    conn.save(player).await?;
    if reached.is_empty() {
        return perks::announce(conn, player, &[perk], false).await;
    }
    perks::announce(conn, player, &[perk], true).await?;
    perks::send(conn, player).await?;
    if player.data.studying != perk {
        send_study(conn, player).await?;
    }
    Ok(())
}

/// The share of an experience gain that goes to the skill on study.
pub fn share(settings: &SkillSettings, experience: i64) -> f64 {
    experience as f64 * settings.experience_percent / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SkillNode;

    struct Rules {
        skill_tree: SkillTree,
        skills: SkillSettings,
    }

    fn settings() -> Rules {
        let node = |id, parent| SkillNode { id, parent, effect: [1.0, 0.0, 0.0], cost: [100.0, 100.0, 0.0] };
        Rules {
            skill_tree: SkillTree::new(vec![node(100, 0), node(101, 100), node(140, 0)]),
            skills: SkillSettings {
                experience_percent: 10.0,
                course_experience: 300.0,
                course_sub_experience: 100.0,
                max_level: 5,
                limit_base: 2,
                limit_per_level: 1,
            },
        }
    }

    fn reader(level: i32) -> CharacterData {
        CharacterData { level, ..Default::default() }
    }

    #[test]
    fn a_course_opens_the_skill_and_its_sub_skills() {
        let settings = settings();
        let mut character = reader(10);
        let course = read_course(&settings.skill_tree, &settings.skills, &mut character, 100);
        assert_eq!(course, Course::Opened(vec![100, 101]));
        assert_eq!(character.skills[&100].level, 1);
        assert_eq!(character.skills[&101].level, 0);
        assert_eq!(used(&character), 1);
    }

    #[test]
    fn a_second_course_helps_only_the_skill_on_study() {
        let settings = settings();
        let mut character = reader(10);
        read_course(&settings.skill_tree, &settings.skills, &mut character, 100);
        assert_eq!(read_course(&settings.skill_tree, &settings.skills, &mut character, 100), Course::Nothing);
        character.studying = 100;
        assert_eq!(read_course(&settings.skill_tree, &settings.skills, &mut character, 100), Course::Points(300.0));
        character.skills.get_mut(&100).unwrap().level = 5;
        character.studying = 101;
        assert_eq!(read_course(&settings.skill_tree, &settings.skills, &mut character, 100), Course::Points(100.0));
    }

    #[test]
    fn a_sub_skill_waits_for_its_skill() {
        let settings = settings();
        let mut character = reader(10);
        read_course(&settings.skill_tree, &settings.skills, &mut character, 100);
        assert_eq!(choose(&settings.skill_tree, &settings.skills, &mut character, 101), Ok(()));
        // Level 0 to 1 costs 100; 1 to 2 would pass the skill at 1.
        assert_eq!(gain(&settings.skill_tree, &settings.skills, &mut character, 450.0), vec![1]);
        assert_eq!(character.skills[&101], Skill { level: 1, experience: 200.0 });
        assert_eq!(choose(&settings.skill_tree, &settings.skills, &mut character, 101), Err("low_root_level"));
    }

    #[test]
    fn points_carry_over_levels_until_the_top() {
        let settings = settings();
        let mut character = reader(30);
        read_course(&settings.skill_tree, &settings.skills, &mut character, 100);
        character.studying = 100;
        // 1 to 2 costs 200, 2 to 3 300, 3 to 4 400, 4 to 5 500.
        assert_eq!(gain(&settings.skill_tree, &settings.skills, &mut character, 450.0), vec![2]);
        assert_eq!(character.skills[&100].experience, 250.0);
        assert_eq!(gain(&settings.skill_tree, &settings.skills, &mut character, 5000.0), vec![3, 4, 5]);
        assert_eq!(character.studying, 0);
        assert_eq!(choose(&settings.skill_tree, &settings.skills, &mut character, 100), Err("max_level"));
    }

    #[test]
    fn the_limit_holds_levels_back() {
        let settings = settings();
        // Level 0: the limit is 2 levels in all.
        let mut character = reader(0);
        read_course(&settings.skill_tree, &settings.skills, &mut character, 100);
        character.studying = 100;
        assert_eq!(gain(&settings.skill_tree, &settings.skills, &mut character, 1000.0), vec![2]);
        assert_eq!(gain(&settings.skill_tree, &settings.skills, &mut character, 1000.0), Vec::<i32>::new());
        assert_eq!(character.skills[&100], Skill { level: 2, experience: 300.0 });
    }

    #[test]
    fn unknown_skills_cannot_be_studied() {
        let settings = settings();
        assert_eq!(choose(&settings.skill_tree, &settings.skills, &mut reader(1), 140), Err("no_perk"));
    }

    #[test]
    fn courses_name_their_skill_and_needs() {
        let xml = "<basePar>120/0/0</basePar><minimalLevel>10</minimalLevel><reqPerk>110/2/0</reqPerk>";
        assert_eq!(tag_number(xml, "basePar"), Some(120));
        assert_eq!(tag_number(xml, "minimalLevel"), Some(10));
        assert_eq!(required_perk(xml), Some((110, 2)));
        assert_eq!(required_perk("<req />"), None);
    }
}
