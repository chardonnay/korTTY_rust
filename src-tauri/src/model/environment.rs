use serde::{Deserialize, Serialize};

pub const BUILT_IN_ENVIRONMENTS: [(&str, &str); 4] = [
    ("Production", "Production"),
    ("Development", "Development"),
    ("Test", "Test"),
    ("Staging", "Staging"),
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentDefinition {
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub built_in: bool,
}

impl EnvironmentDefinition {
    pub fn new_custom(id: String, display_name: String) -> Self {
        Self {
            id,
            display_name,
            built_in: false,
        }
    }
}

pub fn built_in_environments() -> Vec<EnvironmentDefinition> {
    BUILT_IN_ENVIRONMENTS
        .into_iter()
        .map(|(id, display_name)| EnvironmentDefinition {
            id: id.to_string(),
            display_name: display_name.to_string(),
            built_in: true,
        })
        .collect()
}

pub fn default_environment_id() -> String {
    BUILT_IN_ENVIRONMENTS[0].0.to_string()
}

pub fn is_built_in_environment(id: &str) -> bool {
    BUILT_IN_ENVIRONMENTS
        .iter()
        .any(|(built_in_id, _)| *built_in_id == id)
}
