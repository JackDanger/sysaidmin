use std::io::{self, BufRead, Write};

use anyhow::{Context, Result, anyhow};
use reqwest::blocking::Client;
use serde::Deserialize;

use crate::config::AppConfig;

pub fn select_model(config: &AppConfig, cli_model: Option<String>) -> Result<String> {
    if let Some(m) = cli_model {
        return Ok(m);
    }
    if config.offline_mode {
        return Ok(config.model.clone());
    }

    let selector = ModelSelector::new(&config.api_key, &config.api_url)?;
    let stdin = io::stdin();
    let mut stdin_lock = stdin.lock();
    let mut stdout = io::stdout();
    match selector.prompt(config.model.as_str(), &mut stdin_lock, &mut stdout) {
        Ok(model) => Ok(model),
        Err(err) => {
            eprintln!(
                "Warning: failed to fetch model list ({err}). Falling back to {}.",
                config.model
            );
            Ok(config.model.clone())
        }
    }
}

struct ModelSelector {
    http: Client,
    endpoint: String,
}

impl ModelSelector {
    fn new(api_key: &str, api_url: &str) -> Result<Self> {
        let endpoint = build_models_endpoint(api_url)?;
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "x-api-key",
            reqwest::header::HeaderValue::from_str(api_key)
                .context("invalid API key header for models request")?,
        );
        headers.insert(
            "anthropic-version",
            reqwest::header::HeaderValue::from_static("2023-06-01"),
        );
        let http = Client::builder().default_headers(headers).build()?;

        Ok(Self { http, endpoint })
    }

    fn prompt(
        &self,
        default_model: &str,
        reader: &mut dyn BufRead,
        writer: &mut dyn Write,
    ) -> Result<String> {
        let models = self.fetch_models()?;
        if models.is_empty() {
            return Err(anyhow!("Anthropic API returned an empty model list"));
        }

        writeln!(writer, "\nAvailable Anthropic models:")?;
        for (idx, model) in models.iter().enumerate() {
            let is_default = model.id == default_model;
            writeln!(
                writer,
                "[{}] {} ({}){}",
                idx + 1,
                model.display_name.as_deref().unwrap_or(&model.id),
                model.id,
                if is_default { " [default]" } else { "" }
            )?;
        }

        loop {
            write!(
                writer,
                "\nSelect model by number (Enter for default '{}'): ",
                default_model
            )?;
            writer.flush()?;
            let mut input = String::new();
            reader.read_line(&mut input)?;
            let trimmed = input.trim();
            if trimmed.is_empty() {
                return Ok(default_model.to_string());
            }

            match trimmed.parse::<usize>() {
                Ok(choice) if choice >= 1 && choice <= models.len() => {
                    return Ok(models[choice - 1].id.clone());
                }
                _ => {
                    writeln!(writer, "Invalid selection '{}'. Please try again.", trimmed)?;
                }
            }
        }
    }

    fn fetch_models(&self) -> Result<Vec<ModelInfo>> {
        let resp = self
            .http
            .get(&self.endpoint)
            .send()
            .context("failed requesting model list from Anthropic")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp
                .text()
                .unwrap_or_else(|_| "unable to read response body".into());
            return Err(anyhow!(
                "Anthropic model list failed with {}: {}",
                status.as_u16(),
                body
            ));
        }
        let parsed: ModelsResponse = resp
            .json()
            .context("failed to parse Anthropic model list response")?;
        Ok(parsed.data)
    }
}

fn build_models_endpoint(api_url: &str) -> Result<String> {
    let mut url = reqwest::Url::parse(api_url)
        .context("invalid ANTHROPIC API URL - expected absolute URL")?;
    url.set_path("v1/models");
    url.set_query(None);
    Ok(url.to_string())
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Vec<ModelInfo>,
}

#[derive(Debug, Deserialize)]
struct ModelInfo {
    id: String,
    #[serde(default)]
    display_name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_models_endpoint_rewrites_path() {
        let url = build_models_endpoint("https://api.anthropic.com/v1/messages").unwrap();
        assert_eq!(url, "https://api.anthropic.com/v1/models");
    }
}
