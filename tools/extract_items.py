#!/usr/bin/env python3
"""Builds data/items.json from your own copy of the game.

The server needs the client's item definitions, and they are not ours to
publish, so each installation extracts them locally.

Usage:
    python -I tools/extract_items.py ["<game folder>"]
"""
import json
import re
import sys
from pathlib import Path

DEFAULT_GAME = r"C:\Program Files (x86)\Steam\steamapps\common\AF2"
OUTPUT = Path(__file__).resolve().parent.parent / "data" / "items.json"

# Item types that pile up in one cell.
STACKING_KINDS = {10, 11}


def field(block, name):
    match = re.search(rf"^{name}=(.*)$", block, re.M)
    return match[1] if match else ""


def tag(xml, name):
    match = re.search(rf"<{name}>(.*?)</{name}>", xml, re.S)
    return match[1] if match else ""


def extract(assets):
    definitions = list(re.finditer(rb"<wpn_add\b.*?</wpn_add>", assets, re.S))
    if not definitions:
        sys.exit("No item definitions found in resources.assets")

    start = assets.rfind(b"\nid=", 0, definitions[0].start())
    start = assets.rfind(b"#", 0, start)
    text = assets[start:definitions[-1].end()].decode("utf-8", "replace").replace("\r", "")

    items = []
    for block in text.split("wpnend=1"):
        identifier = re.search(r"^id=(\d+)", block, re.M)
        if not identifier:
            continue
        definition = re.search(r"<wpn_add.*?</wpn_add>", block, re.S)
        xml = definition[0] if definition else ""

        item_id = int(identifier[1])
        kind = int(field(block, "wtype") or 0)
        xml = re.sub(r">\s+<", "><", re.sub(r"\\[rn]", "", xml).strip())
        xml = xml.replace("<base>", f"<base><tid>{item_id}</tid><quality>0</quality>", 1)
        items.append({
            "id": item_id,
            "kind": kind,
            "cost": int(float(field(block, "costCr") or 0)),
            "durability": float(tag(xml, "defDur") or 1),
            "stacks": kind in STACKING_KINDS,
            "xml": xml,
        })
    return items


def main():
    game = Path(sys.argv[1] if len(sys.argv) > 1 else DEFAULT_GAME)
    assets_path = game / "AFCGame_Data" / "resources.assets"
    if not assets_path.is_file():
        sys.exit(f"Not found: {assets_path}")

    items = extract(assets_path.read_bytes())
    OUTPUT.parent.mkdir(exist_ok=True)
    OUTPUT.write_text(json.dumps(items, ensure_ascii=False), encoding="utf-8")
    print(f"{len(items)} items written to {OUTPUT}")


if __name__ == "__main__":
    main()
