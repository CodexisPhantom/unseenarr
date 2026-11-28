use serde::{Deserialize, Serialize};

/// One named setting of a Prowlarr indexer.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Field {
    pub name: String,
    pub value: Option<serde_json::Value>,
    pub privacy: String,
}

/// A Prowlarr v1 indexer resource, trimmed to what the import needs.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct IndexerResource {
    pub id: i32,
    pub name: String,
    pub fields: Vec<Field>,
    pub implementation: String,
    pub implementation_name: String,
    pub config_contract: String,
    pub info_link: String,
    pub tags: Vec<i32>,
    pub enable_rss: bool,
    pub enable_automatic_search: bool,
    pub enable_interactive_search: bool,
    pub supports_rss: bool,
    pub supports_search: bool,
    pub protocol: String,
    pub priority: i32,
    pub download_client_id: i32,
    pub definition_name: String,
    pub app_profile_id: i32,
}

impl IndexerResource {
    /// The field called `name`, if the indexer has one.
    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|field| field.name == name)
    }

    /// The `categories` field as a list of category ids. Prowlarr sends an
    /// array of integers; a comma-separated string is read too, so a hand-made
    /// fixture stays readable.
    pub fn categories(&self) -> Vec<i32> {
        let Some(value) = self
            .field("categories")
            .and_then(|field| field.value.as_ref())
        else {
            return Vec::new();
        };
        match value {
            serde_json::Value::Array(items) => items
                .iter()
                .filter_map(serde_json::Value::as_i64)
                .filter_map(|id| i32::try_from(id).ok())
                .collect(),
            serde_json::Value::String(text) => text
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .filter_map(|part| part.parse::<i32>().ok())
                .collect(),
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn resource(value: Option<serde_json::Value>) -> IndexerResource {
        IndexerResource {
            fields: vec![Field {
                name: "categories".to_owned(),
                value,
                privacy: "normal".to_owned(),
            }],
            ..IndexerResource::default()
        }
    }

    #[test]
    fn categories_read_an_array_or_a_list() {
        assert_eq!(
            resource(Some(json!([2000, 5000]))).categories(),
            vec![2000, 5000]
        );
        assert_eq!(
            resource(Some(json!("2000, 5000 , 5040"))).categories(),
            vec![2000, 5000, 5040]
        );
        assert_eq!(resource(None).categories(), Vec::<i32>::new());
    }
}
