(() => {
    "use strict";

    const byId = (id) => document.getElementById(id);
    const entries = Array.from(document.querySelectorAll(".entry"));
    const days = Array.from(document.querySelectorAll(".day"));
    const MAX_HITS = 10000;
    const PAGE_ROWS = 500;

    function listen(id, event, handler) {
        byId(id).addEventListener(event, handler);
    }

    function navigateIndex(index, size, step) {
        if (index < 0) {
            return step > 0 ? 0 : size - 1;
        }
        return (index + step + size) % size;
    }

    function reveal(entry) {
        document.querySelector(".entry.current")?.classList.remove("current");
        entry.classList.add("current");
        entry.scrollIntoView({ block: "center" });
    }

    function escapePattern(text) {
        return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    }

    function casefold(text) {
        return text.toLowerCase();
    }

    function setFont(delta) {
        const root = document.documentElement;
        const scale = delta === 0 ? 0 : Math.max(-2, Math.min(3, Number(root.dataset.scale) + delta));
        root.dataset.scale = String(scale);
        byId("font-smaller").disabled = scale === -2;
        byId("font-larger").disabled = scale === 3;
    }

    function bindAppearance() {
        listen("theme", "click", () => {
            const root = document.documentElement;
            const themes = ["system", "light", "dark"];
            const index = themes.indexOf(root.dataset.theme);
            const theme = themes[(index + 1) % themes.length];
            root.dataset.theme = theme;
            byId("theme").textContent = `Theme: ${theme}`;
        });
        listen("font-smaller", "click", () => setFont(-1));
        listen("font-reset", "click", () => setFont(0));
        listen("font-larger", "click", () => setFont(1));
    }

    function searchableNodes(entry) {
        const walker = document.createTreeWalker(entry, NodeFilter.SHOW_TEXT);
        const nodes = [];
        for (let node = walker.nextNode(); node; node = walker.nextNode()) {
            if (node.parentElement?.closest("[data-search]")) {
                nodes.push(node);
            }
        }
        return nodes;
    }

    function searchTerms(value) {
        const tokens = Array.from(value.slice(0, 512).matchAll(/"([^"]+)"|(\S+)/g), (token) => {
            return casefold((token[1] ?? token[2]).trim());
        });
        return Array.from(new Set(tokens.filter(Boolean)))
            .sort((left, right) => right.length - left.length);
    }

    function emphasize(node, index, length) {
        const before = document.createTextNode(node.data.slice(0, index));
        const mark = document.createElement("mark");
        const after = document.createTextNode(node.data.slice(index + length));
        mark.textContent = node.data.slice(index, index + length);
        node.replaceWith(before, mark, after);
        for (let parent = mark.parentElement; parent; parent = parent.parentElement) {
            if (parent.tagName === "DETAILS") {
                parent.open = true;
            }
        }
        return () => {
            before.replaceWith(node);
            mark.remove();
            after.remove();
        };
    }

    function bindJournalSearch() {
        const panel = byId("search-controls");
        const input = byId("journal-search");
        const onlyMatches = byId("only-matches");
        const searchable = entries.map((entry) => {
            const nodes = searchableNodes(entry);
            return { entry, nodes, text: casefold(nodes.map((node) => node.data).join(" ")) };
        });
        let hits = [];
        let matchingEntries = 0;
        let current = -1;
        let limited = false;
        let restore = null;
        let debounce;

        function status() {
            const count = limited ? `${hits.length}+` : hits.length;
            const active = current >= 0 ? `${current + 1} / ${count}` : `${count} matches`;
            byId("journal-count").textContent = input.value.trim()
                ? `${active} · ${matchingEntries} of ${entries.length} entries` : "";
            byId("journal-prev").disabled = hits.length === 0;
            byId("journal-next").disabled = hits.length === 0;
        }

        function search() {
            clearTimeout(debounce);
            debounce = undefined;
            restore?.();
            restore = null;
            current = -1;
            hits = [];
            limited = false;
            matchingEntries = 0;
            const terms = searchTerms(input.value);
            const pattern = terms.length ? new RegExp(terms.map(escapePattern).join("|"), "giu") : null;
            for (const { entry, nodes, text } of searchable) {
                const matches = terms.every((term) => text.includes(term));
                entry.hidden = Boolean(terms.length && onlyMatches.checked && !matches);
                if (!pattern || !matches) {
                    continue;
                }
                matchingEntries += 1;
                if (hits.length === MAX_HITS) {
                    limited = true;
                    continue;
                }
                for (const node of nodes) {
                    for (const match of node.data.matchAll(pattern)) {
                        hits.push({ entry, node, index: match.index, length: match[0].length });
                        if (hits.length === MAX_HITS) {
                            limited = true;
                            break;
                        }
                    }
                    if (limited) {
                        break;
                    }
                }
            }
            for (const day of days) {
                day.hidden = !day.querySelector(".entry:not([hidden])");
            }
            status();
        }

        function move(step) {
            if (debounce !== undefined) {
                search();
            }
            if (!hits.length) {
                return;
            }
            restore?.();
            current = navigateIndex(current, hits.length, step);
            const hit = hits[current];
            restore = emphasize(hit.node, hit.index, hit.length);
            reveal(hit.entry);
            status();
        }

        function focusSearch() {
            panel.open = true;
            input.focus();
            input.select();
        }

        input.addEventListener("input", () => {
            clearTimeout(debounce);
            debounce = setTimeout(() => {
                debounce = undefined;
                search();
            }, 100);
        });
        input.addEventListener("keydown", (event) => {
            if (event.key === "Enter") {
                event.preventDefault();
                move(event.shiftKey ? -1 : 1);
            }
        });
        listen("only-matches", "change", search);
        listen("journal-prev", "click", () => move(-1));
        listen("journal-next", "click", () => move(1));
        for (const tag of document.querySelectorAll(".tag")) {
            tag.addEventListener("click", () => {
                input.value = `"${tag.textContent.slice(0, 510).replaceAll('"', "")}"`;
                search();
                focusSearch();
            });
        }
        document.addEventListener("keydown", (event) => {
            if ((event.ctrlKey || event.metaKey) && !event.altKey && casefold(event.key) === "f") {
                if (byId("image-dialog").open || !byId("capture-panel").hidden) {
                    return;
                }
                event.preventDefault();
                focusSearch();
            } else if (event.key === "F3" && panel.open) {
                event.preventDefault();
                move(event.shiftKey ? -1 : 1);
            }
        });
        status();
    }

    function bindMarkers() {
        const panel = byId("marker-controls");
        if (!panel) {
            return;
        }
        const select = byId("marker");
        let currentEntry = null;
        function move(step) {
            const markers = entries.filter((entry) => !entry.hidden && entry.dataset.marker
                && (!select.value || entry.dataset.marker === select.value));
            if (!markers.length) {
                byId("marker-count").textContent = "No marked entries";
                return;
            }
            const index = navigateIndex(markers.indexOf(currentEntry), markers.length, step);
            currentEntry = markers[index];
            byId("marker-count").textContent = `${index + 1} / ${markers.length}`;
            reveal(currentEntry);
        }
        select.addEventListener("change", () => {
            currentEntry = null;
            move(1);
        });
        listen("marker-prev", "click", () => move(-1));
        listen("marker-next", "click", () => move(1));
        document.addEventListener("keydown", (event) => {
            if (!event.altKey || event.ctrlKey || event.metaKey || event.target.matches?.("input, select")) {
                return;
            }
            if (event.key === "ArrowUp" || event.key === "ArrowDown") {
                event.preventDefault();
                panel.open = true;
                move(event.key === "ArrowUp" ? -1 : 1);
            }
        });
    }

    function parseClock(text) {
        const match = /^(\d{1,2})(?:[:.h ](\d{1,2})|(\d{2}))?$/i.exec(text);
        if (!match) {
            return null;
        }
        const hour = Number(match[1]);
        const minute = Number(match[2] ?? match[3] ?? 0);
        return hour < 24 && minute < 60 ? { hour, minute } : null;
    }

    function civilMilliseconds(year, month, day, { hour, minute }, seconds = 0) {
        const civil = new Date(0);
        civil.setUTCFullYear(year, month - 1, day);
        civil.setUTCHours(hour, minute, seconds, 0);
        if (civil.getUTCMonth() !== month - 1 || civil.getUTCDate() !== day) {
            return null;
        }
        return civil.getTime();
    }

    function parseJump(text) {
        const value = text.trim().replace(/\s+/g, " ");
        const iso = /^(\d{4})-(\d{1,2})-(\d{1,2})[ T](.+)$/.exec(value);
        const european = /^(\d{1,2})\.(\d{1,2})\.(\d{4})?\s*(.+)$/.exec(value);
        const clock = parseClock(iso?.[4] ?? european?.[4] ?? value);
        if (!clock) {
            return null;
        }
        if (iso) {
            const target = civilMilliseconds(Number(iso[1]), Number(iso[2]), Number(iso[3]), clock);
            return target === null ? null : { target };
        }
        if (european) {
            const year = european[3] ? Number(european[3]) : new Date().getFullYear();
            const target = civilMilliseconds(year, Number(european[2]), Number(european[1]), clock);
            return target === null ? null : { target };
        }
        return { clock };
    }

    function bindTime() {
        const input = byId("time-search");
        const status = byId("time-status");
        const timed = entries.flatMap((entry) => {
            const match = /^(\d{4})-(\d{2})-(\d{2}) (\d{2}):(\d{2}):(\d{2})$/.exec(entry.dataset.time ?? "");
            if (!match) {
                return [];
            }
            const [, year, month, day, hour, minute, second] = match.map(Number);
            const clock = { hour, minute };
            const timestamp = civilMilliseconds(year, month, day, clock, second);
            return [{ entry, timestamp, seconds: hour * 3600 + minute * 60 + second }];
        });
        function jump() {
            const when = parseJump(input.value);
            if (!when) {
                status.textContent = "Enter a valid time or date and time.";
                return;
            }
            let best = null;
            let shortest = Infinity;
            for (const row of timed) {
                if (row.entry.hidden) {
                    continue;
                }
                const distance = when.clock
                    ? Math.abs(row.seconds - when.clock.hour * 3600 - when.clock.minute * 60)
                    : Math.abs(row.timestamp - when.target) / 1000;
                if (distance < shortest) {
                    best = row.entry;
                    shortest = distance;
                }
            }
            status.textContent = best ? `Jumped to ${best.dataset.time}` : "No dated entries";
            if (best) {
                reveal(best);
            }
        }
        listen("time-go", "click", jump);
        input.addEventListener("keydown", (event) => {
            if (event.key === "Enter") {
                event.preventDefault();
                jump();
            }
        });
    }

    function bindScreenshots() {
        const dialog = byId("image-dialog");
        const image = byId("large-image");
        for (const button of document.querySelectorAll("button[data-image]")) {
            button.addEventListener("click", () => {
                image.src = button.querySelector("img").src;
                dialog.showModal();
            });
        }
        listen("image-close", "click", () => dialog.close());
        dialog.addEventListener("close", () => image.removeAttribute("src"));
    }

    function captureRows() {
        const logData = byId("log-data");
        try {
            return JSON.parse(logData.content.textContent).map((row) => ({
                ...row,
                sequenceNumber: BigInt(row.sequence),
            }));
        } catch {
            byId("capture-title").textContent = "Could not read the embedded capture log";
            return null;
        }
    }

    function bindCapture() {
        const panel = byId("capture-panel");
        const input = byId("capture-search");
        const list = byId("capture-rows");
        let log = null;
        let selection = [];
        let page = 0;
        let matches = [];
        let current = -1;
        let activeButton = null;

        function renderRows() {
            const fragment = document.createDocumentFragment();
            const start = page * PAGE_ROWS;
            const end = Math.min(start + PAGE_ROWS, selection.length);
            const hit = current >= 0 ? matches[current] : null;
            for (let index = start; index < end; index += 1) {
                const row = selection[index];
                const line = document.createElement("li");
                const count = row.repeat > 1 ? ` ×${row.repeat}` : "";
                line.append(`[${row.time}] #${row.sequence} ${row.kind}  `);
                if (hit?.index === index) {
                    line.className = "capture-hit";
                    const mark = document.createElement("mark");
                    line.append(row.text.slice(0, hit.offset));
                    mark.textContent = row.text.slice(hit.offset, hit.offset + hit.length);
                    line.append(mark, row.text.slice(hit.offset + hit.length));
                } else {
                    line.append(row.text);
                }
                line.append(count);
                fragment.append(line);
            }
            list.replaceChildren(fragment);
            byId("capture-page").textContent = selection.length
                ? `Lines ${start + 1}–${end} of ${selection.length}` : "No embedded lines for this entry";
            byId("capture-prev-page").disabled = page === 0;
            byId("capture-next-page").disabled = end >= selection.length;
            byId("capture-prev").disabled = matches.length === 0;
            byId("capture-next").disabled = matches.length === 0;
            const count = matches.length === MAX_HITS ? `${matches.length}+` : matches.length;
            byId("capture-count").textContent = current >= 0
                ? `${current + 1} / ${count}` : `${count} matches`;
        }

        function search() {
            const needle = input.value.trim();
            matches = [];
            current = -1;
            if (needle) {
                const pattern = new RegExp(escapePattern(needle), "giu");
                for (const [index, row] of selection.entries()) {
                    for (const match of row.text.matchAll(pattern)) {
                        matches.push({ index, offset: match.index, length: match[0].length });
                        if (matches.length === MAX_HITS) {
                            break;
                        }
                    }
                    if (matches.length === MAX_HITS) {
                        break;
                    }
                }
            }
            renderRows();
        }

        function move(step) {
            if (!matches.length) {
                return;
            }
            current = navigateIndex(current, matches.length, step);
            page = Math.floor(matches[current].index / PAGE_ROWS);
            renderRows();
            list.querySelector(".capture-hit")?.scrollIntoView({ block: "nearest" });
        }

        function close() {
            panel.hidden = true;
            document.body.classList.remove("capture-open");
            activeButton?.focus();
        }

        for (const button of document.querySelectorAll(".entry-log")) {
            button.addEventListener("click", () => {
                activeButton = button;
                log ??= captureRows();
                const start = BigInt(button.dataset.from);
                const end = BigInt(button.dataset.to);
                selection = log?.filter((row) => row.sequenceNumber >= start && row.sequenceNumber <= end) ?? [];
                if (log) {
                    byId("capture-title").textContent = `Capture log #${start}–#${end}`;
                }
                page = 0;
                input.value = "";
                panel.hidden = false;
                document.body.classList.add("capture-open");
                search();
                input.focus();
            });
        }
        listen("capture-close", "click", close);
        listen("capture-search", "input", search);
        listen("capture-prev", "click", () => move(-1));
        listen("capture-next", "click", () => move(1));
        listen("capture-prev-page", "click", () => { page -= 1; renderRows(); });
        listen("capture-next-page", "click", () => { page += 1; renderRows(); });
        input.addEventListener("keydown", (event) => {
            if (event.key === "Enter") {
                event.preventDefault();
                move(event.shiftKey ? -1 : 1);
            }
        });
        document.addEventListener("keydown", (event) => {
            if (panel.hidden || byId("image-dialog").open) {
                return;
            }
            if (event.key === "Escape") {
                event.preventDefault();
                close();
            } else if ((event.ctrlKey || event.metaKey) && casefold(event.key) === "f") {
                event.preventDefault();
                input.focus();
                input.select();
            }
        });
    }

    bindAppearance();
    bindJournalSearch();
    bindMarkers();
    bindTime();
    bindScreenshots();
    bindCapture();
})();
