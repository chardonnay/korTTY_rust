use std::sync::LazyLock;

use regex::Regex;

use super::{Suggestions, identifier, in_code};

mod typescript;

const GLOBALS: &[&str] = &[
    "Array",
    "ArrayBuffer",
    "BigInt",
    "Boolean",
    "Date",
    "Error",
    "Infinity",
    "Intl",
    "JSON",
    "Map",
    "Math",
    "NaN",
    "Number",
    "Object",
    "Promise",
    "Reflect",
    "RegExp",
    "Set",
    "String",
    "Symbol",
    "URL",
    "URLSearchParams",
    "Uint8Array",
    "WeakMap",
    "WeakSet",
    "console",
    "document",
    "fetch",
    "globalThis",
    "isFinite",
    "isNaN",
    "parseFloat",
    "parseInt",
    "queueMicrotask",
    "setInterval",
    "setTimeout",
    "undefined",
    "window",
];

const KEYWORDS: &[&str] = &[
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "let",
    "new",
    "null",
    "of",
    "return",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "yield",
];

const TYPES: &[&str] = &[
    "any",
    "as",
    "asserts",
    "bigint",
    "boolean",
    "declare",
    "enum",
    "implements",
    "infer",
    "interface",
    "keyof",
    "namespace",
    "never",
    "number",
    "object",
    "private",
    "protected",
    "public",
    "readonly",
    "satisfies",
    "string",
    "type",
    "unknown",
    "Partial",
    "Readonly",
    "Required",
    "Record",
    "Pick",
    "Omit",
    "NonNullable",
    "Parameters",
    "ReturnType",
];

pub(super) fn suggest(lead: &str, typescript: bool) -> Suggestions {
    let mut result = Suggestions::default();
    if !in_code(lead, true) {
        return result;
    }
    if let Some(expression) = lead.strip_suffix('.') {
        let receiver = expression.trim_end_matches('?').trim_end();
        if typescript && let Some(members) = typescript::members(lead) {
            for member in members {
                result.add(&member, &member);
            }
            return result;
        }
        let name = receiver
            .rsplit(|c| !identifier(c))
            .next()
            .unwrap_or_default();
        if let Some(members) = static_members(name) {
            result.words(members);
        } else if let Some((annotation, value)) = declaration(lead, name) {
            result.words(instance_members(annotation, value));
            if value.starts_with('{') {
                static FIELD: LazyLock<Regex> =
                    LazyLock::new(|| Regex::new(r"(?:^|[,{])\s*([\w$]{1,96})\s*:").unwrap());
                for field in FIELD.captures_iter(value).take(40) {
                    result.add(&field[1], &field[1]);
                }
            }
        }
        return result;
    }
    result.generic = true;
    result.words(KEYWORDS);
    result.words(GLOBALS);
    if typescript {
        result.words(TYPES);
    }
    result
}

fn declaration<'a>(source: &'a str, name: &str) -> Option<(&'a str, &'a str)> {
    static DECLARATION: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\b(?:const|let|var)\s+([\w$]+)\s*(?::\s*([^;=\r\n]+))?(?:=\s*([^;\r\n]+))?")
            .unwrap()
    });
    DECLARATION
        .captures_iter(source)
        .filter(|found| &found[1] == name)
        .last()
        .map(|found| {
            (
                found.get(2).map_or("", |part| part.as_str().trim()),
                found.get(3).map_or("", |part| part.as_str().trim()),
            )
        })
}

fn static_members(receiver: &str) -> Option<&'static [&'static str]> {
    Some(match receiver {
        "Array" => &["from", "fromAsync", "isArray", "of", "prototype"],
        "console" => &[
            "assert", "clear", "count", "debug", "dir", "error", "group", "info", "log", "table",
            "time", "timeEnd", "trace", "warn",
        ],
        "document" => &[
            "addEventListener",
            "body",
            "createElement",
            "getElementById",
            "getElementsByClassName",
            "head",
            "querySelector",
            "querySelectorAll",
            "title",
        ],
        "JSON" => &["parse", "stringify"],
        "Math" => &[
            "abs", "ceil", "cos", "floor", "log", "max", "min", "PI", "pow", "random", "round",
            "sign", "sin", "sqrt", "trunc",
        ],
        "Number" => &[
            "EPSILON",
            "isFinite",
            "isInteger",
            "isNaN",
            "isSafeInteger",
            "MAX_SAFE_INTEGER",
            "parseFloat",
            "parseInt",
            "prototype",
        ],
        "Object" => &[
            "assign",
            "create",
            "defineProperty",
            "entries",
            "freeze",
            "fromEntries",
            "getPrototypeOf",
            "hasOwn",
            "keys",
            "values",
        ],
        "Promise" => &["all", "allSettled", "any", "race", "reject", "resolve"],
        "window" => &[
            "addEventListener",
            "clearInterval",
            "clearTimeout",
            "document",
            "fetch",
            "location",
            "setInterval",
            "setTimeout",
        ],
        _ => return None,
    })
}

fn instance_members(annotation: &str, value: &str) -> &'static [&'static str] {
    if annotation.ends_with("[]") || annotation.starts_with("Array<") || value.starts_with('[') {
        &[
            "at",
            "concat",
            "every",
            "filter",
            "find",
            "findIndex",
            "flat",
            "flatMap",
            "forEach",
            "includes",
            "indexOf",
            "join",
            "length",
            "map",
            "pop",
            "push",
            "reduce",
            "reverse",
            "shift",
            "slice",
            "some",
            "sort",
            "splice",
            "unshift",
        ]
    } else if annotation == "string" || value.starts_with(['\'', '"', '`']) {
        &[
            "at",
            "charAt",
            "charCodeAt",
            "endsWith",
            "includes",
            "indexOf",
            "length",
            "match",
            "padEnd",
            "padStart",
            "repeat",
            "replace",
            "replaceAll",
            "slice",
            "split",
            "startsWith",
            "substring",
            "toLowerCase",
            "toUpperCase",
            "trim",
        ]
    } else if annotation.starts_with("Map<") || value.starts_with("new Map(") {
        &[
            "clear", "delete", "entries", "forEach", "get", "has", "keys", "set", "size", "values",
        ]
    } else if annotation.starts_with("Set<") || value.starts_with("new Set(") {
        &[
            "add", "clear", "delete", "entries", "forEach", "has", "keys", "size", "values",
        ]
    } else if annotation.starts_with("Promise<") || value.starts_with("Promise.") {
        &["catch", "finally", "then"]
    } else {
        &[]
    }
}
