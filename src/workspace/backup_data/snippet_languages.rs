use std::io;

use roxmltree::Node;

use crate::snippet_window::languages;

use super::{RawFields, preference_node};

const KEY: &str = "custom_snippet_code_languages";

pub(super) fn read(root: Node<'_, '_>, omitted: &mut usize) -> Vec<String> {
    let list = match preference_node(root, "customSnippetCodeLanguages") {
        Ok(Some(list)) => list,
        Ok(None) => return Vec::new(),
        Err(()) => {
            *omitted += 1;
            return Vec::new();
        }
    };
    let mut result = Vec::new();
    for item in list.children().filter(Node::is_element) {
        let mut children = item.children();
        let token = if item.has_tag_name("language")
            && item.tag_name().namespace().is_none()
            && item.attributes().next().is_none()
        {
            children
                .next()
                .filter(Node::is_text)
                .and_then(|child| child.text())
                .filter(|_| children.next().is_none())
                .and_then(languages::normalize)
        } else {
            None
        };
        if let Some(token) = token {
            if languages::built_in(&token) || result.contains(&token) {
                continue;
            }
            if result.len() < languages::MAX_CUSTOM {
                result.push(token);
                continue;
            }
        }
        *omitted += 1;
    }
    result
}

pub(super) fn merge(fields: &mut RawFields, incoming: &[String]) -> io::Result<bool> {
    if incoming.is_empty() {
        return Ok(false);
    }
    let mut local: Vec<String> = fields
        .get(KEY)
        .map(|value| serde_json::from_str(value.get()))
        .transpose()?
        .unwrap_or_default();
    if !languages::valid(&local) {
        return Err(io::Error::other("Invalid saved snippet language catalogue"));
    }
    let before = local.len();
    let mut changed = false;
    local.retain(|language| language != "css");
    changed |= local.len() != before;
    let before = local.len();
    for language in incoming
        .iter()
        .filter(|language| !languages::built_in(language))
    {
        if local.len() == languages::MAX_CUSTOM {
            break;
        }
        if !local.contains(language) {
            local.push(language.clone());
        }
    }
    if !changed && local.len() == before {
        return Ok(false);
    }
    fields.insert(KEY.into(), serde_json::value::to_raw_value(&local)?);
    Ok(true)
}
