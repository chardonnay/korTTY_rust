use std::collections::BTreeMap;
use std::sync::LazyLock;

static CATALOG: LazyLock<Catalog> = LazyLock::new(|| Catalog::parse(include_str!("html-data.txt")));

type Rows = BTreeMap<&'static str, &'static str>;

#[derive(Default)]
struct Catalog {
    elements: Rows,
    globals: &'static str,
    aria: Rows,
    values: BTreeMap<&'static str, Rows>,
}

impl Catalog {
    fn parse(data: &'static str) -> Self {
        let mut catalog = Self::default();
        for line in data.lines().filter(|line| !line.starts_with('#')) {
            let mut fields = line.split('\t');
            match (fields.next(), fields.next()) {
                (Some("G"), Some(names)) => catalog.globals = names,
                (Some("A"), Some(name)) => {
                    catalog.aria.insert(name, fields.next().unwrap_or_default());
                }
                (Some("T"), Some(tag)) => {
                    catalog
                        .elements
                        .insert(tag, fields.next().unwrap_or_default());
                }
                (Some("V"), Some(tag)) => {
                    let name = fields.next().expect("HTML catalog attribute");
                    let words = fields.next().expect("HTML catalog attribute values");
                    catalog.values.entry(tag).or_default().insert(name, words);
                }
                _ => unreachable!("unknown HTML catalog row"),
            }
        }
        catalog
    }
}

pub(super) fn elements() -> impl Iterator<Item = &'static str> {
    CATALOG.elements.keys().copied()
}

pub(super) fn attributes(tag: &str) -> impl Iterator<Item = &'static str> {
    CATALOG
        .elements
        .get(tag)
        .into_iter()
        .flat_map(|names| names.split_ascii_whitespace())
        .chain(CATALOG.globals.split_ascii_whitespace())
        .chain(aria_attributes())
}

pub(super) fn aria_attributes() -> impl Iterator<Item = &'static str> {
    CATALOG.aria.keys().copied()
}

pub(super) fn values(tag: &str, name: &str) -> impl Iterator<Item = &'static str> {
    CATALOG
        .values
        .get(tag)
        .and_then(|rows| rows.get(name))
        .or_else(|| CATALOG.values.get("*").and_then(|rows| rows.get(name)))
        .or_else(|| CATALOG.aria.get(name))
        .into_iter()
        .flat_map(|words| words.split_ascii_whitespace())
}
