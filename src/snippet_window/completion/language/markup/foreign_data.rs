use std::collections::BTreeMap;
use std::sync::LazyLock;

type Elements = BTreeMap<&'static str, &'static str>;

static CATALOG: LazyLock<(Elements, Elements)> = LazyLock::new(|| {
    let (mut svg, mut math) = (Elements::new(), Elements::new());
    for line in include_str!("foreign-data.txt")
        .lines()
        .filter(|line| !line.starts_with('#'))
    {
        let mut fields = line.split('\t');
        let elements = match fields.next() {
            Some("S") => &mut svg,
            Some("M") => &mut math,
            _ => unreachable!("unknown foreign markup catalog row"),
        };
        let tag = fields.next().expect("foreign markup element");
        elements.insert(tag, fields.next().unwrap_or_default());
    }
    (svg, math)
});

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Namespace {
    #[default]
    Html,
    Svg,
    Math,
}

impl Namespace {
    fn vocabulary(self) -> Option<&'static Elements> {
        match self {
            Self::Html => None,
            Self::Svg => Some(&CATALOG.0),
            Self::Math => Some(&CATALOG.1),
        }
    }

    pub(super) fn elements(self) -> impl Iterator<Item = &'static str> {
        self.vocabulary()
            .into_iter()
            .flat_map(|items| items.keys().copied().filter(|&name| name != "*"))
    }

    pub(super) fn attributes(self, tag: &str) -> impl Iterator<Item = &'static str> {
        self.vocabulary().into_iter().flat_map(move |items| {
            let specific = items
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(tag))
                .map(|(_, attrs)| *attrs);
            specific
                .into_iter()
                .chain(items.get("*").copied())
                .flat_map(str::split_ascii_whitespace)
        })
    }

    pub(super) fn values(self, tag: &str, name: &str) -> &'static [&'static str] {
        match (self, tag, name) {
            (
                Self::Svg,
                _,
                "gradientunits"
                | "patternunits"
                | "patterncontentunits"
                | "maskunits"
                | "maskcontentunits"
                | "filterunits"
                | "primitiveunits"
                | "clippathunits",
            ) => &["objectBoundingBox", "userSpaceOnUse"],
            (Self::Svg, _, "spreadmethod") => &["pad", "reflect", "repeat"],
            (Self::Svg, _, "stroke-linecap") => &["butt", "round", "square"],
            (Self::Svg, _, "stroke-linejoin") => &["arcs", "bevel", "miter", "miter-clip", "round"],
            (Self::Svg, _, "fill-rule" | "clip-rule") => &["evenodd", "nonzero"],
            (Self::Math, "math", "display") => &["block", "inline"],
            (Self::Math, "mo", "form") => &["infix", "postfix", "prefix"],
            (
                Self::Math,
                "mo",
                "fence" | "largeop" | "movablelimits" | "separator" | "stretchy" | "symmetric",
            )
            | (Self::Math, _, "displaystyle") => &["false", "true"],
            (Self::Math, "annotation-xml", "encoding") => &["application/xhtml+xml", "text/html"],
            (Self::Math, _, "dir") => &["ltr", "rtl"],
            _ => &[],
        }
    }
}
