//! Snapshot of the upstream provider registry and model metadata.
use serde::Deserialize;
use std::sync::OnceLock;

#[derive(Debug, Deserialize)]
pub struct Provider {
    pub id: String,
    pub alias: String,
    pub base_url: Option<String>,
    pub format: Option<String>,
    pub models: Vec<Model>,
}

#[derive(Debug, Deserialize)]
pub struct Model {
    pub id: String,
    pub name: Option<String>,
    #[serde(flatten)]
    pub metadata: std::collections::HashMap<String, serde_json::Value>,
}

pub fn all() -> &'static [Provider] {
    static PROVIDERS: OnceLock<Vec<Provider>> = OnceLock::new();
    PROVIDERS.get_or_init(|| {
        serde_json::from_str(include_str!("registry.json"))
            .expect("valid provider registry snapshot")
    })
}

pub fn lookup(id_or_alias: &str) -> Option<&'static Provider> {
    all()
        .iter()
        .find(|p| p.id == id_or_alias)
        .or_else(|| all().iter().find(|p| p.alias == id_or_alias))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aliases_and_model_counts_match_snapshot() {
        let source: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("registry.json")).unwrap();
        assert_eq!(all().len(), source.len());
        for (provider, original) in all().iter().zip(source.iter()) {
            assert_eq!(lookup(&provider.id).unwrap().id, provider.id);
            let expected = all()
                .iter()
                .find(|p| p.id == provider.alias)
                .or_else(|| all().iter().find(|p| p.alias == provider.alias))
                .unwrap();
            assert_eq!(lookup(&provider.alias).unwrap().id, expected.id);
            assert_eq!(
                provider.models.len(),
                original["models"].as_array().unwrap().len(),
                "{}",
                provider.id
            );
        }
    }
}
