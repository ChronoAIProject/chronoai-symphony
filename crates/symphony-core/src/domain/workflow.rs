use serde::{Deserialize, Serialize};

/// A parsed workflow definition consisting of YAML front matter and a markdown prompt template.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkflowDefinition {
    /// The YAML front matter root object containing all configuration.
    pub config: serde_yaml_ng::Value,

    /// The markdown body after the front matter, used as a prompt template.
    pub prompt_template: String,
}
