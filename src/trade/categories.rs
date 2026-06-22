//! trade2 item-category taxonomy (from `/data/filters`), used to offer `/harvest`
//! autocomplete and to issue category-filtered searches.

use anyhow::{Context, Result};
use serde_json::Value;

use crate::trade::client::TradeClient;

#[derive(Clone, Debug, PartialEq)]
pub struct Category {
    /// trade2 category option id, e.g. "weapon.staff".
    pub id: String,
    /// Human label, e.g. "Staff".
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct CategoryCatalog {
    categories: Vec<Category>,
}

impl CategoryCatalog {
    /// Parses the `category` filter's options out of a `/data/filters` body.
    /// Walks to the object whose `"id" == "category"` and reads `option.options`,
    /// skipping the null-id "Any" entry.
    pub fn from_filters_json(body: &str) -> Self {
        let v: Value = serde_json::from_str(body).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "failed to parse /data/filters; using empty category catalog");
            Value::Null
        });
        let mut categories = Vec::new();
        Self::collect(&v, &mut categories);
        CategoryCatalog { categories }
    }

    fn collect(v: &Value, out: &mut Vec<Category>) {
        match v {
            Value::Object(obj) => {
                if obj.get("id").and_then(|x| x.as_str()) == Some("category") {
                    if let Some(opts) = obj
                        .get("option")
                        .and_then(|o| o.get("options"))
                        .and_then(|o| o.as_array())
                    {
                        for o in opts {
                            let id = o.get("id").and_then(|x| x.as_str());
                            let text = o.get("text").and_then(|x| x.as_str());
                            if let (Some(id), Some(text)) = (id, text) {
                                out.push(Category {
                                    id: id.to_string(),
                                    text: text.to_string(),
                                });
                            }
                        }
                        return; // found the category filter; stop recursing
                    }
                }
                for (_, val) in obj {
                    Self::collect(val, out);
                }
            }
            Value::Array(arr) => {
                for val in arr {
                    Self::collect(val, out);
                }
            }
            _ => {}
        }
    }

    /// Fetches `/data/filters` and parses it. Returns an error on fetch failure.
    pub async fn fetch(client: &TradeClient) -> Result<Self> {
        let body = client
            .fetch_filters_raw()
            .await
            .context("failed to fetch trade2 /data/filters")?;
        Ok(Self::from_filters_json(&body))
    }

    /// Returns all known categories.
    pub fn all(&self) -> &[Category] {
        &self.categories
    }

    /// Case-insensitive prefix match on the human text, for autocomplete.
    pub fn matches(&self, prefix: &str) -> Vec<&Category> {
        let p = prefix.to_lowercase();
        self.categories
            .iter()
            .filter(|c| c.text.to_lowercase().starts_with(&p))
            .collect()
    }

    /// The trade2 category id for an exact human text (autocomplete returns text).
    pub fn id_for_text(&self, text: &str) -> Option<&str> {
        self.categories
            .iter()
            .find(|c| c.text == text)
            .map(|c| c.id.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_category_options_skipping_any() {
        let cat = CategoryCatalog::from_filters_json(include_str!("fixtures/filters_sample.json"));
        let ids: Vec<&str> = cat.all().iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&"weapon.staff"));
        assert!(ids.contains(&"accessory.amulet"));
        // The null-id "Any" option is skipped (not a harvestable category).
        assert!(cat.all().iter().all(|c| !c.id.is_empty()));
    }

    #[test]
    fn matches_is_case_insensitive_prefix() {
        let cat = CategoryCatalog::from_filters_json(include_str!("fixtures/filters_sample.json"));
        let m: Vec<&str> = cat.matches("sta").iter().map(|c| c.text.as_str()).collect();
        assert!(m.contains(&"Staff"));
        assert!(!m.contains(&"Helmet"));
    }

    #[test]
    fn id_for_text_returns_correct_id() {
        let cat = CategoryCatalog::from_filters_json(include_str!("fixtures/filters_sample.json"));
        assert_eq!(cat.id_for_text("Staff"), Some("weapon.staff"));
        assert_eq!(cat.id_for_text("Amulet"), Some("accessory.amulet"));
        assert_eq!(cat.id_for_text("NonExistent"), None);
    }
}
