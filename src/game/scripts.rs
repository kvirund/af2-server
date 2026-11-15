//! The Lua use scripts of items (`<useScript>` in the client's item table).
//! The original server ran them; this one reads the few shapes they come in
//! and turns each call into an action. `wpn.baseParametr.iX/iY/iZ` are the
//! item's own `basePar` numbers.

use std::collections::HashMap;

use regex::Regex;

/// One entry of `lut:rndChanceList`: "item/min/max/weight". A count of 0/0
/// means the script's own count (1).
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub item: i32,
    pub min: i32,
    pub max: i32,
    pub weight: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Give { item: i32, count: i32 },
    /// One item of the list by weight. If it is `card.0`, it comes with a
    /// species from `card.1` in its param (a fish card).
    Pick { choices: Vec<Choice>, card: Option<(i32, Vec<i32>)> },
    TeachRecipe { recipe: i32, uses: i32 },
    Effect { effect: i32, force: i32, seconds: i64 },
    Experience(i64),
    VipDays(i64),
    WorkPoints(i64),
    Premium(i64),
    /// `gu.nav_point`: free points of the parameters.
    ParameterPoints(i32),
    ResetParameters,
    /// `goToLoc`, and `goToLocTimed` for a stay with a time limit.
    Travel { base: i32, location: i32, timed: bool },
    /// Adds 1 to the quest value named prefix + the current base.
    BaseValue { prefix: String },
    Unknown(String),
}

/// Calls that only make a sound, store the character or belong to another
/// action's shape.
const QUIET: [&str; 10] =
    ["playSound", "store", "getQValue", "getRandom", "getIntMinMax", "addMsgLong", "rndlist", "giveWpnWithParams", "updateRealStatus", "teachPerk"];

fn unescape(text: &str) -> String {
    text.replace("&gt;", ">").replace("&lt;", "<").replace("&quot;", "\"").replace("&amp;", "&").replace("\\r\\n", "\n")
}

/// The script inside an item's description, if it has one.
pub fn script_of(xml: &str) -> Option<String> {
    let (_, rest) = xml.split_once("<useScript>")?;
    let script = unescape(rest.split("</useScript>").next()?).trim().to_string();
    (!script.is_empty()).then_some(script)
}

fn base_par(xml: &str) -> [f64; 3] {
    let mut out = [0.0; 3];
    if let Some((_, rest)) = xml.split_once("<basePar>") {
        let fields = rest.split('<').next().unwrap_or("").split('/');
        for (slot, field) in out.iter_mut().zip(fields) {
            *slot = field.trim().parse().unwrap_or(0.0);
        }
    }
    out
}

/// Numbers, names of numeric locals, `*` and `+`.
fn eval(expr: &str, vars: &HashMap<String, f64>) -> Option<f64> {
    let mut sum = 0.0;
    for term in expr.split('+') {
        let mut product = 1.0;
        for factor in term.split('*') {
            let factor = factor.trim().trim_start_matches('(').trim_end_matches(')').trim();
            product *= factor.parse::<f64>().ok().or_else(|| vars.get(factor).copied())?;
        }
        sum += product;
    }
    Some(sum)
}

fn choices(list: &str) -> Vec<Choice> {
    list.split(',')
        .filter_map(|entry| {
            let mut f = entry.split('/').map(str::trim);
            Some(Choice {
                item: f.next()?.parse().ok()?,
                min: f.next()?.parse().ok()?,
                max: f.next()?.parse().ok()?,
                weight: f.next()?.parse().ok()?,
            })
        })
        .collect()
}

/// What a script does, in order. `xml` is the item's description, for
/// `basePar`.
pub fn parse(script: &str, xml: &str) -> Vec<Action> {
    let par = base_par(xml);
    let text = script.replace("wpn.baseParametr.iX", &par[0].to_string()).replace("wpn.baseParametr.iY", &par[1].to_string()).replace(
        "wpn.baseParametr.iZ",
        &par[2].to_string(),
    );

    let mut numbers: HashMap<String, f64> = HashMap::new();
    let mut strings: HashMap<String, String> = HashMap::new();
    let mut prefixes: HashMap<String, String> = HashMap::new();
    let local_string = Regex::new(r#"local\s+(\w+)\s*=\s*"([^"]*)"\s*(\.\.\s*\w+)?"#).expect("pattern");
    for found in local_string.captures_iter(&text) {
        if found.get(3).is_some() {
            prefixes.insert(found[1].to_string(), found[2].to_string());
        } else {
            strings.insert(found[1].to_string(), found[2].to_string());
        }
    }
    let local_number = Regex::new(r"local\s+(\w+)\s*=\s*([\w.*+() ]+?)\s*;").expect("pattern");
    for found in local_number.captures_iter(&text) {
        if let Some(value) = eval(&found[2], &numbers) {
            numbers.insert(found[1].to_string(), value);
        }
    }

    let card = Regex::new(r"wpid\s*==\s*(\d+)")
        .expect("pattern")
        .captures(&text)
        .and_then(|found| found[1].parse::<i32>().ok())
        .zip(Regex::new(r"rndlist\s*\(\s*(\w+)").expect("pattern").captures(&text).and_then(|found| strings.get(&found[1]).cloned()))
        .map(|(item, list)| (item, list.split(',').filter_map(|s| s.trim().parse().ok()).collect::<Vec<i32>>()));

    let number = |arg: Option<&&str>| arg.and_then(|arg| eval(arg, &numbers));
    let call = Regex::new(r"(?:gu\.acc|gu|lut)\s*[:.]\s*(\w+)\s*\(([^)]*)\)|gu\.nav_point\s*=\s*gu\.nav_point\s*\+\s*(\w+)").expect("pattern");
    let mut actions = Vec::new();
    for found in call.captures_iter(&text) {
        if let Some(points) = found.get(3) {
            actions.push(Action::ParameterPoints(eval(points.as_str(), &numbers).unwrap_or(0.0) as i32));
            continue;
        }
        let name = &found[1];
        let args: Vec<&str> = found[2].split(',').map(str::trim).collect();
        let action = match name {
            "giveWeaponToUser" => match (number(args.first()), number(args.get(1))) {
                // `wpid` is the picked item: the pick gives it.
                (Some(item), count) if !args[0].starts_with("wpid") => Some(Action::Give { item: item as i32, count: count.unwrap_or(1.0) as i32 }),
                _ => None,
            },
            "rndChanceList" => {
                let list = args.first().and_then(|arg| strings.get(*arg)).map(|list| choices(list)).unwrap_or_default();
                Some(Action::Pick { choices: list, card: card.clone() })
            }
            "teachRecept" => number(args.first())
                .zip(number(args.get(1)))
                .map(|(recipe, uses)| Action::TeachRecipe { recipe: recipe as i32, uses: uses as i32 }),
            "addActiveEffectLUA" => match (number(args.first()), number(args.get(1)), number(args.get(4))) {
                (Some(effect), Some(force), Some(seconds)) => Some(Action::Effect { effect: effect as i32, force: force as i32, seconds: seconds as i64 }),
                _ => None,
            },
            "addExp" => number(args.first()).map(|amount| Action::Experience(amount as i64)),
            "addVIPStatus" => number(args.first()).map(|days| Action::VipDays(days as i64)),
            "addWorkPoints" => number(args.first()).map(|points| Action::WorkPoints(points as i64)),
            "addReal" => number(args.first()).map(|ergs| Action::Premium(ergs as i64)),
            "resetParametrs" => Some(Action::ResetParameters),
            "goToLoc" => number(args.first())
                .zip(number(args.get(1)))
                .map(|(base, location)| Action::Travel { base: base as i32, location: location as i32, timed: false }),
            "goToLocTimed" => number(args.first())
                .zip(number(args.get(1)))
                .map(|(base, location)| Action::Travel { base: base as i32, location: location as i32, timed: true }),
            "setQValue" => args.first().and_then(|arg| prefixes.get(*arg)).map(|prefix| Action::BaseValue { prefix: prefix.clone() }),
            quiet if QUIET.contains(&quiet) => None,
            other => Some(Action::Unknown(other.to_string())),
        };
        actions.extend(action);
    }
    actions
}

/// Picks one choice by weight; `roll` is in 0..1.
pub fn pick(choices: &[Choice], roll: f64) -> Option<&Choice> {
    let total: f64 = choices.iter().map(|choice| choice.weight.max(0.0)).sum();
    if total <= 0.0 {
        return None;
    }
    let mut left = roll * total;
    for choice in choices {
        left -= choice.weight.max(0.0);
        if left < 0.0 {
            return Some(choice);
        }
    }
    choices.last()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_box_gives_its_items() {
        let script = r#"do trace("use rod-container!"); gu:giveWeaponToUser(1153, 3);  gu:giveWeaponToUser(1154, 3); end;"#;
        assert_eq!(parse(script, ""), vec![Action::Give { item: 1153, count: 3 }, Action::Give { item: 1154, count: 3 }]);
    }

    #[test]
    fn a_sealed_card_picks_from_its_list_with_fish_cards() {
        let script = r#"do local wpid=0; local cnt=1; local list="1310/0/0/10,1309/0/0/15"; local res=lut:rndChanceList(list); wpid=res.iX;  local wpnid=0; if (wpid==1309) then  local flist="84,56";  local fid=lut:rndlist(flist);  local wpn=gu:giveWpnWithParams(wpid,1,fid.."/0/0",true,true);  wpnid=wpn.id;  else   local wpn=gu:giveWeaponToUser(wpid, cnt,true,true);  wpnid=wpn.id; end;  gu.currentPacket:addMsgLong (1704,wpnid);end;"#;
        let choices = vec![Choice { item: 1310, min: 0, max: 0, weight: 10.0 }, Choice { item: 1309, min: 0, max: 0, weight: 15.0 }];
        assert_eq!(parse(script, ""), vec![Action::Pick { choices, card: Some((1309, vec![84, 56])) }]);
    }

    #[test]
    fn a_sock_picks_a_count_too() {
        let script = r#"do local wpid=0; local cnt=1; local wpidun=0; local list="1264/1/2/5,1308/10/20/5"; local res=lut:rndChanceList(list); wpid=res.iX; cnt=lut:getRandom():getIntMinMax(res.iY,res.iZ); if (wpid&gt;0) then    local wpn=gu:giveWeaponToUser(wpid, cnt);    wpidun=wpn.id   gu.currentPacket:addMsgLong (1704,wpidun); end;end;"#;
        let actions = parse(&unescape(script), "");
        assert_eq!(actions.len(), 1);
        let Action::Pick { choices, card: None } = &actions[0] else { panic!("{actions:?}") };
        assert_eq!(choices[1], Choice { item: 1308, min: 10, max: 20, weight: 5.0 });
    }

    #[test]
    fn base_par_fills_the_numbers() {
        let card = "do local effid=wpn.baseParametr.iX; local force=wpn.baseParametr.iY; local minuts=wpn.baseParametr.iZ; gu:addActiveEffectLUA(effid,force,0,0,minuts*60); gu:playSound(\"Hint.ogg\");end;";
        let xml = "<basePar>21/50/120</basePar>";
        assert_eq!(parse(card, xml), vec![Action::Effect { effect: 21, force: 50, seconds: 7200 }]);
        let trip = "do local bid=wpn.baseParametr.iX; local lid=wpn.baseParametr.iY; local tm=wpn.baseParametr.iZ; gu:goToLocTimed(bid, lid, 0, tm*60);end;";
        assert_eq!(parse(trip, "<basePar>5/51/4</basePar>"), vec![Action::Travel { base: 5, location: 51, timed: true }]);
    }

    #[test]
    fn the_other_shapes() {
        let token = "do local effid=21; local force=50; local minuts=240; gu:addActiveEffectLUA(effid,force,0,0,minuts*60); gu:addActiveEffectLUA(20,20,0,0,minuts*60); gu:playSound(\"Hint.ogg\");end;";
        assert_eq!(
            parse(token, ""),
            vec![Action::Effect { effect: 21, force: 50, seconds: 14400 }, Action::Effect { effect: 20, force: 20, seconds: 14400 }]
        );
        let implant = "do  local ppt=2;  gu.nav_point=gu.nav_point+ppt;  gu:store();  gu:playSound(\"start2.ogg\");end;";
        assert_eq!(parse(implant, ""), vec![Action::ParameterPoints(2)]);
        let recipes = "do  gu:teachRecept(571,-1);  gu:teachRecept(574,3);end;";
        assert_eq!(parse(recipes, ""), vec![Action::TeachRecipe { recipe: 571, uses: -1 }, Action::TeachRecipe { recipe: 574, uses: 3 }]);
        let cold = "do local bsid=gu.baseid; local name=\"holod_\"..bsid; local holid=gu:getQValue (name); holid=holid+1; gu:setQValue (name,holid); gu:playSound(\"Hint.ogg\");end;";
        assert_eq!(parse(cold, ""), vec![Action::BaseValue { prefix: "holod_".into() }]);
        let coin = "do local orw=wpn.baseParametr.iX; gu.acc:addReal(orw,\"moneta\"); gu.acc:updateRealStatus(false); gu:playSound(\"money.wav\");end;";
        assert_eq!(parse(coin, "<basePar>50/0/0</basePar>"), vec![Action::Premium(50)]);
        assert_eq!(parse("do gu:resetParametrs();end;", ""), vec![Action::ResetParameters]);
        let extract = "do local exp=wpn.baseParametr.iX; local force=wpn.baseParametr.iY; gu:addExp(exp, 1, 1); gu:addActiveEffectLUA(21,force,0,0,5*60);end;";
        assert_eq!(
            parse(extract, "<basePar>100/10/0</basePar>"),
            vec![Action::Experience(100), Action::Effect { effect: 21, force: 10, seconds: 300 }]
        );
    }

    #[test]
    fn picks_go_by_weight() {
        let list = choices("1/0/0/10,2/0/0/30");
        assert_eq!(pick(&list, 0.2).map(|c| c.item), Some(1));
        assert_eq!(pick(&list, 0.3).map(|c| c.item), Some(2));
        assert_eq!(pick(&[], 0.5), None);
    }
}

#[cfg(test)]
mod game_data {
    use super::*;

    /// Every script in `data/items.json` (made by af2-launcher): what each one
    /// does. Run with `cargo test -- --ignored --nocapture every_script`.
    #[test]
    #[ignore]
    fn every_script_parses() {
        let text = std::fs::read_to_string("data/items.json").expect("data/items.json");
        let items: Vec<serde_json::Value> = serde_json::from_str(&text).expect("items");
        for item in items {
            let xml = item["xml"].as_str().unwrap_or("");
            if let Some(script) = script_of(xml) {
                let actions = parse(&script, xml);
                println!("{} {:?}", item["id"], actions);
            }
        }
    }
}
