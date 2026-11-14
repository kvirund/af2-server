//! Packages and boxes (wtype 30, «Использовать» in the item's menu). The client
//! shows the bar and then sends 275 with the item, the same as for a course.
//! What each one holds or gives is in `config/settings.json` "packages": fixed
//! contents, one random item of a list (a fish card among them), or a gift of
//! the item's own `basePar` first number: VIP days, experience, work points or
//! ergs.

use anyhow::Result;
use rand::random;

use super::{Conn, Outcome, Player, character, chat, crafting, inventory, vip, vitals};
use crate::config::{CatalogItem, Gift, PackageSettings, Recipe};

/// Species whose card some collection recipe asks for: only those cards matter.
fn wanted_cards(recipes: &[Recipe]) -> Vec<i32> {
    let mut species: Vec<i32> = recipes
        .iter()
        .flat_map(|recipe| &recipe.ingredients)
        .filter(|ingredient| ingredient[0] as i32 == crafting::FISH_CARD && ingredient[3] as i32 > 0)
        .map(|ingredient| ingredient[3] as i32)
        .collect();
    species.sort_unstable();
    species.dedup();
    species
}

/// What comes out: (item, pieces, param). `roll` gives numbers in 0..1.
fn contents(package: &PackageSettings, cards: &[i32], roll: &mut impl FnMut() -> f64) -> Vec<(i32, i32, i32)> {
    let mut out: Vec<(i32, i32, i32)> = package.items.iter().map(|[item, count]| (*item, (*count).max(1), 0)).collect();
    let pick = |from: usize, roll: f64| ((roll * from as f64) as usize).min(from.saturating_sub(1));
    if !cards.is_empty() && roll() * 100.0 < package.fish_card_percent {
        out.push((crafting::FISH_CARD, 1, cards[pick(cards.len(), roll())]));
    } else if !package.one_of.is_empty() {
        out.push((package.one_of[pick(package.one_of.len(), roll())], 1, 0));
    }
    out
}

/// The first `basePar` number: days, experience, work points or ergs.
fn amount(item: &CatalogItem) -> i64 {
    item.xml
        .split_once("<basePar>")
        .and_then(|(_, rest)| rest.split(['/', '<']).next())
        .and_then(|number| number.trim().parse().ok())
        .unwrap_or(0)
}

fn name_of(conn: &Conn, template: i32, param: i32) -> String {
    let name = conn.world.catalog.get(template).map_or_else(|| template.to_string(), |item| item.name.clone());
    let species = conn.world.fish.get(param).map_or("?", |species| species.name.as_str());
    name.replace("{0}", species)
}

/// Client 275 for a package. `None` when the item is not one.
pub async fn open(conn: &Conn, player: &mut Player, id: i64, template: i32) -> Result<Option<Outcome>> {
    let world = conn.world.clone();
    let Some(package) = world.settings.packages.get(&template) else {
        return Ok(None);
    };
    let Some(box_item) = world.catalog.get(template) else {
        return Ok(None);
    };
    let title = box_item.name.clone();

    if let Some(gift) = package.gives {
        let amount = amount(box_item);
        let done = match gift {
            Gift::Vip => {
                inventory::use_piece(conn, player, id).await?;
                vip::grant(conn, player, amount, 0).await?;
                format!("VIP-премиум продлён на {amount} дн.")
            }
            Gift::Experience => {
                inventory::use_piece(conn, player, id).await?;
                character::award_experience(conn, player, amount as i32, 1.0).await?;
                format!("получено {amount} опыта")
            }
            Gift::WorkPoints => {
                inventory::use_piece(conn, player, id).await?;
                let most = world.settings.vitals.max_work_points;
                if let Some(now) = player.data.vitals.as_mut() {
                    now.work_points = (now.work_points + amount as f32).min(most.max(now.work_points));
                }
                conn.save(player).await?;
                vitals::send(conn, player).await?;
                format!("восстановлено {amount} ОР")
            }
            Gift::Premium => {
                inventory::use_piece(conn, player, id).await?;
                player.data.premium += amount;
                conn.save(player).await?;
                character::send_money(conn, player, 0).await?;
                format!("зачислено {amount} эргов")
            }
        };
        chat::system(conn, &format!("{title}: {done}.")).await?;
        conn.log(&format!("   package {template} used: {done}"));
        return Ok(Some(Outcome::Answered));
    }

    let mut roll = random::<f64>;
    let out = contents(package, &wanted_cards(&world.recipes), &mut roll);
    // Everything has to fit before the box is opened; the box's own cell is not
    // counted, so a nearly full page may refuse a box that would just fit.
    for (inside, count, param) in &out {
        let fits = world.catalog.get(*inside).is_some_and(|item| {
            inventory::plan_pieces_of(&world.settings.bags, &player.data, item, *count, *param).is_ok()
        });
        if !fits {
            chat::system(conn, &format!("{title}: содержимое не помещается в рюкзак.")).await?;
            conn.log(&format!("   package {template}: {inside} x{count} does not fit or is not in the catalog"));
            return Ok(Some(Outcome::Answered));
        }
    }
    inventory::use_piece(conn, player, id).await?;
    let mut got = Vec::new();
    for (template, count, param) in out {
        let Some(item) = world.catalog.get(template) else { continue };
        match inventory::give_of(conn, player, item, count, param).await? {
            Ok(changed) => {
                inventory::send_updates(conn, player.data.id, &changed).await?;
                got.push(if count > 1 { format!("{} x{count}", name_of(conn, template, param)) } else { name_of(conn, template, param) });
            }
            Err(error) => conn.log(&format!("   package item {template} lost: {}", error.status())),
        }
    }
    conn.save(player).await?;
    let list = if got.is_empty() { "пусто".to_string() } else { got.join(", ") };
    chat::system(conn, &format!("{title}: {list}.")).await?;
    conn.log(&format!("   -> 181 package {template} opened: {list}"));
    Ok(Some(Outcome::Answered))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(items: Vec<[i32; 2]>, one_of: Vec<i32>, fish_card_percent: f64) -> PackageSettings {
        PackageSettings { items, one_of, fish_card_percent, gives: None }
    }

    #[test]
    fn a_box_gives_its_fixed_contents() {
        let crate_of_parts = package(vec![[1153, 3], [1154, 3]], vec![], 0.0);
        assert_eq!(contents(&crate_of_parts, &[], &mut || 0.5), vec![(1153, 3, 0), (1154, 3, 0)]);
    }

    #[test]
    fn a_sealed_card_is_a_base_card_or_a_wanted_fish_card() {
        let sealed = package(vec![], vec![1310, 1313], 50.0);
        let mut rolls = [0.9, 0.99].into_iter();
        assert_eq!(contents(&sealed, &[77, 78], &mut || rolls.next().unwrap()), vec![(1313, 1, 0)]);
        let mut rolls = [0.1, 0.6].into_iter();
        assert_eq!(contents(&sealed, &[77, 78], &mut || rolls.next().unwrap()), vec![(crafting::FISH_CARD, 1, 78)]);
        let mut rolls = [0.1, 0.0].into_iter();
        assert_eq!(contents(&sealed, &[], &mut || rolls.next().unwrap()), vec![(1310, 1, 0)]);
    }
}
