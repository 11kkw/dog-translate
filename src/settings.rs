use std::{collections::HashMap, fs, path::PathBuf};

use serde::{Deserialize, Serialize};

use super::{openai, provider};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum OverlayLayoutMode {
    #[default]
    Reader,
    Paragraph,
    Source,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct AppSettings {
    #[serde(default)]
    pub(super) provider: provider::ProviderKind,
    #[serde(default = "default_model")]
    pub(super) model: String,
    #[serde(default)]
    pub(super) provider_models: HashMap<String, String>,
    #[serde(default)]
    pub(super) reasoning_efforts: HashMap<String, String>,
    #[serde(default)]
    pub(super) overlay_layout: OverlayLayoutMode,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            provider: provider::ProviderKind::default(),
            model: default_model(),
            provider_models: HashMap::new(),
            reasoning_efforts: HashMap::new(),
            overlay_layout: OverlayLayoutMode::default(),
        }
    }
}

pub(super) fn load() -> AppSettings {
    let mut settings: AppSettings = settings_path()
        .and_then(|path| fs::read_to_string(path).ok())
        .or_else(|| legacy_settings_path().and_then(|path| fs::read_to_string(path).ok()))
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default();
    settings.model = settings.model.trim().to_owned();
    if settings.model.is_empty() || settings.model.len() > 128 {
        settings.model = provider::default_model(settings.provider).to_owned();
    }
    settings.provider_models.retain(|provider, model| {
        matches!(provider.as_str(), "chatgpt" | "claude")
            && !model.trim().is_empty()
            && model.len() <= 128
    });
    let provider_key = settings.provider.settings_key().to_owned();
    settings
        .provider_models
        .entry(provider_key)
        .or_insert_with(|| settings.model.clone());
    settings.model = settings
        .provider_models
        .get(settings.provider.settings_key())
        .cloned()
        .unwrap_or_else(|| provider::default_model(settings.provider).to_owned());
    settings.reasoning_efforts.retain(|model, effort| {
        !model.trim().is_empty()
            && model.len() <= 128
            && !effort.trim().is_empty()
            && effort.len() <= 32
    });
    settings
}

pub(super) fn save(settings: &AppSettings) -> Result<(), String> {
    let path = settings_path().ok_or_else(|| "설정 저장 폴더를 찾지 못했습니다".to_owned())?;
    let parent = path
        .parent()
        .ok_or_else(|| "설정 저장 폴더가 올바르지 않습니다".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| format!("모델 설정 폴더 생성 실패: {error}"))?;
    let contents = serde_json::to_vec_pretty(settings)
        .map_err(|error| format!("모델 설정 변환 실패: {error}"))?;
    fs::write(path, contents).map_err(|error| format!("모델 설정 저장 실패: {error}"))
}

fn settings_path() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        std::env::var_os("LOCALAPPDATA")
            .or_else(|| std::env::var_os("APPDATA"))
            .map(PathBuf::from)
            .map(|root| root.join("DogTranslate").join("settings.json"))
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(PathBuf::from).map(|root| {
            root.join("Library")
                .join("Application Support")
                .join("DogTranslate")
                .join("settings.json")
        })
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|root| root.join(".config"))
            })
            .map(|root| root.join("dog-translate").join("settings.json"))
    }
}

fn legacy_settings_path() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        std::env::var_os("LOCALAPPDATA")
            .or_else(|| std::env::var_os("APPDATA"))
            .map(PathBuf::from)
            .map(|root| root.join("DofTranslate").join("settings.json"))
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(PathBuf::from).map(|root| {
            root.join("Library")
                .join("Application Support")
                .join("DofTranslate")
                .join("settings.json")
        })
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|root| root.join(".config"))
            })
            .map(|root| root.join("dof-translate").join("settings.json"))
    }
}

fn default_model() -> String {
    openai::DEFAULT_MODEL.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_legacy_model_only_settings() {
        let settings: AppSettings = serde_json::from_str(r#"{"model":"gpt-5.4-mini"}"#).unwrap();
        assert_eq!(settings.provider, provider::ProviderKind::ChatGpt);
        assert_eq!(settings.model, "gpt-5.4-mini");
        assert!(settings.provider_models.is_empty());
        assert!(settings.reasoning_efforts.is_empty());
        assert_eq!(settings.overlay_layout, OverlayLayoutMode::Reader);
    }

    #[test]
    fn loads_paragraph_overlay_layout() {
        let settings: AppSettings =
            serde_json::from_str(r#"{"overlay_layout":"paragraph"}"#).unwrap();
        assert_eq!(settings.overlay_layout, OverlayLayoutMode::Paragraph);
    }

    #[test]
    fn loads_claude_provider_settings() {
        let settings: AppSettings = serde_json::from_str(
            r#"{"provider":"claude","model":"sonnet","provider_models":{"chatgpt":"gpt-5.6-luna","claude":"sonnet"}}"#,
        )
        .unwrap();
        assert_eq!(settings.provider, provider::ProviderKind::Claude);
        assert_eq!(settings.model, "sonnet");
    }
}
