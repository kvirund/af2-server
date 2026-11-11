"""Which items a player can get on this server, and why the others cannot be had.

Usage: python tools/reachability.py <game folder> [report.md]
Run from the repository root after af2-launcher has made data/items.json and
data/recipes.json. The game folder is the one with AFCGame_Data in it.

An item counts as obtainable when a shop of some base sells it (config/shops.json),
it is in the starter kit, it comes up with fish or from a gather point
(config/settings.json "loot", config/gathering.json), an obtainable item refines
into it (`<refine>` in the item's description), or a recipe the server can make
produces it from obtainable things. A recipe is usable when it is known from
the start or its recipe item is obtainable. Items whose name or hint says they
are a test are listed apart.
"""

import json, re, sys
from collections import defaultdict

game, out = sys.argv[1], (sys.argv[2] if len(sys.argv) > 2 else None)


def commented(path):
    """JSON with // comments, as the server reads it."""
    text, result, quoted, i = open(path, encoding="utf-8").read(), [], False, 0
    while i < len(text):
        c = text[i]
        if quoted:
            result.append(c)
            if c == "\\":
                result.append(text[i + 1]); i += 1
            elif c == '"':
                quoted = False
        elif c == '"':
            quoted = True; result.append(c)
        elif text.startswith("//", i):
            while i < len(text) and text[i] != "\n":
                i += 1
            continue
        else:
            result.append(c)
        i += 1
    return json.loads("".join(result))


items = {i["id"]: i for i in json.load(open("data/items.json", encoding="utf-8"))}
recipes = json.load(open("data/recipes.json", encoding="utf-8"))
settings = commented("config/settings.json")
shops = commented("config/shops.json")
gathering = commented("config/gathering.json")
fish = commented("config/fish.json")

lang = open(f"{game}/AFCGame_Data/StreamingAssets/Localization/Russian/WpnLang.xml", encoding="utf-8-sig").read()
names, hints = {}, {}
for wid, body in re.findall(r'<weapon id="(\d+)">(.*?)</weapon>', lang, re.S):
    name = re.search(r"<name>(.*?)</name>", body, re.S)
    info = re.search(r"<info>(.*?)</info>", body, re.S)
    names[int(wid)] = name.group(1).strip() if name else "?"
    hints[int(wid)] = info.group(1).strip() if info else ""
kinds = {int(k): v for k, v in re.findall(r'<type id="(\d+)" name="([^"]*)"', lang)}


def tag(item, name):
    m = re.search(rf"<{name}>([^<]*)</{name}>", item["xml"])
    return m.group(1) if m else ""


def is_test(i):
    text = (names.get(i, "") + " " + hints.get(i, "")).lower()
    return bool(re.search(r"тестов|\bтест\b|\btest|\[sysname\]", text))


# Where things come from.
sources = defaultdict(set)
shop = settings.get("shop", {})
hidden_kinds, hidden_items = set(shop.get("hiddenKinds", [])), set(shop.get("hiddenItems", []))
prices = {int(i): cost for i, cost in shop.get("prices", {}).items()}
for base, ids in shops["bases"].items():
    for i in ids:
        item = items.get(i)
        if item and prices.get(i, item["cost"]) > 0 and item["kind"] not in hidden_kinds and i not in hidden_items:
            sources[i].add(f"магазин базы {base}")
for kit in settings["starterKit"]:
    sources[kit["item"]].add("стартовый набор")
loot = settings.get("loot", {})
for entry in loot.get("items", []):
    sources[entry["item"]].add("улов с рыбой")
for base, card in loot.get("baseCards", {}).items():
    sources[card].add(f"улов с рыбой на базе {base}")
for rid, res in gathering["resources"].items():
    for drop in res["drops"]:
        sources[drop["item"]].add(f"сбор: {res['name']}")

caught = {s for ids in fish["locations"].values() for s in (e if isinstance(e, int) else e.get("id", e.get("species")) for e in ids)}
mutants = set(settings["fishing"].get("mutantSpecies", []))


def fish_ok(ing):
    species, other = int(ing[0]), int(ing[5])
    if other:
        return False, "особое условие на рыбу"
    if species > 0:
        return species in caught, f"вид {species} не ловится"
    if species == 0:
        return bool(caught - mutants), "нет обычной рыбы"
    return bool(caught & mutants), "мутанты не ловятся"


teaches = {}
for i, item in items.items():
    if item["kind"] == 28:
        par = tag(item, "basePar").split("/")
        if len(par) >= 2 and par[0] in ("3", "7"):
            teaches.setdefault(int(par[1]), []).append(i)

refines = {i: [int(part.split("/")[0]) for part in tag(item, "refine").split(",") if part.strip()]
           for i, item in items.items() if tag(item, "refine").strip()}

reach = set(sources)
usable = set()
changed = True
while changed:
    changed = False
    for i, made in refines.items():
        if i in reach:
            for m in made:
                if m not in reach:
                    reach.add(m); changed = True
                    sources[m].add(f"переработка {i}")
    for r in recipes:
        rid = r["id"]
        known = r["default"] or any(t in reach for t in teaches.get(rid, []))
        if not known or r["kind"] != 0 or r["profession"]:
            continue
        ok = True
        for ing in r["ingredients"]:
            if ing[2] == 0:
                ok &= int(ing[0]) in reach and int(ing[0]) != 1309
            else:
                ok &= fish_ok(ing)[0]
        if not ok:
            continue
        for made in (r["result"], r.get("crit_result", [0, 0, 0])):
            m = int(made[0])
            if m > 0 and m not in reach:
                reach.add(m); changed = True
                sources[m].add(f"рецепт {rid}")
        usable.add(rid)


def why(i):
    item, reasons = items[i], []
    made_by = [r for r in recipes if int(r["result"][0]) == i or int(r.get("crit_result", [0])[0]) == i]
    for r in made_by:
        rid, problems = r["id"], []
        if r["kind"] != 0:
            problems.append("конструктор, сервер его не делает")
        if r["profession"]:
            problems.append(f"нужен навык {r['profession']} ур. {r['profession_level']}")
        if not r["default"]:
            t = teaches.get(rid, [])
            if not t:
                problems.append("рецепт ничему не учится")
            elif not any(x in reach for x in t):
                problems.append("рецепт недоступен: " + ", ".join(f"{x} {names.get(x, '?')}" for x in t))
        for ing in r["ingredients"]:
            if ing[2] == 0 and int(ing[0]) not in reach:
                problems.append(f"нет {int(ing[0])} {names.get(int(ing[0]), '?')}")
            elif ing[2] != 0:
                ok, reason = fish_ok(ing)
                if not ok:
                    problems.append(reason)
        reasons.append(f"рецепт {rid} (станок {r['station']}): " + "; ".join(problems or ["?"]))
    if int(tag(item, "pricePt") or 0) > 0:
        reasons.append(f"цена в премиум-валюте {tag(item, 'pricePt')}")
    if item["cost"] > 0 and not made_by:
        reasons.append("есть цена, но ни одна база не продаёт")
    return reasons or ["нет источника"]


lines = ["# Достижимость предметов", ""]
missing = sorted(i for i in items if i not in reach and not is_test(i))
tests = sorted(i for i in items if is_test(i))
lines.append(f"Всего предметов {len(items)}, достижимо {len(reach & set(items))}, недостижимо {len(missing)}, тестовых {len(tests)}.")
lines.append(f"Рецептов {len(recipes)}, можно сделать {len(usable)}.")
by_kind = defaultdict(list)
for i in missing:
    by_kind[items[i]["kind"]].append(i)
for kind in sorted(by_kind):
    lines += ["", f"## {kind} {kinds.get(kind, '?')} ({len(by_kind[kind])})", ""]
    for i in by_kind[kind]:
        lines.append(f"- {i} {names.get(i, '?')}: " + " | ".join(why(i)))
lines += ["", "## Тестовые (не считаются)", ""]
lines += [f"- {i} {names.get(i, '?')}" for i in tests]
report = "\n".join(lines) + "\n"
if out:
    open(out, "w", encoding="utf-8").write(report)
else:
    sys.stdout.reconfigure(encoding="utf-8")
    print(report)
