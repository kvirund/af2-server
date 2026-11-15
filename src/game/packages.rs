//! Items used with «Использовать» (wtype 30, and tokens of wtype 27): boxes,
//! packages, recipe collections, implants, trips, effect cards. The client
//! shows the bar and then sends 275 with the item, the same as for a course.
//! What each one does is its own use script in the client's item table
//! (`scripts.rs`): the server runs that, not a list of its own.

use anyhow::Result;
use rand::random;

use super::{Conn, Outcome, Player, character, chat, crafting, effects, inventory, perks, scripts, travel, vip, vitals};
use super::scripts::Action;
use crate::afc::AfcWriter;
use crate::messages as msg;

/// «Параметры»: its level and its four sub-skills' are what the reset course
/// gives back.
const PARAMETERS: i32 = 500;
/// The quest value that adds to the skills limit (`gu.nav_point`).
pub const PARAMETER_POINTS: &str = "nav_point";

/// An item to give: template, pieces, param, and whether the client should
/// show it in its "you got" window (case 1704), as the scripts that pick do.
type Gift = (i32, i32, i32, bool);

/// Items first, so that nothing is spent when they do not fit.
fn gifts(actions: &[Action], roll: &mut impl FnMut() -> f64) -> Vec<Gift> {
    let mut out = Vec::new();
    for action in actions {
        match action {
            Action::Give { item, count } => out.push((*item, (*count).max(1), 0, false)),
            Action::Pick { choices, card } => {
                let Some(choice) = scripts::pick(choices, roll()) else { continue };
                let count = if choice.max > 0 { choice.min + ((roll() * f64::from(choice.max - choice.min + 1)) as i32).min(choice.max - choice.min) } else { 1 };
                match card {
                    Some((item, species)) if *item == choice.item && !species.is_empty() => {
                        let at = ((roll() * species.len() as f64) as usize).min(species.len() - 1);
                        out.push((choice.item, 1, species[at], true));
                    }
                    _ => out.push((choice.item, count.max(1), 0, true)),
                }
            }
            _ => {}
        }
    }
    out
}

fn name_of(conn: &Conn, template: i32, param: i32) -> String {
    let name = conn.world.catalog.get(template).map_or_else(|| template.to_string(), |item| item.name.clone());
    let species = conn.world.fish.get(param).map_or("?", |species| species.name.as_str());
    name.replace("{0}", species)
}

async fn show_item(conn: &Conn, id: i64) -> Result<()> {
    let mut w = AfcWriter::new();
    w.int64(id);
    conn.send(msg::ITEM_RECEIVED, w.finish()).await
}

/// Runs one action that is not an item. Returns a line for the chat.
async fn run(conn: &Conn, player: &mut Player, action: &Action) -> Result<Option<String>> {
    let world = conn.world.clone();
    let done = match action {
        Action::Give { .. } | Action::Pick { .. } => None,
        Action::TeachRecipe { recipe, uses } => {
            match world.recipes.iter().find(|one| one.id == *recipe) {
                Some(found) if !found.default => {
                    crafting::add_uses(&mut player.data.known_recipes, *recipe, *uses);
                    conn.save(player).await?;
                    Some(format!("рецепт {recipe}"))
                }
                _ => {
                    conn.log(&format!("   recipe {recipe} is unknown or known to all"));
                    None
                }
            }
        }
        Action::Effect { effect, force, seconds } => effects::start(conn, player, *effect, *force, *seconds).await?,
        Action::Experience(amount) => {
            character::award_experience(conn, player, *amount as i32, 1.0).await?;
            Some(format!("{amount} опыта"))
        }
        Action::VipDays(days) => {
            vip::grant(conn, player, *days, 0).await?;
            Some(format!("VIP-премиум продлён на {days} дн."))
        }
        Action::WorkPoints(amount) => {
            let most = world.settings.vitals.max_work_points;
            if let Some(now) = player.data.vitals.as_mut() {
                now.work_points = (now.work_points + *amount as f32).min(most.max(now.work_points));
            }
            conn.save(player).await?;
            vitals::send(conn, player).await?;
            Some(format!("{amount} ОР"))
        }
        Action::Premium(amount) => {
            player.data.premium += amount;
            conn.save(player).await?;
            character::send_money(conn, player, 0).await?;
            Some(format!("{amount} эргов"))
        }
        Action::ParameterPoints(points) => {
            *player.data.quest_values.entry(PARAMETER_POINTS.into()).or_insert(0) += points;
            conn.save(player).await?;
            perks::send(conn, player).await?;
            Some(format!("+{points} к пределу навыков"))
        }
        Action::ResetParameters => {
            let returned = reset_parameters(&world.settings.skill_tree, &mut player.data);
            conn.save(player).await?;
            perks::send(conn, player).await?;
            Some(format!("параметры сброшены, возвращено {returned} ур."))
        }
        Action::Travel { base, location, timed } => {
            if *timed {
                conn.log("   timed trip: the server keeps no time limit, the base is open anyway");
            }
            travel::teleport(conn, player, *base, *location).await?;
            None
        }
        Action::BaseValue { prefix } => {
            let base = travel::current_base(&world.settings, player);
            *player.data.quest_values.entry(format!("{prefix}{base}")).or_insert(0) += 1;
            conn.save(player).await?;
            Some(format!("{prefix}{base} +1"))
        }
        Action::Unknown(call) => {
            conn.log(&format!("   script call {call} is not known to the server"));
            None
        }
    };
    Ok(done)
}

/// Sets «Параметры» back to level 1 and its sub-skills to 0. Returns the
/// levels freed under the limit.
fn reset_parameters(tree: &crate::config::SkillTree, character: &mut crate::storage::CharacterData) -> i32 {
    let mut freed = 0;
    let children: Vec<i32> = tree.children(PARAMETERS).map(|node| node.id).collect();
    for (perk, skill) in character.skills.iter_mut() {
        let start = if *perk == PARAMETERS { 1 } else if children.contains(perk) { 0 } else { continue };
        freed += (skill.level - start).max(0);
        skill.level = skill.level.min(start);
        skill.experience = 0.0;
    }
    freed
}

/// Client 275 for an item with a use script. `None` when it has none.
pub async fn open(conn: &Conn, player: &mut Player, id: i64, template: i32) -> Result<Option<Outcome>> {
    let world = conn.world.clone();
    let Some(item) = world.catalog.get(template) else {
        return Ok(None);
    };
    let Some(script) = scripts::script_of(&item.xml) else {
        return Ok(None);
    };
    let actions = scripts::parse(&script, &item.xml);
    let title = item.name.clone();
    if actions.is_empty() {
        conn.log(&format!("   item {template}: its script does nothing the server knows"));
        return Ok(None);
    }

    let mut roll = random::<f64>;
    let out = gifts(&actions, &mut roll);
    // Everything has to fit before the item is spent; the item's own cell is
    // not counted, so a nearly full page may refuse what would just fit.
    for (inside, count, param, _) in &out {
        let fits = world.catalog.get(*inside).is_some_and(|item| {
            inventory::plan_pieces_of(&world.settings.bags, &player.data, item, *count, *param).is_ok()
        });
        if !fits {
            chat::system(conn, &format!("{title}: содержимое не помещается в рюкзак.")).await?;
            conn.log(&format!("   item {template}: {inside} x{count} does not fit or is not in the catalog"));
            return Ok(Some(Outcome::Answered));
        }
    }
    inventory::use_piece(conn, player, id).await?;

    let mut got = Vec::new();
    for (inside, count, param, show) in out {
        let Some(item) = world.catalog.get(inside) else { continue };
        match inventory::give_of(conn, player, item, count, param).await? {
            Ok(changed) => {
                inventory::send_updates(conn, player.data.id, &changed).await?;
                if let Some(piece) = changed.iter().find(|entry| entry.template == inside).filter(|_| show) {
                    show_item(conn, piece.id).await?;
                }
                let name = name_of(conn, inside, param);
                got.push(if count > 1 { format!("{name} x{count}") } else { name });
            }
            Err(error) => conn.log(&format!("   item {inside} from {template} lost: {}", error.status())),
        }
    }
    conn.save(player).await?;
    let mut recipes = 0;
    for action in &actions {
        if let Some(line) = run(conn, player, action).await? {
            if matches!(action, Action::TeachRecipe { .. }) {
                recipes += 1;
            } else {
                got.push(line);
            }
        }
    }
    if recipes > 0 {
        got.push(format!("изучено рецептов: {recipes}"));
    }
    if !got.is_empty() {
        chat::system(conn, &format!("{title}: {}.", got.join(", "))).await?;
    }
    conn.log(&format!("   -> item {template} used: {}", got.join(", ")));
    Ok(Some(Outcome::Answered))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{SkillNode, SkillTree};
    use crate::storage::{CharacterData, Skill};
    use scripts::Choice;

    #[test]
    fn fixed_items_come_first_and_quietly() {
        let actions = vec![Action::Give { item: 1153, count: 3 }, Action::Experience(5), Action::Give { item: 1154, count: 1 }];
        assert_eq!(gifts(&actions, &mut || 0.5), vec![(1153, 3, 0, false), (1154, 1, 0, false)]);
    }

    #[test]
    fn a_pick_gives_a_count_or_a_fish_card() {
        let choices = vec![Choice { item: 1308, min: 10, max: 20, weight: 1.0 }, Choice { item: 1309, min: 0, max: 0, weight: 1.0 }];
        let pick = vec![Action::Pick { choices: choices.clone(), card: Some((1309, vec![77, 78])) }];
        let mut rolls = [0.1, 0.99].into_iter();
        assert_eq!(gifts(&pick, &mut || rolls.next().unwrap()), vec![(1308, 20, 0, true)]);
        let mut rolls = [0.9, 0.6].into_iter();
        assert_eq!(gifts(&pick, &mut || rolls.next().unwrap()), vec![(1309, 1, 78, true)]);
    }

    #[test]
    fn the_reset_frees_the_parameters() {
        let node = |id, parent| SkillNode { id, parent, effect: [1.0, 0.0, 0.0], cost: [100.0, 100.0, 0.0] };
        let tree = SkillTree::new(vec![node(500, 0), node(510, 500), node(520, 500), node(100, 0)]);
        let mut character = CharacterData::default();
        for (perk, level) in [(500, 3), (510, 2), (520, 0), (100, 4)] {
            character.skills.insert(perk, Skill { level, experience: 10.0 });
        }
        assert_eq!(reset_parameters(&tree, &mut character), 4);
        assert_eq!(character.skills[&500].level, 1);
        assert_eq!(character.skills[&510].level, 0);
        assert_eq!(character.skills[&100].level, 4);
    }
}
