//! A bounded snapshot of a JSON schema used for snippet completion.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fmt;
use std::io;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

use percent_encoding::percent_decode_str;
use referencing::{Draft, Registry, RegistryBuilder, Resolver, uri};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::Value;

mod location;
mod related;
pub(crate) use related::read;

pub(crate) const MAX_SCHEMA_BYTES: u64 = 512 * 1024;
const MAX_EXPANSION: usize = 128;
const BASE_URI: &str = "https://schema.kortty.invalid/schema.json";
const MAX_RESOURCES: usize = 32;

#[derive(Eq, PartialEq)]
pub(crate) struct Schema {
    root: Value,
    documents: BTreeMap<String, Value>,
}

impl Serialize for Schema {
    fn serialize<S: Serializer>(&self, encoder: S) -> Result<S::Ok, S::Error> {
        if self.documents.is_empty() {
            self.root.serialize(encoder)
        } else {
            (&self.root, &self.documents).serialize(encoder)
        }
    }
}

impl fmt::Debug for Schema {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JSON schema")
    }
}

impl<'de> Deserialize<'de> for Schema {
    fn deserialize<D: Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        let stored = Value::deserialize(decoder)?;
        if stored.is_array() {
            let (root, documents) = serde_json::from_value(stored).map_err(de::Error::custom)?;
            Self::from_documents(root, documents).map_err(de::Error::custom)
        } else {
            Self::from_value(stored).map_err(de::Error::custom)
        }
    }
}

pub(crate) enum Segment<'a> {
    Property(&'a str),
    Item(usize),
}

pub(crate) struct Check {
    pub(crate) notice: &'static str,
    pub(crate) diagnostics: String,
    pub(crate) marks: Vec<Mark>,
}

#[derive(Clone)]
pub(crate) struct Mark {
    pub(crate) label: String,
    pub(crate) bytes: Range<usize>,
}

impl Check {
    fn report(notice: &'static str, diagnostics: String, marks: Vec<Mark>) -> Self {
        Self {
            notice,
            diagnostics,
            marks,
        }
    }
}

pub(crate) fn check(
    source: &str,
    schema: Option<&Schema>,
    cancel: &AtomicBool,
    limit: usize,
) -> Result<Check, String> {
    let check_cancel = || {
        if cancel.load(Ordering::Relaxed) {
            Err("JSON validation was canceled.".to_owned())
        } else {
            Ok(())
        }
    };
    check_cancel()?;
    let instance: Value = match serde_json::from_str(source) {
        Ok(value) => value,
        Err(error) => {
            let bytes = location::syntax(source, error.line(), error.column());
            let label = format!("Line {}, column {}", error.line(), error.column());
            return Ok(Check::report(
                "JSON syntax error.",
                error.to_string(),
                vec![Mark { label, bytes }],
            ));
        }
    };
    let Some(schema) = schema else {
        return Ok(Check::report(
            "JSON syntax check passed.",
            String::new(),
            Vec::new(),
        ));
    };
    check_cancel()?;
    let options = jsonschema::options().offline().with_pattern_options(
        jsonschema::PatternOptions::fancy_regex()
            .backtrack_limit(20_000)
            .size_limit(2 * 1024 * 1024),
    );
    let registry = (!schema.documents.is_empty()).then(|| schema.registry());
    let validator = match &registry {
        None => options.build(&schema.root),
        Some(Ok(registry)) => options
            .with_registry(registry)
            .with_base_uri(BASE_URI)
            .build(&schema.root),
        Some(Err(_)) => {
            return Ok(Check::report(
                "The JSON schema could not be used.",
                "The saved schema references could not be resolved.".into(),
                Vec::new(),
            ));
        }
    };
    let validator = match validator {
        Ok(validator) => validator,
        Err(error) => {
            let mut message = error.masked().to_string();
            message.truncate(message.floor_char_boundary(limit));
            return Ok(Check::report(
                "The JSON schema could not be used.",
                message,
                Vec::new(),
            ));
        }
    };
    let mut diagnostics = String::new();
    let mut paths = Vec::new();
    let mut invalid = false;
    for (index, error) in validator.iter_errors(&instance).enumerate() {
        invalid = true;
        check_cancel()?;
        let path = error.instance_path().to_string();
        let label = if path.is_empty() { "(root)" } else { &path };
        let message = format!("{label}: {}\n", error.masked());
        if index == 16 || diagnostics.len().saturating_add(message.len()) > limit {
            let marker = "Further schema diagnostics omitted.";
            let end = limit.saturating_sub(marker.len());
            if diagnostics.is_empty() {
                diagnostics.push_str(&message[..message.floor_char_boundary(end)]);
            } else {
                diagnostics.truncate(diagnostics.floor_char_boundary(end));
            }
            if limit >= marker.len() {
                diagnostics.push_str(marker);
            }
            break;
        }
        if !paths.contains(&path) {
            paths.push(path);
        }
        diagnostics.push_str(&message);
    }
    check_cancel()?;
    if !invalid {
        Ok(Check::report(
            "JSON matches the schema.",
            diagnostics,
            Vec::new(),
        ))
    } else {
        Ok(Check::report(
            "JSON does not match the schema.",
            diagnostics,
            location::marks(source, &paths),
        ))
    }
}

impl Schema {
    pub(crate) fn parse(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() as u64 > MAX_SCHEMA_BYTES {
            return Err(io::Error::other(
                "JSON schema files are limited to 512 KiB.",
            ));
        }
        let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
        Self::from_value(serde_json::from_slice(bytes)?)
    }

    fn from_value(value: Value) -> io::Result<Self> {
        Self::from_documents(value, BTreeMap::new())
    }

    fn from_documents(root: Value, documents: BTreeMap<String, Value>) -> io::Result<Self> {
        let valid = |value: &Value| value.is_object() || value.is_boolean();
        if !valid(&root) || documents.values().any(|value| !valid(value)) {
            return Err(io::Error::other(
                "A JSON schema must be an object or a Boolean.",
            ));
        }
        if documents.len() > MAX_RESOURCES
            || documents.keys().any(|uri| {
                uri.len() > 4096 || !(uri.starts_with("http://") || uri.starts_with("https://"))
            })
        {
            return Err(io::Error::other("Invalid saved JSON schema resources."));
        }
        let schema = Self { root, documents };
        if serde_json::to_vec(&schema)?.len() as u64 > MAX_SCHEMA_BYTES {
            return Err(io::Error::other(
                "A saved JSON schema is limited to 512 KiB.",
            ));
        }
        Ok(schema)
    }

    fn registry_builder(&self) -> Result<RegistryBuilder<'_>, referencing::Error> {
        Registry::new()
            .add(BASE_URI, &self.root)?
            .extend(self.documents.iter())
    }

    fn registry(&self) -> Result<Registry<'_>, referencing::Error> {
        self.registry_builder()?.prepare()
    }

    pub(crate) fn with_at<T>(
        &self,
        path: &[Segment<'_>],
        use_schemas: impl FnOnce(&[&Value]) -> T,
    ) -> T {
        if !self.documents.is_empty()
            && let Ok(registry) = self.registry()
            && let Ok(base) = uri::from_str(BASE_URI)
        {
            let draft = Draft::default().detect(&self.root);
            if let Ok(resolver) = registry
                .resolver(base)
                .in_subresource(draft.create_resource_ref(&self.root))
            {
                let mut schemas = expand_registered([(&self.root, resolver, draft)]);
                for segment in path {
                    schemas = expand_registered(schemas.into_iter().filter_map(
                        |(schema, resolver, draft)| {
                            let child = child_at(schema, segment)?;
                            let draft = draft.detect(child);
                            let resolver = resolver
                                .in_subresource(draft.create_resource_ref(child))
                                .ok()?;
                            Some((child, resolver, draft))
                        },
                    ));
                }
                return use_schemas(
                    &schemas
                        .into_iter()
                        .map(|(schema, _, _)| schema)
                        .collect::<Vec<_>>(),
                );
            }
        }
        use_schemas(&self.at(path))
    }

    fn at(&self, path: &[Segment<'_>]) -> Vec<&Value> {
        let mut schemas = self.expand([&self.root]);
        for segment in path {
            schemas = self.expand(
                schemas
                    .into_iter()
                    .filter_map(|schema| child_at(schema, segment)),
            );
            if schemas.is_empty() {
                break;
            }
        }
        schemas
    }

    fn expand<'a>(&'a self, initial: impl IntoIterator<Item = &'a Value>) -> Vec<&'a Value> {
        let mut todo = VecDeque::new();
        let mut seen = HashSet::new();
        let mut enqueue = |schema: &'a Value, queue: &mut VecDeque<&'a Value>| {
            if seen.len() < MAX_EXPANSION && seen.insert(std::ptr::from_ref(schema)) {
                queue.push_back(schema);
            }
        };
        for schema in initial.into_iter().take(MAX_EXPANSION) {
            enqueue(schema, &mut todo);
        }
        let mut resolved = Vec::new();
        while let Some(schema) = todo.pop_front() {
            resolved.push(schema);
            if let Some(reference) = schema
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|value| value.strip_prefix('#'))
                && let Ok(pointer) = percent_decode_str(reference).decode_utf8()
                && let Some(target) = self.root.pointer(&pointer)
            {
                enqueue(target, &mut todo);
            }
            for keyword in ["allOf", "anyOf", "oneOf"] {
                if let Some(branches) = schema.get(keyword).and_then(Value::as_array) {
                    for branch in branches.iter().take(MAX_EXPANSION) {
                        enqueue(branch, &mut todo);
                    }
                }
            }
        }
        resolved
    }
}

fn child_at<'a>(schema: &'a Value, segment: &Segment<'_>) -> Option<&'a Value> {
    match segment {
        Segment::Property(key) => schema
            .get("properties")
            .and_then(|props| props.get(*key))
            .or_else(|| schema.get("additionalProperties")),
        Segment::Item(index) => schema
            .get("prefixItems")
            .and_then(|prefix| prefix.get(*index))
            .or_else(|| {
                let items = schema.get("items")?;
                if items.is_array() {
                    items.get(*index).or_else(|| schema.get("additionalItems"))
                } else {
                    Some(items)
                }
            }),
    }
}

type ResolvedSchema<'a> = (&'a Value, Resolver<'a>, Draft);

fn expand_registered<'a>(
    initial: impl IntoIterator<Item = ResolvedSchema<'a>>,
) -> Vec<ResolvedSchema<'a>> {
    let mut todo: VecDeque<_> = initial.into_iter().take(MAX_EXPANSION).collect();
    let mut seen = HashSet::new();
    let mut resolved = Vec::new();
    while let Some((schema, resolver, draft)) = todo.pop_front() {
        if !seen.insert(std::ptr::from_ref(schema)) {
            continue;
        }
        resolved.push((schema, resolver.clone(), draft));
        if resolved.len() == MAX_EXPANSION {
            break;
        }
        if let Some(reference) = schema.get("$ref").and_then(Value::as_str)
            && let Ok(target) = resolver.lookup(reference)
        {
            todo.push_back(target.into_inner());
        }
        for keyword in ["allOf", "anyOf", "oneOf"] {
            if let Some(branches) = schema.get(keyword).and_then(Value::as_array) {
                for child in branches.iter().take(MAX_EXPANSION) {
                    let draft = draft.detect(child);
                    if let Ok(resolver) = resolver.in_subresource(draft.create_resource_ref(child))
                    {
                        todo.push_back((child, resolver, draft));
                    }
                }
            }
        }
    }
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_reports_syntax_and_handles_valid_instances_and_unavailable_references() {
        let cancel = AtomicBool::new(false);
        let source = "{\n  \"broken\": ]";
        let result = check(source, None, &cancel, 4096).unwrap();
        assert_eq!(result.notice, "JSON syntax error.");
        assert!(result.diagnostics.contains("line 2"));
        assert_eq!(&source[result.marks[0].bytes.clone()], "]");
        let schema = Schema::parse(br#"{"type":"integer","minimum":2}"#).unwrap();
        assert_eq!(
            check("3", Some(&schema), &cancel, 4096).unwrap().notice,
            "JSON matches the schema."
        );
        let unavailable =
            Schema::parse(br#"{"$ref":"https://schemas.invalid/external.json"}"#).unwrap();
        let result = check("{}", Some(&unavailable), &cancel, 4096).unwrap();
        assert_eq!(result.notice, "The JSON schema could not be used.");
        assert!(result.diagnostics.contains("schemas.invalid"));
        assert!(result.marks.is_empty());
        cancel.store(true, Ordering::Relaxed);
        assert!(check("3", Some(&schema), &cancel, 4096).is_err());
    }
}
