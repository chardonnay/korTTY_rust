use std::collections::{BTreeMap, HashMap};
use std::sync::LazyLock;

static CATALOG: LazyLock<Catalog> = LazyLock::new(|| Catalog::parse(include_str!("css-data.txt")));

type Values = BTreeMap<&'static str, &'static str>;

#[derive(Default)]
struct Catalog {
    properties: Values,
    at_rules: Vec<&'static str>,
    pseudo_classes: Vec<&'static str>,
    pseudo_elements: Vec<&'static str>,
    descriptors: HashMap<&'static str, Values>,
}

impl Catalog {
    fn parse(data: &'static str) -> Self {
        let mut catalog = Self::default();
        for line in data.lines().filter(|line| !line.starts_with('#')) {
            let mut fields = line.split('\t');
            let kind = fields.next().expect("CSS catalog row kind");
            let name = fields.next().expect("CSS catalog name");
            match kind {
                "P" => {
                    let values = fields.next().unwrap_or_default();
                    catalog.properties.insert(name, values);
                }
                "A" => catalog.at_rules.push(name),
                "C" => catalog.pseudo_classes.push(name.trim_end_matches("()")),
                "E" => catalog.pseudo_elements.push(name.trim_end_matches("()")),
                "D" => {
                    let property = fields.next().expect("CSS descriptor name");
                    let values = fields.next().unwrap_or_default();
                    catalog
                        .descriptors
                        .entry(name)
                        .or_default()
                        .insert(property, values);
                }
                _ => unreachable!("unknown CSS catalog row kind"),
            }
        }
        catalog
    }
}

pub(super) fn properties() -> impl Iterator<Item = &'static str> {
    CATALOG.properties.keys().copied()
}

pub(super) fn contains_property(property: &str) -> bool {
    CATALOG.properties.contains_key(property)
}

pub(super) fn at_rules() -> &'static [&'static str] {
    &CATALOG.at_rules
}

pub(super) fn pseudo_names(elements: bool) -> &'static [&'static str] {
    if elements {
        &CATALOG.pseudo_elements
    } else {
        &CATALOG.pseudo_classes
    }
}

pub(super) fn descriptors(rule: &str) -> Option<&'static Values> {
    CATALOG.descriptors.get(rule)
}

pub(super) fn values(property: &str) -> impl Iterator<Item = &'static str> {
    CATALOG
        .properties
        .get(property)
        .into_iter()
        .flat_map(|words| words.split_ascii_whitespace())
}
