"""Refresh the offline HTML and ARIA catalog from fixed standards snapshots."""

import re
from collections import defaultdict
from html import unescape
from pathlib import Path
from urllib.request import urlopen

SOURCE = "https://html.spec.whatwg.org/review-drafts/2026-07/"
ARIA_SOURCE = "https://www.w3.org/TR/2023/REC-wai-aria-1.2-20230606/"
CATALOG = (
    Path(__file__).resolve().parents[1]
    / "src/snippet_window/completion/language/markup/html-data.txt"
)
NAME = re.compile(r"[a-z][a-z0-9-]*")
KEYWORD = r'"([a-zA-Z0-9_./-]+)"'
ENUMERATION = re.compile(rf'{KEYWORD}(?:\s*;\s*{KEYWORD})*(?:\s*;\s*the empty string)?')


def plain(html):
    return " ".join(unescape(re.sub(r"<[^>]*>", "", html)).split())


def code_names(cell):
    return [
        name
        for code in re.findall(r"<code\b[^>]*>(.*?)</code>", cell, flags=re.DOTALL)
        if NAME.fullmatch(name := plain(code))
    ]


def rows(document, start):
    table = document.split(start, 1)[1].split("</table>", 1)[0]
    body = table.split("<tbody>", 1)[1]
    for row in re.split(r"<tr\b[^>]*>", body)[1:]:
        yield re.split(r"<t[hd]\b[^>]*>", row)[1:]


def snapshot(document):
    tags = {}
    for cells in rows(document, "<caption>List of elements</caption>"):
        for tag in code_names(cells[0]):
            tags[tag] = set(code_names(cells[5]))
    globals_ = set()
    values = defaultdict(set)
    for index in ("attributes-1", "ix-event-handlers"):
        for name_cell, scope, _, value_cell in rows(document, f"<table id={index}>"):
            selectors = set(code_names(scope)) & tags.keys()
            if plain(scope).startswith("HTML elements"):
                selectors.add("*")
            for attribute in code_names(name_cell):
                for tag in selectors:
                    if tag == "*":
                        globals_.add(attribute)
                    else:
                        tags[tag].add(attribute)
                    value = plain(value_cell).removesuffix("*").rstrip()
                    if ENUMERATION.fullmatch(value):
                        values[tag, attribute].update(re.findall(KEYWORD, value))
    if len(tags) < 100 or len(globals_) < 50:
        raise ValueError("The HTML index did not contain the expected tables")
    yield "# HTML Living Standard, WHATWG July 2026 review draft"
    yield f"# {SOURCE}#index"
    yield "# Vocabulary and explicit keyword values extracted from the index."
    yield "# Regenerate with python scripts/update-html-completion-data.py"
    yield "G\t" + " ".join(sorted(globals_))
    for tag, attrs in sorted(tags.items()):
        yield f"T\t{tag}\t{' '.join(sorted(attrs))}".rstrip("\t")
    for (tag, attr), words in sorted(values.items()):
        yield f"V\t{tag}\t{attr}\t{' '.join(sorted(words))}"


def aria_snapshot(document):
    roles = set()
    attributes = {}
    sections = re.findall(
        r'<section class="((?:role|state|property)\b[^"]*)" id="([^"]+)">'
        r"(.*?)</section>",
        document,
        flags=re.DOTALL,
    )
    for classes, name, section in sections:
        if not NAME.fullmatch(name) or "deprecated" in classes.split():
            continue
        if classes.startswith("role "):
            if '<td class="role-abstract">True</td>' not in section:
                roles.add(name)
        elif name.startswith("aria-"):
            words = {
                word
                for cell in re.findall(
                    r'<th class="value-name"[^>]*>(.*?)</th>', section, flags=re.DOTALL
                )
                if NAME.fullmatch(word := plain(cell).removesuffix(" (default)"))
            }
            attributes[name] = sorted(words)
    if len(roles) < 80 or len(attributes) < 40:
        raise ValueError("The ARIA recommendation did not contain the expected vocabulary")
    yield "# WAI-ARIA 1.2, W3C Recommendation (6 June 2023)"
    yield f"# {ARIA_SOURCE}"
    for attribute, words in sorted(attributes.items()):
        yield f"A\t{attribute}\t{' '.join(words)}".rstrip("\t")
    yield "V\t*\trole\t" + " ".join(sorted(roles))


def download(url):
    with urlopen(url, timeout=30) as response:
        return response.read().decode("utf-8")


if __name__ == "__main__":
    rows = [*snapshot(download(SOURCE)), *aria_snapshot(download(ARIA_SOURCE))]
    CATALOG.write_text("\n".join(rows) + "\n", encoding="utf-8", newline="\n")
