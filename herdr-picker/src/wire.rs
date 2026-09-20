//! Provider wire types: the JSON frames a source process writes to the picker.

use std::{collections::HashSet, fmt};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tone {
    Muted,
    Accent,
    Success,
    Warning,
    Danger,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Item {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub subtitle: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub badge: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub indicator: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tone: Option<Tone>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub spinning: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub search: String,
    pub value: Value,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub items: Vec<Item>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ProviderMessage {
    Snapshot(Snapshot),
    Error { error: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemsError {
    EmptyId { index: usize },
    EmptyTitle { index: usize },
    DuplicateId { id: String },
}

impl fmt::Display for ItemsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyId { index } => write!(f, "items[{index}].id must not be empty"),
            Self::EmptyTitle { index } => write!(f, "items[{index}].title must not be empty"),
            Self::DuplicateId { id } => write!(f, "duplicate item id {id:?}"),
        }
    }
}

impl std::error::Error for ItemsError {}

pub fn validate_items(items: &[Item]) -> Result<(), ItemsError> {
    let mut ids = HashSet::new();
    for (index, item) in items.iter().enumerate() {
        if item.id.trim().is_empty() {
            return Err(ItemsError::EmptyId { index });
        }
        if item.title.trim().is_empty() {
            return Err(ItemsError::EmptyTitle { index });
        }
        if !ids.insert(&item.id) {
            return Err(ItemsError::DuplicateId {
                id: item.id.clone(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn provider_messages_reject_mixed_or_malformed_frames() {
        for frame in [
            json!({}),
            json!({ "items": [], "error": "offline" }),
            json!({ "error": 42 }),
            json!({ "error": "offline", "unknown": true }),
        ] {
            assert!(serde_json::from_value::<ProviderMessage>(frame).is_err());
        }
    }
}
