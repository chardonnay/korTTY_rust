"""Refresh SVG and MathML completion vocabulary from fixed W3C snapshots."""

import re
from pathlib import Path
from urllib.request import urlopen

SVG = "https://www.w3.org/TR/2026/CR-SVG2-20261006/"
MATH = "https://www.w3.org/TR/2025/CR-mathml-core-20250624/"
CATALOG = (
    Path(__file__).resolve().parents[1]
    / "src/snippet_window/completion/language/markup/foreign-data.txt"
)
NAME = r"[a-zA-Z][a-zA-Z0-9:-]*"


def fetch(url):
    with urlopen(url, timeout=30) as response:
        return response.read().decode("utf-8")


def spans(document, kind):
    pattern = rf'<span class="{kind}">[^<]*<a\b[^>]*><span>({NAME})</span>'
    return re.findall(pattern, document)


def svg_vocabulary():
    elements = {tag: set() for tag in spans(fetch(SVG + "eltindex.html"), "element-name")}
    index = fetch(SVG + "attindex.html")
    table = index.split('<table class="proptable attrtable">', 1)[1].split("</table>", 1)[0]
    for row in re.findall(r"<tr\b[^>]*>(.*?)</tr>", table, flags=re.DOTALL):
        attrs = [name for name in spans(row, "attr-name") if not name.startswith("aria-")]
        for tag in spans(row, "element-name"):
            if tag in elements:
                elements[tag].update(attrs)
    presentation = index.split('id="PresentationAttributes"', 1)[1]
    globals_ = set(spans(presentation, "attr-name"))
    globals_.update(set.intersection(*elements.values()))
    globals_.update(("class", "id", "role", "style", "xmlns"))
    return elements, globals_


def math_vocabulary():
    document = fetch(MATH)
    definitions = re.findall(rf"<dfn\b([^>]+)>({NAME})</dfn>", document)
    elements = {
        name: set()
        for fields, name in definitions
        if 'data-dfn-type="element"' in fields
    }
    globals_ = {"role", "xmlns"}
    for fields, name in definitions:
        if 'data-dfn-type="element-attr"' not in fields:
            continue
        scope = re.search(r'data-dfn-for="([^"]+)"', fields)
        if scope:
            for tag in scope[1].split(","):
                if tag.strip() in elements:
                    elements[tag.strip()].add(name)
        else:
            globals_.add(name)
    return elements, globals_


def snapshot():
    for kind, source, vocabulary, minimum in (
        ("S", SVG, svg_vocabulary, 50),
        ("M", MATH, math_vocabulary, 25),
    ):
        elements, globals_ = vocabulary()
        if len(elements) < minimum or sum(map(len, elements.values())) < 30:
            raise ValueError(f"The {kind} index did not contain the expected vocabulary")
        yield f"# {source}"
        yield f"{kind}\t*\t" + " ".join(sorted(globals_))
        for tag, attributes in sorted(elements.items()):
            yield f"{kind}\t{tag}\t{' '.join(sorted(attributes - globals_))}".rstrip("\t")


if __name__ == "__main__":
    lines = ["# Regenerate with python scripts/update-foreign-completion-data.py", *snapshot()]
    CATALOG.write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")
