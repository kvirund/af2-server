//! Recipes. Stations list the recipes the character can use, a recipe item
//! teaches one, and a station makes things from items and fish.

use anyhow::Result;
use rand::random;

use std::collections::HashMap;

use super::{Outcome, Session, active, character, inventory, keepnet, perks, vitals};
use crate::afc::{AfcMessage, AfcWriter};
use crate::config::{CatalogItem, Recipe};
use crate::messages as msg;
use crate::storage::{CaughtFish, CharacterData, ItemEntry};

const RECIPE_KIND: i32 = 28;
/// The kind of `<recept>` the client lists with the fields of `row`. The other
/// one, assembled from parts, needs the constructor.
const SIMPLE_RECIPE: i32 = 0;
/// Item-kind ingredients; the other value means a fish of a given weight.
const ITEM_INGREDIENT: f64 = 0.0;
/// Bags below this are the inventory pages; the client counts only those.
const FIRST_NON_INVENTORY_BAG: i32 = 50;
/// A fish card: an item ingredient of it names the species in its fourth
/// number, and the piece keeps the species in its `param`.
pub const FISH_CARD: i32 = 1309;
/// Species in a fish ingredient: any ordinary fish, or any mutant.
const ANY_FISH: i32 = 0;
const UNLIMITED: i32 = -1;
/// «Ремесло» adds its `val` per level to the critical chance in points,
/// «Опыт ремесла» takes its `val` per level in percent off the work points.
const CRAFT: i32 = 130;
const CRAFT_EXPERIENCE: i32 = 132;
/// «Полевая переработка рыбы»: more of what the cutting table gives.
const FIELD_PROCESSING: i32 = 550;
const LEARNED: &str = "ok";
const REFUSED: &str = "no_recept";

/// Recipe and number of uses: the 2nd and 3rd numbers of `basePar`, a negative
/// count meaning no limit. The tooltip in the client reads them the same way.
fn taught_by(xml: &str) -> Option<(i32, i32)> {
    let (_, rest) = xml.split_once("<basePar>")?;
    let mut fields = rest.split('<').next()?.split('/').skip(1);
    Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
}

/// A recipe item whose recipe the character already has for good, or always
/// had: learning it would teach nothing.
pub fn teaches_nothing_new(recipes: &[Recipe], character: &CharacterData, item: &CatalogItem) -> bool {
    if item.kind != RECIPE_KIND {
        return false;
    }
    let Some(recipe) = taught_by(&item.xml).and_then(|(recipe, _)| recipes.iter().find(|one| one.id == recipe)) else {
        return false;
    };
    recipe.default || character.known_recipes.get(&recipe.id) == Some(&UNLIMITED)
}

/// Learning a limited recipe again adds its uses.
pub(super) fn add_uses(known: &mut HashMap<i32, i32>, recipe: i32, uses: i32) {
    let total = known.entry(recipe).or_insert(0);
    *total = if uses < 0 || *total < 0 { UNLIMITED } else { *total + uses };
}

/// Both requests name the station whose recipes to list: 1701 from a workbench,
/// 1700 from the cutting table (its addId) and from the collections button.
fn at_station(recipe: &Recipe, station: i32) -> bool {
    recipe.station == station
}

/// One entry of cases 1700 and 1701: int recipe, int station, int kind, result,
/// int uses left, cost (money, 0, work points), int seconds, int category, int
/// profession, int its level, int ingredient count, and for each three doubles
/// and three ints. The first number of each triple is an item id.
fn write_row(w: &mut AfcWriter, recipe: &Recipe, station: i32, uses: i32) {
    let triple = |w: &mut AfcWriter, values: &[f64]| {
        for value in values {
            w.double(*value);
        }
    };
    w.int32(recipe.id).int32(station).int32(SIMPLE_RECIPE);
    triple(w, &recipe.result);
    w.int32(uses);
    triple(w, &recipe.cost);
    w.int32(recipe.time).int32(recipe.category).int32(recipe.profession).int32(recipe.profession_level);
    w.int32(recipe.ingredients.len() as i32);
    for ingredient in &recipe.ingredients {
        triple(w, &ingredient[..3]);
        for value in &ingredient[3..] {
            w.int32(*value as i32);
        }
    }
}

pub async fn station(session: &mut Session, message: &AfcMessage, id: i32) -> Result<Outcome> {
    let mut r = message.reader();
    if id == msg::CRAFT {
        r.int32()?;
    }
    let station = r.int32().unwrap_or(0);

    let player = active(&mut session.player)?;
    let world = &session.conn.world;
    let exists = |item: i32| world.catalog.get(item).is_some();
    let drawable = |recipe: &&Recipe| {
        recipe.kind == SIMPLE_RECIPE
            && exists(recipe.result[0] as i32)
            && recipe.ingredients.iter().all(|one| one[2] != ITEM_INGREDIENT || exists(one[0] as i32))
    };

    let mut w = AfcWriter::new();
    w.int32(station);
    let mut listed = 0;
    for recipe in world.recipes.iter().filter(|recipe| at_station(recipe, station)).filter(drawable) {
        let uses = if recipe.default { Some(UNLIMITED) } else { player.data.known_recipes.get(&recipe.id).copied() };
        if let Some(uses) = uses {
            write_row(&mut w, recipe, station, uses);
            listed += 1;
        }
    }
    session.conn.send(id, w.finish()).await?;
    session.conn.log(&format!("   -> {id} station {station}, {listed} recipes"));
    Ok(Outcome::Answered)
}

/// Client: long item id. Reply case 1710: a string, "ok" meaning learned. The
/// client leaves the item in the bag for the server to take.
pub async fn learn(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let item = message.reader().int64()?;
    let player = active(&mut session.player)?;
    let conn = &session.conn;
    let world = &conn.world;

    let taught = player
        .data
        .items
        .iter()
        .find(|entry| entry.id == item)
        .and_then(|entry| world.catalog.get(entry.template))
        .filter(|template| template.kind == RECIPE_KIND)
        .and_then(|template| taught_by(&template.xml))
        .and_then(|(recipe, uses)| Some((world.recipes.iter().find(|one| one.id == recipe)?, uses)))
        .filter(|(recipe, _)| !recipe.default && player.data.known_recipes.get(&recipe.id) != Some(&UNLIMITED));

    let mut w = AfcWriter::new();
    let Some((recipe, uses)) = taught else {
        w.string(REFUSED);
        conn.send(msg::RECIPE_LEARN, w.finish()).await?;
        conn.log(&format!("   -> 1710 item {item} teaches nothing new"));
        return Ok(Outcome::Answered);
    };

    let recipe = recipe.id;
    add_uses(&mut player.data.known_recipes, recipe, uses);
    inventory::use_piece(conn, player, item).await?;
    w.string(LEARNED);
    conn.send(msg::RECIPE_LEARN, w.finish()).await?;
    conn.log(&format!("   -> 1710 learned recipe {recipe}, uses {}", player.data.known_recipes[&recipe]));
    Ok(Outcome::Answered)
}

/// What one craft takes: pieces from items, and whole fish.
#[derive(Debug, Default, PartialEq)]
struct Taken {
    items: Vec<(i64, i32)>,
    fish: Vec<i64>,
}

/// Species in a fish ingredient that means any mutant, as the client checks it.
const ANY_MUTANT: i32 = -1;

/// The client's own test: 0 takes any fish but a mutant, -1 only a mutant.
/// A trait other than 0 (the ingredient's sixth number) must be on the fish:
/// the recipes use only 108 «Красная икра». The client also accepts species
/// types below 100 there, which no recipe asks for and the server does not keep.
fn fish_fits(fish: &CaughtFish, species: i32, min: i32, max: i32, trait_id: i32, mutants: &[i32]) -> bool {
    let mutant = mutants.contains(&fish.species);
    let kind = match species {
        ANY_FISH => !mutant,
        ANY_MUTANT => mutant,
        exact => fish.species == exact,
    };
    kind && (min <= 0 || fish.grams >= min) && (max <= 0 || fish.grams <= max) && (trait_id == 0 || fish.traits.contains(&trait_id))
}

/// The items and fish a recipe would use, or `None` if something is missing.
/// Fish go lightest first, so the big ones stay for other recipes.
fn ingredients_for(character: &CharacterData, recipe: &Recipe, mutants: &[i32]) -> Option<Taken> {
    let mut taken = Taken::default();
    for ingredient in &recipe.ingredients {
        let (id, need) = (ingredient[0] as i32, ingredient[1].max(1.0) as i32);
        if ingredient[2] == ITEM_INGREDIENT {
            // Every other item ingredient leaves its fourth number at 0.
            let param = if id == FISH_CARD { ingredient[3] as i32 } else { 0 };
            let mut left = need;
            let matching = |item: &&ItemEntry| item.template == id && item.param == param && item.bag < FIRST_NON_INVENTORY_BAG;
            for item in character.items.iter().filter(matching) {
                let already = taken.items.iter().filter(|(held, _)| *held == item.id).map(|(_, n)| n).sum::<i32>();
                let part = left.min(item.count - already);
                if part > 0 {
                    taken.items.push((item.id, part));
                    left -= part;
                }
                if left == 0 {
                    break;
                }
            }
            if left > 0 {
                return None;
            }
        } else {
            let (min, max, trait_id) = (ingredient[3] as i32, ingredient[4] as i32, ingredient[5] as i32);
            if id < ANY_MUTANT {
                return None;
            }
            let mut fitting: Vec<&CaughtFish> = character
                .fish
                .iter()
                .filter(|fish| keepnet::in_keepnet(fish) && !taken.fish.contains(&fish.id) && fish_fits(fish, id, min, max, trait_id, mutants))
                .collect();
            if fitting.len() < need as usize {
                return None;
            }
            fitting.sort_by_key(|fish| fish.grams);
            taken.fish.extend(fitting.iter().take(need as usize).map(|fish| fish.id));
        }
    }
    Some(taken)
}

async fn refuse(session: &Session, reason: &str, recipe: i32) -> Result<Outcome> {
    let mut w = AfcWriter::new();
    w.string(&format!("{reason}=0"));
    session.conn.send(msg::CRAFT_ERROR, w.finish()).await?;
    session.conn.log(&format!("   -> 1703 recipe {recipe}: {reason}"));
    Ok(Outcome::Answered)
}

/// Client: int station, int recipe, once the progress bar is full. Reply case
/// 1702: int item, int count, bool critical (an int quality may follow; without
/// it the client takes 1); the client then counts
/// its ingredients again from the item and fish updates sent before it. Case
/// 1703: a string naming the reason in the client's texts.
pub async fn make(session: &mut Session, message: &AfcMessage) -> Result<Outcome> {
    let mut r = message.reader();
    let (_station, recipe_id) = (r.int32()?, r.int32()?);
    let world = session.conn.world.clone();

    let Some(recipe) = world.recipes.iter().find(|one| one.id == recipe_id && one.kind == SIMPLE_RECIPE) else {
        return refuse(session, "no_recept", recipe_id).await;
    };
    let player = active(&mut session.player)?;
    let uses = if recipe.default { UNLIMITED } else { player.data.known_recipes.get(&recipe.id).copied().unwrap_or(0) };
    let money = recipe.cost[0] as i64;
    let skill = |perk: i32| {
        let per_level = world.settings.skill_tree.get(perk).map_or(0.0, |node| node.effect[0]);
        per_level * f64::from(perks::level_of(&session.conn, player, perk))
    };
    let work = (f64::from(recipe.cost[2] as f32) * (1.0 - skill(CRAFT_EXPERIENCE) / 100.0)).max(0.0) as f32;
    let cutting = recipe.station == world.settings.skills.fish_cutting_station;
    let crit_chance = recipe.crit_chance + skill(CRAFT) + if cutting { skill(FIELD_PROCESSING) } else { 0.0 };
    let points = player.data.vitals.as_ref().map_or(0.0, |vitals| vitals.work_points);
    let critical = recipe.crit_result[0] > 0.0 && random::<f64>() * 100.0 < crit_chance;
    let made = if critical { recipe.crit_result } else { recipe.result };
    let (template, count) = (made[0] as i32, made[1].max(1.0) as i32);

    let problem = if uses == 0 {
        Some("no_user_recept")
    } else if player.data.money < money {
        Some("no_money")
    } else if points < work {
        Some("no_workpoint")
    } else {
        None
    };
    if let Some(reason) = problem {
        return refuse(session, reason, recipe_id).await;
    }
    let Some(taken) = ingredients_for(&player.data, recipe, &world.settings.fishing.mutant_species) else {
        return refuse(session, "no_ingridients", recipe_id).await;
    };
    let Some(item) = world.catalog.get(template) else {
        return refuse(session, "no_recept", recipe_id).await;
    };
    if inventory::plan_pieces(&world.settings.bags, &player.data, item, count).is_err() {
        return refuse(session, "no_inventory", recipe_id).await;
    }

    let conn = &session.conn;
    let mut changed = Vec::new();
    for (id, pieces) in &taken.items {
        match inventory::take_pieces(&mut player.data, *id, *pieces) {
            Some(Ok(item)) => changed.push(item),
            Some(Err(gone)) => inventory::send_removed(conn, gone).await?,
            None => {}
        }
    }
    player.data.fish.retain(|fish| !taken.fish.contains(&fish.id));
    for id in &taken.fish {
        let mut w = AfcWriter::new();
        w.int64(*id);
        conn.send(msg::FISH_REMOVE, w.finish()).await?;
    }
    player.data.money -= money;
    if let Some(vitals) = player.data.vitals.as_mut() {
        vitals.work_points -= work;
    }
    if uses > 0 {
        player.data.known_recipes.insert(recipe.id, uses - 1);
    }
    if let Ok(gained) = inventory::give(conn, player, item, count).await? {
        changed.extend(gained);
    }
    conn.save(player).await?;

    inventory::send_updates(conn, player.data.id, &changed).await?;
    if money > 0 {
        character::send_money(conn, player, -money).await?;
    }
    vitals::send(conn, player).await?;
    let mut w = AfcWriter::new();
    w.int32(template).int32(count).int32(made[2] as i32);
    conn.send(msg::ITEMS_GAINED, w.finish()).await?;
    let mut w = AfcWriter::new();
    w.int32(template).int32(count).boolean(critical);
    conn.send(msg::CRAFT_MAKE, w.finish()).await?;
    conn.log(&format!(
        "   -> 1702 recipe {recipe_id}: {template} x{count}{}, used {} items and {} fish",
        if critical { " (critical)" } else { "" },
        taken.items.len(),
        taken.fish.len()
    ));
    Ok(Outcome::Answered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::ItemEntry;

    fn fish(id: i64, species: i32, grams: i32) -> CaughtFish {
        CaughtFish { id, species, grams, size_class: 0, bait: 0, list: 0, experience: 1, traits: Vec::new() }
    }

    fn held(id: i64, template: i32, count: i32, bag: i32) -> ItemEntry {
        ItemEntry { id, bag, cell: 0, count, durability: 1.0, template, param: 0 }
    }

    fn needing(ingredients: Vec<[f64; 6]>) -> Recipe {
        Recipe { ingredients, ..recipe(3, 0) }
    }

    #[test]
    fn fish_go_lightest_first_within_the_weights() {
        let character = CharacterData {
            fish: vec![fish(1, 5, 2000), fish(2, 7, 600), fish(3, 5, 450), fish(4, 5, 900)],
            ..Default::default()
        };
        let recipe = needing(vec![[0.0, 2.0, 1.0, 451.0, 1000.0, 0.0]]);
        assert_eq!(ingredients_for(&character, &recipe, &[]).unwrap().fish, vec![2, 4]);
        let one_species = needing(vec![[5.0, 2.0, 1.0, 0.0, 0.0, 0.0]]);
        assert_eq!(ingredients_for(&character, &one_species, &[]).unwrap().fish, vec![3, 4]);
        let too_many = needing(vec![[0.0, 3.0, 1.0, 451.0, 1000.0, 0.0]]);
        assert_eq!(ingredients_for(&character, &too_many, &[]), None);
    }

    #[test]
    fn a_collection_takes_the_card_of_its_own_species() {
        let card = |id, species| ItemEntry { param: species, ..held(id, FISH_CARD, 1, 7) };
        let character = CharacterData { items: vec![card(1, 78), card(2, 77)], ..Default::default() };
        let recipe = needing(vec![[1309.0, 1.0, 0.0, 77.0, 0.0, 0.0], [1309.0, 1.0, 0.0, 78.0, 0.0, 0.0]]);
        assert_eq!(ingredients_for(&character, &recipe, &[]).unwrap().items, vec![(2, 1), (1, 1)]);
        let other = needing(vec![[1309.0, 1.0, 0.0, 54.0, 0.0, 0.0]]);
        assert_eq!(ingredients_for(&character, &other, &[]), None);
    }

    #[test]
    fn items_add_up_across_stacks_but_not_from_the_warehouse() {
        let character = CharacterData {
            items: vec![held(1, 1148, 3, 7), held(2, 1148, 4, 7), held(3, 1148, 50, 52)],
            ..Default::default()
        };
        let recipe = needing(vec![[1148.0, 5.0, 0.0, 0.0, 0.0, 0.0]]);
        assert_eq!(ingredients_for(&character, &recipe, &[]).unwrap().items, vec![(1, 3), (2, 2)]);
        let more = needing(vec![[1148.0, 8.0, 0.0, 0.0, 0.0, 0.0]]);
        assert_eq!(ingredients_for(&character, &more, &[]), None);
    }

    #[test]
    fn mutants_go_only_where_a_mutant_is_asked_for() {
        let character = CharacterData { fish: vec![fish(1, 5, 2000), fish(2, 339, 900)], ..Default::default() };
        let mutant = needing(vec![[-1.0, 1.0, 1.0, 100.0, 15000.0, 0.0]]);
        assert_eq!(ingredients_for(&character, &mutant, &[339]).unwrap().fish, vec![2]);
        let any = needing(vec![[0.0, 1.0, 1.0, 100.0, 3000.0, 0.0]]);
        assert_eq!(ingredients_for(&character, &any, &[339]).unwrap().fish, vec![1]);
        assert_eq!(ingredients_for(&character, &mutant, &[]), None);
    }

    #[test]
    fn the_caviar_cut_takes_only_a_fish_with_caviar() {
        let caviar = CaughtFish { traits: vec![108], ..fish(2, 95, 3500) };
        let plain = CharacterData { fish: vec![fish(1, 95, 4000)], ..Default::default() };
        let recipe = needing(vec![[0.0, 1.0, 1.0, 0.0, 0.0, 108.0]]);
        assert_eq!(ingredients_for(&plain, &recipe, &[]), None);
        let with = CharacterData { fish: vec![fish(1, 95, 4000), caviar], ..Default::default() };
        assert_eq!(ingredients_for(&with, &recipe, &[]).unwrap().fish, vec![2]);
    }

    fn recipe(station: i32, show: i32) -> Recipe {
        Recipe {
            id: 142,
            station,
            show,
            category: 400,
            kind: 0,
            default: false,
            result: [1012.0, 20.0, 0.0],
            cost: [1.0, 0.0, 5.0],
            time: 5,
            crit_chance: 0.0,
            crit_result: [0.0; 3],
            profession: 0,
            profession_level: 0,
            ingredients: vec![[1035.0, 1.0, 0.0, 0.0, 0.0, 0.0]],
        }
    }

    #[test]
    fn a_recipe_known_for_good_teaches_nothing_new() {
        let item = |uses: i32| CatalogItem {
            id: 1144,
            kind: RECIPE_KIND,
            cost: 10,
            durability: 1.0,
            stacks: false,
            shop_count: 1,
            max_count: 1,
            premium_cost: 0, xml: format!("<base><basePar>3/142/{uses}</basePar></base>"), refine: String::new(), name: String::new(), cost_item: [0, 0],
        };
        let recipes = [recipe(3, 0)];
        let mut character = CharacterData::default();
        assert!(!teaches_nothing_new(&recipes, &character, &item(-1)));
        character.known_recipes.insert(142, 3);
        assert!(!teaches_nothing_new(&recipes, &character, &item(10)));
        character.known_recipes.insert(142, UNLIMITED);
        assert!(teaches_nothing_new(&recipes, &character, &item(10)));
        let always = [Recipe { default: true, ..recipe(3, 0) }];
        assert!(teaches_nothing_new(&always, &CharacterData::default(), &item(-1)));
    }

    #[test]
    fn the_item_names_recipe_and_uses() {
        assert_eq!(taught_by("<base><basePar>3/142/10</basePar></base>"), Some((142, 10)));
        assert_eq!(taught_by("<base><basePar>7/701/-1</basePar></base>"), Some((701, -1)));
        assert_eq!(taught_by("<base></base>"), None);
    }

    #[test]
    fn learning_again_adds_uses_up_to_unlimited() {
        let mut known = HashMap::new();
        add_uses(&mut known, 1, 10);
        add_uses(&mut known, 1, 10);
        assert_eq!(known[&1], 20);
        add_uses(&mut known, 1, -1);
        add_uses(&mut known, 1, 10);
        assert_eq!(known[&1], UNLIMITED);
    }

    /// The collections window, opened from the bottom panel with 1700.
    const COLLECTIONS: i32 = 1000;

    #[test]
    fn the_cutting_table_lists_its_own_recipes_not_the_collections() {
        assert!(at_station(&recipe(3, 0), 3));
        assert!(!at_station(&recipe(COLLECTIONS, 3), 3));
        assert!(at_station(&recipe(COLLECTIONS, 3), COLLECTIONS));
    }

    #[test]
    fn a_row_has_the_size_the_client_reads() {
        let mut w = AfcWriter::new();
        write_row(&mut w, &recipe(3, 0), 3, 10);
        // Three ints, triple, int, triple, five ints, then a triple and three ints.
        assert_eq!(w.finish().len(), 12 + 24 + 4 + 24 + 20 + 24 + 12);
    }
}
