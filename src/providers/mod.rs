//! Snapshot of the upstream provider registry and model metadata.
use serde::Deserialize;
use std::sync::OnceLock;

#[derive(Debug, Deserialize)]
pub struct Provider {
    pub id: String,
    pub alias: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default, rename = "uiAlias")]
    pub ui_alias: String,
    #[serde(default, rename = "noAuthFree")]
    pub no_auth_free: bool,
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
        .or_else(|| all().iter().find(|p| p.alias == id_or_alias || p.ui_alias == id_or_alias || p.aliases.iter().any(|a| a == id_or_alias)))
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

    #[test]
    fn upstream_aliases_resolve_with_id_precedence() {
        for provider in all() {
            for alias in provider.aliases.iter().chain(std::iter::once(&provider.ui_alias)).filter(|a| !a.is_empty()) {
                let expected = all().iter().find(|p| p.id == *alias).or_else(|| all().iter().find(|p| p.alias == *alias || p.ui_alias == *alias || p.aliases.contains(alias))).unwrap();
                assert_eq!(lookup(alias).unwrap().id, expected.id, "{alias}");
                assert_eq!(crate::handlers::chat::parse_model_target(&format!("{alias}/model")).unwrap().provider, expected.id);
            }
        }
        assert_eq!(lookup("cmc").unwrap().id, "commandcode");
    }
}
