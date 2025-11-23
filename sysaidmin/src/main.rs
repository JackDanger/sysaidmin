mod allowlist;
mod api;
mod app;
mod config;
mod executor;
mod models;
mod parser;
mod session;
mod task;
mod tui;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Explicitly set the Anthropic model (skips interactive selection)
    #[arg(long)]
    model: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut config = config::AppConfig::load()?;
    let selected_model = models::select_model(&config, cli.model)?;
    config.model = selected_model;
    let allowlist_cfg = config.allowlist.clone();
    let client = api::AnthropicClient::new(&config)?;
    let allowlist = allowlist::Allowlist::from_config(allowlist_cfg)?;
    let executor = executor::Executor::new(config.dry_run);
    let session = session::SessionStore::new(config.session_root.clone())?;
    let mut app = app::App::new(config, client, allowlist, executor, session);
    tui::run(&mut app)?;
    Ok(())
}
