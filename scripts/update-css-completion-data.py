"""Refresh the compact, offline CSS completion data from a pinned MDN snapshot."""

import json
import re
from pathlib import Path
from urllib.request import urlopen

REVISION = "7d81f7981cc1c7074b887bb2d16aab66b493dc13"
SOURCE = f"https://raw.githubusercontent.com/mdn/data/{REVISION}/css"
CATALOG = (
    Path(__file__).resolve().parents[1]
    / "src/snippet_window/completion/language/markup/css-data.txt"
)
STATUSES = {"standard", "experimental"}
IDENTIFIER = re.compile(r"[a-zA-Z][a-zA-Z0-9-]*")


def load(name):
    with urlopen(f"{SOURCE}/{name}.json", timeout=30) as response:
        return json.load(response)


def keywords(syntax):
    # Use explicitly written identifiers from value definitions, omitting
    # references to other grammars and arguments of functional notations.
    syntax = re.sub(r"<[^>]*>|[\w-]+\([^)]*\)", "", syntax)
    return " ".join(dict.fromkeys(IDENTIFIER.findall(syntax)))


def snapshot():
    yield f"# MDN data {REVISION} (CC0-1.0)"
    yield f"# https://github.com/mdn/data/tree/{REVISION}/css"
    yield "# Regenerate with python scripts/update-css-completion-data.py"
    for name, entry in sorted(load("properties").items()):
        if entry["status"] in STATUSES and IDENTIFIER.fullmatch(name):
            yield f"P\t{name}\t{keywords(entry['syntax'])}".rstrip("\t")
    for name, entry in sorted(load("at-rules").items()):
        if entry["status"] in STATUSES and IDENTIFIER.fullmatch(name[1:]):
            yield f"A\t{name[1:]}"
            for descriptor, definition in sorted(entry.get("descriptors", {}).items()):
                if definition.get("status", "standard") in STATUSES:
                    yield f"D\t{name}\t{descriptor}\t{keywords(definition['syntax'])}".rstrip("\t")
    for selector, entry in sorted(load("selectors").items()):
        if entry["status"] not in STATUSES or not selector.startswith(":"):
            continue
        marker, prefix = ("E", "::") if selector.startswith("::") else ("C", ":")
        name = selector.removeprefix(prefix)
        if IDENTIFIER.fullmatch(name.removesuffix("()")):
            yield f"{marker}\t{name}"


if __name__ == "__main__":
    CATALOG.parent.mkdir(exist_ok=True)
    CATALOG.write_text("\n".join(snapshot()) + "\n", encoding="utf-8", newline="\n")
