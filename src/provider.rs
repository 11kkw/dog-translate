use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{claude, openai};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    #[default]
    ChatGpt,
    Claude,
}

impl ProviderKind {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::ChatGpt => "ChatGPT",
            Self::Claude => "Claude",
        }
    }

    pub fn settings_key(self) -> &'static str {
        match self {
            Self::ChatGpt => "chatgpt",
            Self::Claude => "claude",
        }
    }
}

pub fn default_model(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::ChatGpt => openai::DEFAULT_MODEL,
        ProviderKind::Claude => claude::DEFAULT_MODEL,
    }
}

pub enum LlmClient {
    ChatGpt(openai::OpenAiClient),
    Claude(claude::ClaudeClient),
}

impl LlmClient {
    pub fn connect(
        provider: ProviderKind,
        requested_model: &str,
        requested_reasoning_effort: Option<&str>,
        status: impl FnMut(&str),
    ) -> Result<Self> {
        match provider {
            ProviderKind::ChatGpt => {
                openai::OpenAiClient::connect(requested_model, requested_reasoning_effort, status)
                    .map(Self::ChatGpt)
            }
            ProviderKind::Claude => {
                claude::ClaudeClient::connect(requested_model, requested_reasoning_effort, status)
                    .map(Self::Claude)
            }
        }
    }

    pub fn plan_type(&self) -> &str {
        match self {
            Self::ChatGpt(client) => client.plan_type(),
            Self::Claude(client) => client.plan_type(),
        }
    }

    pub fn model(&self) -> &str {
        match self {
            Self::ChatGpt(client) => client.model(),
            Self::Claude(client) => client.model(),
        }
    }

    pub fn reasoning_effort(&self) -> &str {
        match self {
            Self::ChatGpt(client) => client.reasoning_effort(),
            Self::Claude(client) => client.reasoning_effort(),
        }
    }

    pub fn available_models(&self) -> &[openai::ModelInfo] {
        match self {
            Self::ChatGpt(client) => client.available_models(),
            Self::Claude(client) => client.available_models(),
        }
    }

    pub fn translate(
        &mut self,
        source: &str,
        direction: openai::TranslationDirection,
    ) -> Result<openai::Translation> {
        match self {
            Self::ChatGpt(client) => client.translate(source, direction),
            Self::Claude(client) => client.translate(source, direction),
        }
    }

    pub fn translate_cancellable(
        &mut self,
        source: &str,
        direction: openai::TranslationDirection,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<Option<openai::Translation>> {
        match self {
            Self::ChatGpt(client) => client.translate_cancellable(source, direction, cancelled),
            Self::Claude(client) => client.translate_cancellable(source, direction, cancelled),
        }
    }

    pub fn translate_batch_cancellable(
        &mut self,
        sources: &[String],
        context_lines: &[String],
        direction: openai::TranslationDirection,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<Option<openai::TranslationBatch>> {
        match self {
            Self::ChatGpt(client) => {
                client.translate_batch_cancellable(sources, context_lines, direction, cancelled)
            }
            Self::Claude(client) => {
                client.translate_batch_cancellable(sources, context_lines, direction, cancelled)
            }
        }
    }
}
