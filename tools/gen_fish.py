"""Generates config/fish.json.

Inputs (paths on the command line):
  waters.json   sections of the Steam guide "Водоёмы, обитатели, наживки, ресурсы"
                (species weights, traits, per-location species, bait lists)
  chart.txt     text of the guide "Клевалка от Элиаса" (depth rules, time of day)
  fish.json     names and model ids taken from the client
  items.json    item definitions taken from the client
"""
import json, re, sys

waters_path, chart_path, fish_path, items_path, out_path = sys.argv[1:6]
sys.stdout.reconfigure(encoding="utf-8")

waters = json.load(open(waters_path, encoding="utf-8"))
chart = open(chart_path, encoding="utf-8").read()
client = json.load(open(fish_path, encoding="utf-8"))
items = json.load(open(items_path, encoding="utf-8"))


def norm(s):
    return re.sub(r"\s+", " ", s.replace("ё", "е").replace("Ё", "Е").lower()).strip()


ids_by_name = {norm(v): int(k) for k, v in client["names"].items()}
ALIASES = {
    "сом": 98,
    "миньон-ктулxy": 141,
    "миньон-ктулху": 141,
    "щука": 128,
    "селедь каспийская": None,
    "подкаменщик сибирский": None,
}
modelled = {int(k) for k in client["models"]}


def species_id(name):
    n = norm(name)
    return ALIASES[n] if n in ALIASES else ids_by_name.get(n)


# guide location name -> client location id (maps.txt)
LOCATIONS = {
    "затон": 1, "серебронкс": 2, "серебронск": 2, "протока": 3, "мост": 4,
    "лодочная база": 5, "лодочная": 5, "склон": 6,
    "спокойный залив": 21, "заправка": 31, "заправка (трасса)": 31, "холмы": 33,
    "лесной пляж": 41, "кулгу": 42, "камышовое": 43, 'катер "сметливый"': 44,
    "устье": 45, "р.каменка": 46, "каменный ручей": 47,
    "ферма": 61, "ферма (лавочка, мосток)": 61, "болотная яма": 62, "река ипа": 63,
    "средний пруд": 64, "верхний пруд": 65, "володин поворот": 66,
    "старое русло": 72, "заливной луг": 73, "лодочный пляж": 74, "заросший омут": 75,
    "д.залесье": 76, "д. залесье": 76, "песчаный обрыв": 77, "огороды": 78, "тихая заводь": 79,
    "каштымская коса": 82, "наливная пристань": 83, "красный брод": 84, "коряжник": 85,
}

keys = [k for k, _ in waters]
species_start = keys.index("ОБИТАТЕЛИ")
baits_start = keys.index("НАЖИВКИ")
resources_start = keys.index("РЕСУРСЫ И РАСТИТЕЛЬНОСТЬ")


def grams(text, label):
    m = re.search(re.escape("[" + label + "]") + r"\s*(?:(\d+)\s*кг)?\s*(?:(\d+)\s*гр)?", text)
    if not m or (m[1] is None and m[2] is None):
        return 0
    return int(m[1] or 0) * 1000 + int(m[2] or 0)


# Bait name -> item template id. Lures are named differently in the guide, so
# fall back to requiring every word of the guide's name in the item's name.
def lure_key(s):
    s = norm(s).replace("м1", "m1").replace("-ice", " ice")
    return re.sub(r"-([123])\b", lambda m: "-" + "i" * int(m[1]), s)


item_by_name = {lure_key(i["name"]): i["id"] for i in items}


def bait_tid(name):
    n = lure_key(name)
    if n in item_by_name:
        return item_by_name[n]
    words = n.split()
    hits = [i for k, i in item_by_name.items() if all(w in k for w in words)]
    return hits[0] if len(hits) == 1 else None


unmatched_baits = set()
baits_by_species = {}
for name, body in waters[baits_start + 1:resources_start]:
    tid = bait_tid(name)
    if tid is None:
        unmatched_baits.add(name)
        continue
    for fish in re.findall(r"\[([^\]]+)\]", body):
        sid = species_id(fish)
        if sid is not None:
            baits_by_species.setdefault(sid, set()).add(tid)

# Depth and time-of-day rules from the second guide, keyed by client species.
RULE = re.compile(
    r"(?m)^\s*([А-ЯЁA-Z][^\n]*?)\s+- [^\n]*\n"
    r"Наживк[аи]\s*-[^\n]*\n"
    r"Локаци[яи]\s*-[^\n]*\n"
    r"Глубина\s*-([^\n]*)\n"
    r"(?:Примечани[яе]\s*-?([^\n]*))?"
)
rules = {}
for m in RULE.finditer(chart):
    sid = species_id(m[1])
    if sid is not None:
        rules[sid] = (m[2].strip(), (m[3] or "").strip())


def centimetres(value, unit):
    return float(value.replace(",", ".")) * (100 if unit == "м" else 1)


def depth_range(text):
    t = text.lower()
    lo = hi = 0
    m = re.search(r"\bот\s*(\d+(?:[.,]\d+)?)\s*(см|м)\b", t) or re.search(r"(\d+(?:[.,]\d+)?)\s*(см|м)\s*\+", t)
    if m:
        lo = centimetres(m[1], m[2])
    m = re.search(r"\bдо\s*(\d+(?:[.,]\d+)?)\s*(см|м)\b", t)
    if m:
        hi = centimetres(m[1], m[2])
    return int(lo), int(hi)


def night_factor(text):
    t = text.lower()
    if re.search(r"ночью\s+не\s+клю", t):
        return 0.0
    if re.search(r"ночью\s+(очень\s+)?(редко|плохо|хуже)", t):
        return 0.25
    if re.search(r"(чаще\s+ночью|ночью\s+(лучше|активн)|в основном ночью|активн\w*\s+ночью)", t):
        return 2.0
    return 1.0


species = {}
dropped = []
for name, text in waters[species_start + 1:baits_start]:
    sid = species_id(name)
    small, large, trophy = (grams(text, f"{c} вес от") for c in ("Маленький", "Крупный", "Трофейный"))
    span = re.search(r"\[Маленький вес от\]\s*(\d+)\s*-\s*(\d+)\s*гр", text)
    if span:
        small, trophy = int(span[1]), int(span[2])
        large = round((small + trophy) / 2)
    if sid is None or sid not in modelled or not any((small, large, trophy)):
        dropped.append(name)
        continue
    # A guide column can be missing; rebuild it from its neighbours.
    if not large:
        large = round((small * trophy) ** 0.5) if small and trophy else round(small * 2) if small else round(trophy * 0.5)
    small = small or round(large / 3)
    trophy = max(trophy or round(large * 2.5), large + 1)
    traits = {t.strip() for t in re.findall(r"\[\]\s*([А-ЯЁ][а-яё ]+)", text)}
    depth, notes = rules.get(sid, ("", ""))
    lo, hi = depth_range(depth)
    near_bottom = "Донная" in traits or bool(re.search(r"дн[ао]", depth.lower()))
    size = 6 if "Мелкая" in traits else 2 if "Крупная" in traits else 1 if "Гигантская" in traits else 4
    share = size * (0.6 if "Мутант" in traits else 1) * (0.6 if "Ценная" in traits else 1) * (1.5 if "Сорная" in traits else 1)
    species[sid] = dict(
        sid=sid, name=client["names"][str(sid)], small=small, large=large, trophy=trophy,
        lo=lo, hi=hi, near=near_bottom, deep="Любит глубину" in traits,
        night=night_factor(depth + " " + notes), hardy="Выносливая" in traits, share=round(share, 2),
        baits=sorted(baits_by_species.get(sid, ())),
    )

# Species per location: the location sections are the full lists.
per_loc = {}
unknown_loc = set()
for name, body in waters[:species_start]:
    loc = LOCATIONS.get(norm(name))
    if loc is None:
        if re.search(r"\[[^\]]+\]", body):
            unknown_loc.add(name)
        continue
    for fish in re.findall(r"\[([^\]]+)\]", body):
        sid = species_id(fish)
        if sid in species:
            per_loc.setdefault(loc, set()).add(sid)

# No record survives for these waters, so they are invented: real fish for the
# region (maps.txt names the Emerald Lake's species), or a neighbouring spot.
INVENTED = {
    51: [105, 95, 65, 50, 66, 77], 52: [105, 95, 65, 48, 69, 103], 53: [65, 50, 48, 74, 95], 54: [105, 52, 66, 77, 69, 103],
    91: [1, 30, 75, 112, 77, 66, 28, 84],
    92: [19, 129, 33, 28, 77, 66, 112, 128, 105],
    93: [37, 54, 52, 78, 26, 128, 66, 61],
    94: [37, 54, 16, 87, 98, 128, 61, 84, 53, 72],
}
COPIED = {22: 21, 23: 21, 32: 31, 34: 31, 71: 74, 78: 73, 81: 83, 86: 85, 87: 85, 88: 85}
for loc, fish in INVENTED.items():
    per_loc[loc] = {f for f in fish if f in species}
    skipped = [f for f in fish if f not in species]
    if skipped:
        print("invented location", loc, "skips", skipped)
for loc, source in COPIED.items():
    per_loc[loc] = set(per_loc[source])

def species_json(s):
    return json.dumps(dict(
        id=s["sid"], name=s["name"], smallGrams=s["small"], largeGrams=s["large"], trophyGrams=s["trophy"],
        minDepthCm=s["lo"], maxDepthCm=s["hi"], nearBottom=s["near"], likesDepth=s["deep"],
        nightFactor=s["night"], hardy=s["hardy"], share=s["share"], baits=s["baits"],
    ), ensure_ascii=False)


species_lines = ",\n".join("    " + species_json(s) for s in sorted(species.values(), key=lambda x: x["sid"]))
location_lines = ",\n".join(f'    "{loc}": {json.dumps(sorted(per_loc[loc]))}' for loc in sorted(per_loc))
open(out_path, "w", encoding="utf-8", newline="\n").write(
    "{\n  \"species\": [\n" + species_lines + "\n  ],\n  \"locations\": {\n" + location_lines + "\n  }\n}\n"
)

print("species", len(species), "dropped", len(dropped), dropped)
print("locations", {k: len(v) for k, v in sorted(per_loc.items())})
print("unknown location sections", sorted(unknown_loc))
print("baits unmatched", len(unmatched_baits), sorted(unmatched_baits)[:30])
print("with depth rules", sum(1 for s in species.values() if s["lo"] or s["hi"]),
      "near bottom", sum(s["near"] for s in species.values()),
      "night!=1", sum(1 for s in species.values() if s["night"] != 1),
      "no baits", [s["name"] for s in species.values() if not s["baits"]])
for sid in (77, 78, 66, 37, 52):
    print(species.get(sid))
