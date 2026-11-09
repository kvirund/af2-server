#!/usr/bin/env python3
"""Point the Atom Fishing 2 client at a server of our own.

The client picks its host in IFPIMPMKJIB.CFGJDNJEBCI:

    if (!isLocalServer) return "gamesrv1.af2.cryptodev.app";   # "prod" slot
    return "192.168.1.22";                                     # "local" slot

The `isLocalServer` switch is NOT reliable: Beebyte's obfuscator shuffles string
literals between the duplicated decoy methods, so the PlayerPrefs key that
actually feeds the flag at runtime is not the one named "isLocalServer". Setting
that registry value does nothing -- the client still dials the prod host. So by
default we patch the **prod** slot, and the flag stops mattering.

Each literal lives in the metadata #US heap as:

    <compressed length prefix> <UTF-16LE chars> <1 terminator byte>

Both slots are longer than "127.0.0.1", so the string is rewritten in place: the
prefix byte is updated, the new text overwrites the old, and the leftover bytes
are zeroed. No other heap offset moves, and the assembly is not strong-named,
so nothing else needs fixing.

Slot offsets are recorded in backup/patch-state.json, so a slot can be found
again even after its original text is gone.

Usage:
    python -I patch_client.py                      # patch prod slot -> 127.0.0.1
    python -I patch_client.py --slot local         # patch the local slot instead
    python -I patch_client.py --host 127.0.0.5     # different target host
    python -I patch_client.py --status             # show both slots
    python -I patch_client.py --revert             # restore the backup
"""

from __future__ import annotations

import argparse
import json
import re
import shutil
import sys
from pathlib import Path

DLL = Path(
    r"C:\Program Files (x86)\Steam\steamapps\common\AF2"
    r"\AFCGame_Data\Managed\Assembly-CSharp.dll"
)
HERE = Path(__file__).resolve().parent.parent
BACKUP = HERE / "backup" / "Assembly-CSharp.dll.orig"
STATE = HERE / "backup" / "patch-state.json"

SLOTS = {
    "prod": "gamesrv1.af2.cryptodev.app",
    "local": "192.168.1.22",
}
DEFAULT_HOST = "127.0.0.1"


def read_dll() -> bytes:
    if not DLL.is_file():
        sys.exit(f"Client assembly not found: {DLL}")
    return DLL.read_bytes()


def load_state() -> dict:
    if STATE.is_file():
        return json.loads(STATE.read_text(encoding="utf-8"))
    return {}


def save_state(state: dict) -> None:
    STATE.parent.mkdir(parents=True, exist_ok=True)
    STATE.write_text(json.dumps(state, indent=2), encoding="utf-8")


def find_unique(data: bytes, text: str) -> int | None:
    """Offset of text's UTF-16LE bytes, if present exactly once with a sane prefix."""
    needle = text.encode("utf-16-le")
    hits = [m.start() for m in re.finditer(re.escape(needle), data)]
    if len(hits) != 1:
        return None
    offset = hits[0]
    if data[offset - 1] != len(needle) + 1:
        return None
    return offset


def read_slot(data: bytes, offset: int, capacity: int) -> str:
    """Read whatever string currently sits in a slot of the given capacity."""
    length = data[offset - 1] - 1  # drop the terminator byte
    if length < 0 or length > capacity:
        return "<unreadable>"
    return data[offset : offset + length].decode("utf-16-le", errors="replace")


def locate(data: bytes, slot: str, state: dict) -> tuple[int, int]:
    """Return (offset, capacity_in_bytes) for a slot, from its text or recorded state."""
    original = SLOTS[slot]
    capacity = len(original.encode("utf-16-le")) + 1

    offset = find_unique(data, original)
    if offset is not None:
        return offset, capacity

    recorded = state.get(slot)
    if recorded:
        return recorded["offset"], recorded["capacity"]

    sys.exit(
        f"Could not locate the {slot!r} slot: its original text is gone and "
        f"nothing is recorded in {STATE}. Restore the backup with --revert."
    )


def cmd_status() -> int:
    data = read_dll()
    state = load_state()
    print(f"assembly : {DLL}")
    print(f"backup   : {'present' if BACKUP.is_file() else 'none'} ({BACKUP})")
    for slot in SLOTS:
        try:
            offset, capacity = locate(data, slot, state)
        except SystemExit:
            print(f"{slot:<6}   : not found")
            continue
        current = read_slot(data, offset, capacity)
        mark = "" if current == SLOTS[slot] else "   <-- patched"
        print(f"{slot:<6}   : \"{current}\" at 0x{offset:X} (capacity {capacity} bytes){mark}")
    return 0


def cmd_revert() -> int:
    if not BACKUP.is_file():
        sys.exit(f"No backup to restore from: {BACKUP}")
    shutil.copy2(BACKUP, DLL)
    if STATE.is_file():
        STATE.unlink()
    print(f"Restored {DLL.name} from {BACKUP}")
    return 0


def cmd_patch(slot: str, new_host: str) -> int:
    data = bytearray(read_dll())
    state = load_state()

    offset, capacity = locate(bytes(data), slot, state)
    current = read_slot(bytes(data), offset, capacity)

    if current == new_host:
        print(f'Slot {slot!r} already points at "{new_host}"; nothing to do.')
        return 0

    new_bytes = new_host.encode("utf-16-le")
    if len(new_bytes) + 1 > capacity:
        sys.exit(
            f'"{new_host}" needs {len(new_bytes) + 1} bytes but slot {slot!r} holds '
            f"only {capacity}. Use a host name of at most {(capacity - 1) // 2} characters."
        )

    # Writing the same text into both slots would make them indistinguishable.
    other = "local" if slot == "prod" else "prod"
    try:
        other_offset, other_capacity = locate(bytes(data), other, state)
        if read_slot(bytes(data), other_offset, other_capacity) == new_host:
            sys.exit(
                f'Slot {other!r} already holds "{new_host}". Give this one a different '
                f"host, or --revert first."
            )
    except SystemExit as exc:
        if "already holds" in str(exc):
            raise

    if not BACKUP.is_file():
        BACKUP.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(DLL, BACKUP)
        print(f"Backed up original to {BACKUP}")

    data[offset - 1] = len(new_bytes) + 1
    data[offset : offset + len(new_bytes)] = new_bytes
    data[offset + len(new_bytes)] = 0
    for i in range(offset + len(new_bytes) + 1, offset + capacity):
        data[i] = 0

    DLL.write_bytes(bytes(data))

    state[slot] = {"offset": offset, "capacity": capacity, "original": SLOTS[slot], "current": new_host}
    save_state(state)

    print(f'Patched {slot} slot: "{current}" -> "{new_host}" at 0x{offset:X}')
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--slot", choices=sorted(SLOTS), default="prod",
                        help="which host literal to rewrite (default: prod)")
    parser.add_argument("--host", default=DEFAULT_HOST, help=f"host to write (default {DEFAULT_HOST})")
    parser.add_argument("--revert", action="store_true", help="restore the backup")
    parser.add_argument("--status", action="store_true", help="show both slots")
    args = parser.parse_args()

    if args.revert:
        return cmd_revert()
    if args.status:
        return cmd_status()
    return cmd_patch(args.slot, args.host)


if __name__ == "__main__":
    sys.exit(main())
