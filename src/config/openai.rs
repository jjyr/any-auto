//! OpenAI configuration overlays and validated runtime settings.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAiSettings {
    pub model: Option<String>,
    pub base_url: Option<String>,
    // Settings are serialized internally for source tracking; never expose them.
    pub api_key: Option<String>,
    #[serde(default)]
    pub common: OpenAiCommonSettings,
    #[serde(default)]
    pub llama_cpp: LlamaCppSettings,
}

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAiCommonSettings {
    pub effort: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_output_tokens: Option<u32>,
}

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlamaCppSettings {
    pub top_k: Option<u32>,
    pub min_p: Option<f64>,
    pub presence_penalty: Option<f64>,
    pub repeat_penalty: Option<f64>,
    pub reasoning_budget_tokens: Option<i32>,
}

#[derive(Clone, Serialize)]
pub struct OpenAiConfig {
    pub model: String,
    pub base_url: String,
    #[serde(serialize_with = "super::serialize_api_key")]
    pub api_key: String,
    pub common: OpenAiCommonSettings,
    pub llama_cpp: LlamaCppSettings,
}

fn replace<T>(value: &mut Option<T>, overlay: Option<T>) {
    if overlay.is_some() {
        *value = overlay;
    }
}

impl OpenAiSettings {
    pub fn merge(&mut self, overlay: Self) {
        replace(&mut self.model, overlay.model);
        replace(&mut self.base_url, overlay.base_url);
        replace(&mut self.api_key, overlay.api_key);
        self.common.merge(overlay.common);
        self.llama_cpp.merge(overlay.llama_cpp);
    }

    pub fn resolve(self) -> Result<OpenAiConfig> {
        self.common.validate()?;
        self.llama_cpp.validate()?;
        let model = self.model.unwrap_or_default().trim().to_owned();
        ensure!(!model.is_empty(), "OpenAI approver requires openai.model");
        let api_key = self.api_key.unwrap_or_default();
        ensure!(
            !api_key.contains(['\r', '\n', '\0']),
            "Invalid API key: control characters are not allowed"
        );
        Ok(OpenAiConfig {
            model,
            base_url: self
                .base_url
                .unwrap_or_else(|| "https://api.openai.com/v1".into()),
            api_key,
            common: OpenAiCommonSettings {
                effort: self.common.effort.filter(|v| !v.trim().is_empty()),
                ..self.common
            },
            llama_cpp: self.llama_cpp,
        })
    }
}

fn finite_range(name: &str, value: Option<f64>, min: f64, max: f64) -> Result<()> {
    if let Some(value) = value {
        ensure!(
            value.is_finite() && (min..=max).contains(&value),
            "Invalid {name}: expected a finite value between {min} and {max}"
        );
    }
    Ok(())
}

impl OpenAiCommonSettings {
    fn merge(&mut self, overlay: Self) {
        replace(&mut self.effort, overlay.effort);
        replace(&mut self.temperature, overlay.temperature);
        replace(&mut self.top_p, overlay.top_p);
        replace(&mut self.max_output_tokens, overlay.max_output_tokens);
    }

    fn validate(&self) -> Result<()> {
        if let Some(effort) = &self.effort {
            ensure!(
                effort.trim().is_empty()
                    || matches!(
                        effort.as_str(),
                        "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
                    ),
                "Unsupported openai.common.effort {effort:?}"
            );
        }
        finite_range("openai.common.temperature", self.temperature, 0.0, f64::MAX)?;
        finite_range("openai.common.top_p", self.top_p, 0.0, 1.0)?;
        ensure!(
            self.max_output_tokens
                .is_none_or(|v| v > 0 && v <= i32::MAX as u32),
            "openai.common.max_output_tokens must be between 1 and 2147483647"
        );
        Ok(())
    }

    fn apply(&self, body: &mut Value) {
        if let Some(effort) = &self.effort {
            body["reasoning"] = json!({"effort": effort});
        }
        if let Some(value) = self.temperature {
            body["temperature"] = json!(value);
        }
        if let Some(value) = self.top_p {
            body["top_p"] = json!(value);
        }
        if let Some(value) = self.max_output_tokens {
            body["max_output_tokens"] = json!(value);
        }
    }
}

impl LlamaCppSettings {
    fn merge(&mut self, overlay: Self) {
        replace(&mut self.top_k, overlay.top_k);
        replace(&mut self.min_p, overlay.min_p);
        replace(&mut self.presence_penalty, overlay.presence_penalty);
        replace(&mut self.repeat_penalty, overlay.repeat_penalty);
        replace(
            &mut self.reasoning_budget_tokens,
            overlay.reasoning_budget_tokens,
        );
    }

    fn validate(&self) -> Result<()> {
        finite_range("openai.llama_cpp.min_p", self.min_p, 0.0, 1.0)?;
        finite_range(
            "openai.llama_cpp.presence_penalty",
            self.presence_penalty,
            -2.0,
            2.0,
        )?;
        finite_range(
            "openai.llama_cpp.repeat_penalty",
            self.repeat_penalty,
            0.0,
            f64::MAX,
        )?;
        ensure!(
            self.top_k.is_none_or(|v| v <= i32::MAX as u32),
            "openai.llama_cpp.top_k must fit a signed 32-bit integer"
        );
        ensure!(
            self.reasoning_budget_tokens.is_none_or(|v| v >= -1),
            "openai.llama_cpp.reasoning_budget_tokens must be -1 or nonnegative"
        );
        Ok(())
    }

    fn apply(&self, body: &mut Value) {
        if let Some(value) = self.top_k {
            body["top_k"] = json!(value);
        }
        if let Some(value) = self.min_p {
            body["min_p"] = json!(value);
        }
        if let Some(value) = self.presence_penalty {
            body["presence_penalty"] = json!(value);
        }
        if let Some(value) = self.repeat_penalty {
            body["repeat_penalty"] = json!(value);
        }
        if let Some(value) = self.reasoning_budget_tokens {
            body["reasoning_budget_tokens"] = json!(value);
        }
    }
}

impl OpenAiConfig {
    pub fn apply_generation(&self, body: &mut Value) {
        self.common.apply(body);
        self.llama_cpp.apply(body);
    }
}
