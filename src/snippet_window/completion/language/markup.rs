use std::collections::HashSet;

use super::Suggestions;
use foreign_data::Namespace;

mod css_context;
mod css_data;
mod foreign_data;
mod html_data;

const CSS_PROPERTIES: &[&str] = &[
    "align-content",
    "align-items",
    "align-self",
    "animation",
    "appearance",
    "aspect-ratio",
    "background",
    "background-color",
    "background-image",
    "background-position",
    "background-size",
    "border",
    "border-color",
    "border-radius",
    "border-style",
    "border-width",
    "bottom",
    "box-shadow",
    "box-sizing",
    "color",
    "content",
    "cursor",
    "display",
    "fill",
    "filter",
    "flex",
    "flex-basis",
    "flex-direction",
    "flex-grow",
    "flex-shrink",
    "flex-wrap",
    "font-family",
    "font-size",
    "font-style",
    "font-weight",
    "gap",
    "grid",
    "grid-area",
    "grid-column",
    "grid-row",
    "grid-template-areas",
    "grid-template-columns",
    "grid-template-rows",
    "height",
    "inset",
    "justify-content",
    "left",
    "letter-spacing",
    "line-height",
    "list-style",
    "margin",
    "margin-block",
    "margin-inline",
    "max-height",
    "max-width",
    "min-height",
    "min-width",
    "object-fit",
    "opacity",
    "outline",
    "overflow",
    "overflow-x",
    "overflow-y",
    "padding",
    "padding-block",
    "padding-inline",
    "pointer-events",
    "position",
    "right",
    "stroke",
    "text-align",
    "text-decoration",
    "text-overflow",
    "text-transform",
    "top",
    "transform",
    "transition",
    "user-select",
    "vertical-align",
    "visibility",
    "white-space",
    "width",
    "z-index",
];

pub(super) fn css(source: &str, lead: &str) -> Suggestions {
    let mut result = Suggestions::default();
    let Some(context) = css_context::scan(lead, |_| {}) else {
        return result;
    };
    if !context.colon && lead.ends_with('@') {
        result.words(css_data::at_rules());
        return result;
    }
    if context.variable_name && context.property.is_some() {
        let mut seen = HashSet::new();
        css_context::scan(source, |name| {
            if name.len() <= 98 && seen.len() < 200 && seen.insert(name) {
                result.add(name, name);
            }
        });
        return result;
    }
    if !context.declarations {
        add_pseudo_names(&mut result, lead);
        return result;
    }

    let rule = context.rule.to_ascii_lowercase();
    let descriptors = css_data::descriptors(&rule);
    let declarations = descriptors.is_none() || rule == "@page";
    if let Some(property) = context.property.map(str::to_ascii_lowercase) {
        if let Some(values) = descriptors.and_then(|entries| entries.get(property.as_str())) {
            for value in values.split_ascii_whitespace() {
                result.add(value, value);
            }
        } else if declarations
            && !css_data::contains_property(&property)
            && !CSS_PROPERTIES.contains(&property.as_str())
            && !property.starts_with("--")
        {
            add_pseudo_names(&mut result, lead);
            return result;
        }
        if declarations {
            result.words(css_values(&property));
            for value in css_data::values(&property) {
                result.add(value, value);
            }
            result.words(&["inherit", "initial", "revert", "revert-layer", "unset"]);
            result.template("var", "var({{--name}})");
            result.template("calc", "calc({{100% - 1rem}})");
        }
    } else if context.colon {
        add_pseudo_names(&mut result, lead);
    } else {
        if let Some(entries) = descriptors {
            for &name in entries.keys() {
                result.add(name, name);
            }
        }
        if declarations {
            result.words(CSS_PROPERTIES);
            for name in css_data::properties() {
                result.add(name, name);
            }
        }
    }
    result
}

fn add_pseudo_names(result: &mut Suggestions, lead: &str) {
    if lead.ends_with(':') {
        result.words(css_data::pseudo_names(lead.ends_with("::")));
    }
}

fn css_values(property: &str) -> &'static [&'static str] {
    match property {
        "display" => &[
            "block",
            "contents",
            "flex",
            "flow-root",
            "grid",
            "inline",
            "inline-block",
            "inline-flex",
            "inline-grid",
            "list-item",
            "none",
            "table",
        ],
        "position" => &["absolute", "fixed", "relative", "static", "sticky"],
        "background" | "background-color" | "border-color" | "color" | "fill" | "stroke" => &[
            "black",
            "blue",
            "currentColor",
            "gray",
            "green",
            "orange",
            "red",
            "transparent",
            "white",
        ],
        "overflow" | "overflow-x" | "overflow-y" => {
            &["auto", "clip", "hidden", "scroll", "visible"]
        }
        "align-items" | "align-self" | "align-content" | "justify-content" => &[
            "baseline",
            "center",
            "end",
            "flex-end",
            "flex-start",
            "normal",
            "space-around",
            "space-between",
            "space-evenly",
            "start",
            "stretch",
        ],
        "flex-direction" => &["column", "column-reverse", "row", "row-reverse"],
        "flex-wrap" => &["nowrap", "wrap", "wrap-reverse"],
        "font-weight" => &["bold", "bolder", "lighter", "normal"],
        "font-family" => &["cursive", "monospace", "sans-serif", "serif", "system-ui"],
        "border-style" => &["dashed", "dotted", "double", "hidden", "none", "solid"],
        "box-sizing" => &["border-box", "content-box"],
        "object-fit" => &["contain", "cover", "fill", "none", "scale-down"],
        "text-align" => &["center", "end", "justify", "left", "right", "start"],
        "visibility" => &["collapse", "hidden", "visible"],
        "white-space" => &[
            "break-spaces",
            "normal",
            "nowrap",
            "pre",
            "pre-line",
            "pre-wrap",
        ],
        "height" | "width" | "max-width" | "min-width" => {
            &["auto", "fit-content", "max-content", "min-content"]
        }
        _ => &[],
    }
}

const HTML_TAGS: &[&str] = &[
    "a", "article", "aside", "audio", "body", "br", "button", "canvas", "code", "dd", "details",
    "dialog", "div", "dl", "dt", "em", "fieldset", "figure", "footer", "form", "h1", "h2", "h3",
    "h4", "h5", "h6", "head", "header", "hr", "html", "iframe", "img", "input", "label", "legend",
    "li", "link", "main", "math", "meta", "nav", "ol", "option", "p", "picture", "pre", "script",
    "section", "select", "small", "source", "span", "strong", "style", "summary", "table", "tbody",
    "svg", "td", "template", "textarea", "th", "thead", "time", "title", "tr", "ul", "video",
];

const HTML_ATTRIBUTES: &[&str] = &[
    "aria-describedby",
    "aria-hidden",
    "aria-label",
    "aria-labelledby",
    "class",
    "contenteditable",
    "dir",
    "draggable",
    "hidden",
    "id",
    "lang",
    "role",
    "style",
    "tabindex",
    "title",
    "translate",
];

pub(super) fn html(lead: &str, tail: &str) -> Suggestions {
    let mut result = Suggestions::default();
    let Some((inside, quote, tags)) = html_context(lead) else {
        return result;
    };
    if inside == "/" {
        let limit = if tags.last().is_some_and(OpenTag::raw_text) {
            1
        } else {
            30
        };
        for OpenTag { name, .. } in tags.into_iter().rev().take(limit) {
            if tail.starts_with('>') {
                result.add(name, name);
            } else {
                result.add(name, &format!("{name}>"));
            }
        }
        return result;
    }
    if inside.is_empty() {
        let namespace = tag_namespace("", tags.last());
        if namespace == Namespace::Html {
            result.words(HTML_TAGS);
            for tag in html_data::elements() {
                result.add(tag, tag);
            }
        } else {
            for tag in namespace.elements() {
                result.add(tag, tag);
            }
        }
        return result;
    }
    let Some((tag, attrs)) = inside.split_once(char::is_whitespace) else {
        return result;
    };
    let tag = tag.to_ascii_lowercase();
    let namespace = start_tag_namespace(&tag, tags.last());
    let before_value = quote.map_or(attrs, |position| &inside[..position]);
    if let Some(left) = before_value.trim_end().strip_suffix('=') {
        let name = left
            .split_whitespace()
            .next_back()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let used = quote
            .filter(|_| matches!(name.as_str(), "role" | "aria-relevant"))
            .map_or("", |position| &inside[position + 1..]);
        if namespace == Namespace::Html {
            result.words(html_values(&tag, &name));
        } else {
            result.words(namespace.values(&tag, &name));
            if name != "role" && !name.starts_with("aria-") {
                return result;
            }
        }
        for word in html_data::values(&tag, &name) {
            if !used
                .split_ascii_whitespace()
                .any(|value| value.eq_ignore_ascii_case(word))
            {
                result.add(word, word);
            }
        }
    } else if quote.is_none() {
        if namespace == Namespace::Html {
            result.words(html_attributes(&tag));
            result.words(HTML_ATTRIBUTES);
            for name in html_data::attributes(&tag) {
                result.add(name, name);
            }
        } else {
            for name in namespace
                .attributes(&tag)
                .chain(html_data::aria_attributes())
            {
                result.add(name, name);
            }
        }
    }
    result
}

struct OpenTag<'a> {
    name: &'a str,
    namespace: Namespace,
    children: Namespace,
}

impl OpenTag<'_> {
    fn raw_text(&self) -> bool {
        match self.namespace {
            Namespace::Html => raw_html_text(self.name),
            Namespace::Svg => named(self.name, &["script", "style"]),
            Namespace::Math => false,
        }
    }
}

fn tag_namespace(name: &str, parent: Option<&OpenTag<'_>>) -> Namespace {
    let namespace = parent.map_or(Namespace::Html, |tag| tag.children);
    if name.eq_ignore_ascii_case("svg")
        && (namespace == Namespace::Html
            || parent.is_some_and(|tag| {
                tag.namespace == Namespace::Math && tag.name.eq_ignore_ascii_case("annotation-xml")
            }))
    {
        Namespace::Svg
    } else if namespace == Namespace::Html
        && (name.eq_ignore_ascii_case("math")
            || (parent.is_some_and(|tag| tag.namespace == Namespace::Math)
                && named(name, &["mglyph", "malignmark"])))
    {
        Namespace::Math
    } else {
        namespace
    }
}

fn start_tag_namespace(name: &str, parent: Option<&OpenTag<'_>>) -> Namespace {
    if named(
        name,
        &[
            "b",
            "big",
            "blockquote",
            "body",
            "br",
            "center",
            "code",
            "dd",
            "div",
            "dl",
            "dt",
            "em",
            "embed",
            "h1",
            "h2",
            "h3",
            "h4",
            "h5",
            "h6",
            "head",
            "hr",
            "i",
            "img",
            "li",
            "listing",
            "menu",
            "meta",
            "nobr",
            "ol",
            "p",
            "pre",
            "ruby",
            "s",
            "small",
            "span",
            "strong",
            "strike",
            "sub",
            "sup",
            "table",
            "tt",
            "u",
            "ul",
            "var",
        ],
    ) {
        Namespace::Html
    } else {
        tag_namespace(name, parent)
    }
}

fn child_namespace(namespace: Namespace, name: &str, attributes: &str) -> Namespace {
    match namespace {
        Namespace::Svg if named(name, &["foreignObject", "desc", "title"]) => Namespace::Html,
        Namespace::Math if named(name, &["mi", "mo", "mn", "ms", "mtext"]) => Namespace::Html,
        Namespace::Math
            if name.eq_ignore_ascii_case("annotation-xml") && html_annotation(attributes) =>
        {
            Namespace::Html
        }
        _ => namespace,
    }
}

fn close_in_scope(tags: &mut Vec<OpenTag<'_>>, names: &[&str], boundaries: &[&str]) {
    let index = tags
        .iter()
        .enumerate()
        .rev()
        .take_while(|(_, tag)| {
            tag.namespace == Namespace::Html
                && !tag.name.eq_ignore_ascii_case("template")
                && !named(tag.name, boundaries)
        })
        .find_map(|(index, tag)| named(tag.name, names).then_some(index));
    if let Some(index) = index {
        tags.truncate(index);
    }
}

fn before_html_start(tags: &mut Vec<OpenTag<'_>>, name: &str) {
    const HEADINGS: &[&str] = &["h1", "h2", "h3", "h4", "h5", "h6"];
    const TABLE_SECTIONS: &[&str] = &["thead", "tbody", "tfoot"];
    if named(
        name,
        &[
            "address",
            "article",
            "aside",
            "blockquote",
            "details",
            "dialog",
            "div",
            "dl",
            "fieldset",
            "figcaption",
            "figure",
            "footer",
            "form",
            "header",
            "hgroup",
            "hr",
            "li",
            "listing",
            "main",
            "menu",
            "nav",
            "ol",
            "p",
            "plaintext",
            "pre",
            "search",
            "section",
            "table",
            "ul",
            "xmp",
            "dd",
            "dt",
        ],
    ) || named(name, HEADINGS)
    {
        close_in_scope(
            tags,
            &["p"],
            &[
                "button", "caption", "table", "td", "th", "object", "marquee", "applet",
            ],
        );
    }
    if named(
        name,
        &["caption", "colgroup", "thead", "tbody", "tfoot", "tr"],
    ) {
        close_in_scope(tags, &["caption", "colgroup"], &["table"]);
    }
    match name.to_ascii_lowercase().as_str() {
        "li" => close_in_scope(tags, &["li"], &["ul", "ol", "menu", "table"]),
        "dt" | "dd" => close_in_scope(tags, &["dt", "dd"], &["dl", "table"]),
        "option" => close_in_scope(tags, &["option"], &["select", "datalist", "optgroup"]),
        "optgroup" | "hr" => {
            close_in_scope(tags, &["option"], &["select", "datalist", "optgroup"]);
            close_in_scope(tags, &["optgroup"], &["select", "datalist"]);
        }
        "rb" | "rtc" => close_in_scope(tags, &["rb", "rtc", "rp", "rt"], &["ruby"]),
        "rp" | "rt" => close_in_scope(tags, &["rb", "rp", "rt"], &["ruby", "rtc"]),
        "tr" => close_in_scope(tags, &["tr"], &["table", "thead", "tbody", "tfoot"]),
        "td" | "th" => close_in_scope(tags, &["td", "th"], &["table", "tr"]),
        "thead" | "tbody" | "tfoot" => {
            close_in_scope(tags, TABLE_SECTIONS, &["table"]);
            close_in_scope(tags, &["tr"], &["table"]);
        }
        "body" => close_in_scope(tags, &["head"], &["html"]),
        "button" => close_in_scope(tags, &["button"], &["table", "td", "th"]),
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
            if tags.last().is_some_and(|tag| {
                tag.namespace == Namespace::Html && named(tag.name, HEADINGS)
            }) =>
        {
            tags.pop();
        }
        _ => {}
    }
}

fn push_open_tag<'a>(
    tags: &mut Vec<OpenTag<'a>>,
    name: &'a str,
    attributes: &str,
    self_closing: bool,
) {
    let namespace = start_tag_namespace(name, tags.last());
    if namespace == Namespace::Html {
        if tags
            .last()
            .is_some_and(|tag| tag.children != Namespace::Html)
        {
            let keep = tags
                .iter()
                .rposition(|tag| {
                    tag.namespace == Namespace::Html || tag.children == Namespace::Html
                })
                .map_or(0, |index| index + 1);
            tags.truncate(keep);
        }
        before_html_start(tags, name);
        if named(
            name,
            &[
                "area", "base", "br", "col", "embed", "hr", "image", "img", "input", "keygen",
                "link", "meta", "param", "source", "track", "wbr",
            ],
        ) {
            return;
        }
    } else if self_closing {
        return;
    }
    let children = child_namespace(namespace, name, attributes);
    tags.push(OpenTag {
        name,
        namespace,
        children,
    });
}

fn html_annotation(mut rest: &str) -> bool {
    while !rest.trim_start().is_empty() {
        rest = rest.trim_start();
        let end = rest
            .find(|ch: char| ch.is_ascii_whitespace() || matches!(ch, '=' | '/'))
            .unwrap_or(rest.len());
        if end == 0 {
            rest = &rest[1..];
            continue;
        }
        let (name, remaining) = rest.split_at(end);
        rest = remaining.trim_start();
        let Some(value) = rest.strip_prefix('=') else {
            continue;
        };
        let value = value.trim_start();
        let (content, remaining) = if let Some(quote @ ('"' | '\'')) = value.chars().next() {
            let Some(parts) = value[1..].split_once(quote) else {
                return false;
            };
            parts
        } else {
            value.split_at(value.find(char::is_whitespace).unwrap_or(value.len()))
        };
        if name.eq_ignore_ascii_case("encoding") {
            return named(content, &["text/html", "application/xhtml+xml"]);
        }
        rest = remaining;
    }
    false
}

type TagContext<'a> = (&'a str, Option<usize>, Vec<OpenTag<'a>>);

fn html_context(lead: &str) -> Option<TagContext<'_>> {
    let mut tags = Vec::<OpenTag<'_>>::new();
    let mut inside = None;
    let mut quote = None;
    let mut cursor = 0;
    while cursor < lead.len() {
        let byte = lead.as_bytes()[cursor];
        if inside.is_none()
            && byte == b'<'
            && let Some(tag) = tags.last().filter(|tag| tag.raw_text())
            && !raw_html_end(&lead[cursor..], tag.name)
        {
            cursor += 1;
            continue;
        }
        if inside.is_none() && lead.as_bytes()[cursor..].starts_with(b"<!--") {
            cursor += 4 + lead[cursor + 4..].find("-->")? + 3;
            continue;
        }
        if inside.is_none()
            && tags
                .last()
                .is_some_and(|tag| tag.children != Namespace::Html)
            && lead.as_bytes()[cursor..].starts_with(b"<![CDATA[")
        {
            cursor += 9 + lead[cursor + 9..].find("]]>")? + 3;
            continue;
        }
        match (inside, quote, byte) {
            (None, _, b'<')
                if lead.as_bytes().get(cursor + 1).is_none_or(|byte| {
                    byte.is_ascii_alphabetic() || matches!(byte, b'/' | b'!' | b'?')
                }) =>
            {
                inside = Some(cursor + 1)
            }
            (_, Some((delimiter, _)), byte) if byte == delimiter => quote = None,
            (Some(start), None, b'\'' | b'"') => quote = Some((byte, cursor - start)),
            (Some(start), None, b'>') => {
                let tag = lead[start..cursor]
                    .split_whitespace()
                    .next()
                    .unwrap_or_default();
                let name = tag.trim_end_matches('/');
                if let Some(close) = name.strip_prefix('/') {
                    if let Some(index) = tags
                        .iter()
                        .rposition(|tag| tag.name.eq_ignore_ascii_case(close))
                    {
                        tags.truncate(index);
                    }
                } else if name.starts_with(|c: char| c.is_ascii_alphabetic()) {
                    push_open_tag(
                        &mut tags,
                        name,
                        &lead[start + tag.len()..cursor],
                        lead[start..cursor].ends_with('/'),
                    );
                }
                inside = None;
            }
            _ => {}
        }
        cursor += 1;
    }
    Some((&lead[inside?..], quote.map(|(_, index)| index), tags))
}

fn raw_html_text(name: &str) -> bool {
    named(
        name,
        &[
            "script", "style", "textarea", "title", "iframe", "xmp", "noembed", "noframes",
        ],
    )
}

fn named(name: &str, names: &[&str]) -> bool {
    names.iter().any(|known| name.eq_ignore_ascii_case(known))
}

fn raw_html_end(remaining: &str, tag: &str) -> bool {
    let Some(end) = remaining.strip_prefix("</") else {
        return false;
    };
    match end.split_at_checked(tag.len()) {
        Some((name, tail)) => {
            name.eq_ignore_ascii_case(tag)
                && (tail.is_empty()
                    || tail
                        .starts_with(|c: char| c.is_ascii_whitespace() || matches!(c, '/' | '>')))
        }
        None => tag
            .get(..end.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(end)),
    }
}

fn html_attributes(tag: &str) -> &'static [&'static str] {
    match tag {
        "a" => &["download", "href", "hreflang", "rel", "target", "type"],
        "button" => &["disabled", "form", "name", "type", "value"],
        "form" => &[
            "action",
            "autocomplete",
            "enctype",
            "method",
            "name",
            "novalidate",
            "target",
        ],
        "img" => &[
            "alt", "decoding", "height", "loading", "sizes", "src", "srcset", "width",
        ],
        "input" => &[
            "accept",
            "autocomplete",
            "checked",
            "disabled",
            "max",
            "maxlength",
            "min",
            "minlength",
            "multiple",
            "name",
            "pattern",
            "placeholder",
            "readonly",
            "required",
            "step",
            "type",
            "value",
        ],
        "label" => &["for"],
        "link" => &[
            "as",
            "crossorigin",
            "href",
            "integrity",
            "media",
            "rel",
            "type",
        ],
        "script" => &["async", "crossorigin", "defer", "integrity", "src", "type"],
        "select" => &[
            "autocomplete",
            "disabled",
            "multiple",
            "name",
            "required",
            "size",
        ],
        "textarea" => &[
            "cols",
            "disabled",
            "maxlength",
            "name",
            "placeholder",
            "required",
            "rows",
            "wrap",
        ],
        "audio" | "video" => &["autoplay", "controls", "loop", "muted", "preload", "src"],
        _ => &[],
    }
}

fn html_values(tag: &str, name: &str) -> &'static [&'static str] {
    match (tag, name) {
        ("input", "type") => &[
            "button", "checkbox", "color", "date", "email", "file", "hidden", "number", "password",
            "radio", "range", "reset", "search", "submit", "tel", "text", "time", "url",
        ],
        ("button", "type") => &["button", "reset", "submit"],
        ("script", "type") => &["module", "text/javascript"],
        ("form", "method") => &["dialog", "get", "post"],
        ("img", "loading") => &["eager", "lazy"],
        (_, "dir") => &["auto", "ltr", "rtl"],
        (_, "target") => &["_blank", "_parent", "_self", "_top"],
        (_, "aria-hidden" | "contenteditable" | "draggable") => &["false", "true"],
        _ => &[],
    }
}
