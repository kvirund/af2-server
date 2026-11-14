//! Things that come up together with a fish. What can come up is from a player
//! guide; how often is set in `config/settings.json`.

use anyhow::Result;
use rand::{random, random_range};

use super::{Conn, Player, cards, character, crafting, inventory, perks, reputation, travel, vip};
use crate::afc::AfcWriter;
use crate::config::{LootItem, Recipe};
use crate::messages as msg;

fn pick(items: &[LootItem], roll: f64) -> Option<&LootItem> {
    let total: f64 = items.iter().map(|item| item.weight.max(0.0)).sum();
    let mut left = roll * total;
    for item in items.iter().filter(|item| item.weight > 0.0) {
        left -= item.weight;
        if left < 0.0 {
            return Some(item);
        }
    }
    None
}

const CRAFTSMANS_EYE: i32 = 131;

/// Whether some recipe asks for the card of this species; only those drop.
fn card_wanted(recipes: &[Recipe], species: i32) -> bool {
    recipes.iter().flat_map(|recipe| &recipe.ingredients).any(|ingredient| {
        ingredient[0] as i32 == crafting::FISH_CARD && ingredient[3] as i32 == species
    })
}

fn happens(percent: f64) -> bool {
    random::<f64>() * 100.0 < percent
}

/// Rolls the loot of one landed fish and puts it in the bags. Case 126: int item,
/// int count (not read), int quality; the client shows "Поймано: ...".
/// `species` is the fish landed, whose card may come up with it.
pub async fn after_catch(conn: &Conn, player: &mut Player, species: i32) -> Result<()> {
    let settings = &conn.world.settings;
    let loot = &settings.loot;
    let mut found = Vec::new();
    // «Глаз ремесленника»: its `val` per level, in percent of the chance.
    let eye = settings.skill_tree.get(CRAFTSMANS_EYE).map_or(0.0, |node| node.effect[0]);
    let per_level = &settings.reputation.per_level;
    let chance = loot.chance_percent
        * (1.0 + eye * f64::from(perks::level_of(conn, player, CRAFTSMANS_EYE)) / 100.0)
        * reputation::base_bonus(settings, player, per_level.loot_percent)
        * vip::multiplier(&player.data, settings.vip.loot_chance_percent);
    if happens(chance)
        && let Some(item) = pick(&loot.items, random())
    {
        found.push((item.item, item.count.max(1), 0));
    }
    // Each wreath of the species makes its card more likely.
    let (wreaths, _) = cards::mastery(&settings.mastery, player.data.catches.get(&species).map_or(0, |catches| catches.count));
    let card_chance = loot.fish_card_chance_percent * (1.0 + settings.mastery.card_percent / 100.0 * f64::from(wreaths));
    if card_wanted(&conn.world.recipes, species) && happens(card_chance) {
        found.push((crafting::FISH_CARD, 1, species));
    }
    for card in loot.species_cards.iter().filter(|card| card.species.is_empty() || card.species.contains(&species)) {
        if happens(card.chance_percent) {
            found.push((card.item, 1, 0));
        }
    }
    let base = travel::current_base(settings, player);
    if let Some(card) = loot.base_cards.get(&base)
        && happens(loot.base_card_chance_percent * reputation::base_bonus(settings, player, per_level.base_card_percent))
    {
        found.push((*card, 1, 0));
    }

    if happens(loot.premium_chance_percent) {
        let [least, most] = loot.premium;
        let amount = random_range(least.min(most)..=most.max(least));
        player.data.premium += amount;
        conn.save(player).await?;
        character::send_money(conn, player, 0).await?;
        conn.log(&format!("   -> 21 premium +{amount}, now {}", player.data.premium));
    }

    for (template, count, param) in found {
        let Some(item) = conn.world.catalog.get(template) else {
            conn.log(&format!("   loot item {template} is not in the catalog"));
            continue;
        };
        match inventory::give_of(conn, player, item, count, param).await? {
            Ok(changed) => {
                inventory::send_updates(conn, player.data.id, &changed).await?;
                let mut w = AfcWriter::new();
                w.int32(template).int32(count).int32(0);
                conn.send(msg::LOOT, w.finish()).await?;
                conn.save(player).await?;
                conn.log(&format!("   -> 126 loot {template} x{count} (param {param})"));
            }
            Err(error) => conn.log(&format!("   loot {template} lost: {}", error.status())),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(item: i32, weight: f64) -> LootItem {
        LootItem { item, weight, count: 1 }
    }

    #[test]
    fn weights_split_the_roll() {
        let items = [item(1, 3.0), item(2, 1.0)];
        assert_eq!(pick(&items, 0.0).map(|i| i.item), Some(1));
        assert_eq!(pick(&items, 0.74).map(|i| i.item), Some(1));
        assert_eq!(pick(&items, 0.76).map(|i| i.item), Some(2));
    }

    #[test]
    fn nothing_to_pick_from_an_empty_table() {
        assert!(pick(&[], 0.5).is_none());
        assert!(pick(&[item(1, 0.0)], 0.5).is_none());
    }
}
