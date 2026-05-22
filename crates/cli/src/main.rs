use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use rotator::{
    AuthType, CircuitBreakerRegistry, CooldownManager, CredentialManager, GoogleOAuthFlow,
    IflowOAuthFlow, OAuthFlow, OAuthToken, ProviderRegistry, QwenOAuthFlow,
};
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

const OAUTH_CREDS_DIR: &str = "oauth_creds";

#[derive(Debug, Parser)]
#[command(
    name = "proxy-cli",
    version,
    about = "LLM API proxy credential utility"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Credentials {
        #[command(subcommand)]
        command: CredentialCommands,
    },
    Oauth {
        #[command(subcommand)]
        command: OauthCommands,
    },
    Env {
        #[command(subcommand)]
        command: EnvCommands,
    },
}

#[derive(Debug, Subcommand)]
enum CredentialCommands {
    List,
    Show { provider: String },
    Add { provider: String, key: String },
}

#[derive(Debug, Subcommand)]
enum OauthCommands {
    Setup { provider: String },
    List,
    Delete { provider: String, index: usize },
}

#[derive(Debug, Subcommand)]
enum EnvCommands {
    Export,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Credentials { command } => run_credentials(command),
        Commands::Oauth { command } => run_oauth(command).await,
        Commands::Env { command } => run_env(command),
    }
}

fn run_credentials(command: CredentialCommands) -> Result<()> {
    let manager = CredentialManager::from_env();
    match command {
        CredentialCommands::List => list_credentials(&manager),
        CredentialCommands::Show { provider } => show_credentials(&manager, &provider),
        CredentialCommands::Add { provider, key } => add_env_credential(&provider, &key),
    }
}

fn add_env_credential(provider: &str, key: &str) -> Result<()> {
    let provider = provider.trim().to_ascii_uppercase();
    let key = key.trim();
    if provider.is_empty() || !provider.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        bail!("invalid provider name: must be alphanumeric with underscores");
    }
    if key.is_empty() || key.contains('\n') || key.contains('\r') {
        bail!("invalid API key: contains newline or is empty");
    }
    let mut env_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(".env")
        .context("open .env")?;
    writeln!(env_file, "{provider}_API_KEY={key}").context("write credential to .env")?;
    println!("Added credential for provider {provider} to .env");
    Ok(())
}

fn list_credentials(manager: &CredentialManager) -> Result<()> {
    let mut providers: Vec<_> = manager
        .credentials
        .iter()
        .map(|entry| entry.key().clone())
        .collect();
    providers.sort();

    if providers.is_empty() {
        println!("No API key credentials found in environment.");
        return Ok(());
    }

    for provider in providers {
        let Some(entries) = manager.credentials.get(&provider) else {
            continue;
        };
        let limit = entries
            .first()
            .map(|entry| entry.concurrent_limit)
            .unwrap_or(0);
        let masked_keys = entries
            .iter()
            .map(|entry| mask_key(&entry.key))
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "{provider}: {} key(s), concurrent_limit={limit}, keys=[{masked_keys}]",
            entries.len()
        );
    }
    Ok(())
}

fn show_credentials(manager: &CredentialManager, provider: &str) -> Result<()> {
    let cooldowns = CooldownManager::new();
    let breakers = CircuitBreakerRegistry::new();
    let Some(entries) = manager.credentials.get(provider) else {
        bail!("no credentials found for provider '{provider}'");
    };

    println!("provider: {provider}");
    println!("credentials: {}", entries.len());
    println!("circuit_breaker: {:?}", breakers.get_state(provider));
    for (index, entry) in entries.iter().enumerate() {
        let usage = entry.current_requests.load(Ordering::Relaxed);
        let cooldown = if cooldowns.is_available(provider, &entry.key) {
            "available"
        } else {
            "cooldown"
        };
        println!(
            "{}: id={}, key={}, concurrent_usage={}/{}, cooldown={}",
            index + 1,
            key_id(&entry.key),
            mask_key(&entry.key),
            usage,
            entry.concurrent_limit,
            cooldown
        );
    }
    Ok(())
}

async fn run_oauth(command: OauthCommands) -> Result<()> {
    match command {
        OauthCommands::Setup { provider } => setup_oauth(&provider).await,
        OauthCommands::List => list_oauth(),
        OauthCommands::Delete { provider, index } => delete_oauth(&provider, index),
    }
}

async fn setup_oauth(provider: &str) -> Result<()> {
    let mut registry = ProviderRegistry::new();
    registry.load_from_env();
    let definition = registry
        .get(provider)
        .ok_or_else(|| anyhow!("unknown provider '{provider}'"))?;
    if definition.auth_type != AuthType::OAuth {
        bail!("provider '{provider}' does not use OAuth");
    }

    let client = reqwest::Client::new();
    let flow = oauth_flow(provider)?;
    let token = flow.authenticate(&client).await?;
    let path = next_oauth_path(provider)?;
    fs::create_dir_all(OAUTH_CREDS_DIR).context("create oauth_creds directory")?;
    let data = serde_json::to_string_pretty(&token).context("serialize OAuth token")?;
    fs::write(&path, data).with_context(|| format!("write {}", path.display()))?;

    println!("Saved OAuth token to {}", path.display());
    println!("expires_at: {}", format_expires_at(token.expires_at));
    Ok(())
}

fn list_oauth() -> Result<()> {
    let dir = Path::new(OAUTH_CREDS_DIR);
    if !dir.exists() {
        println!("No OAuth credential directory found.");
        return Ok(());
    }

    let mut files = Vec::new();
    for entry in fs::read_dir(dir).context("read oauth_creds directory")? {
        let entry = entry?;
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            files.push(path);
        }
    }
    files.sort();

    if files.is_empty() {
        println!("No OAuth credential files found.");
        return Ok(());
    }

    for path in files {
        let token: OAuthToken = serde_json::from_str(
            &fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?,
        )
        .with_context(|| format!("parse {}", path.display()))?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let provider = oauth_provider_from_file(file_name).unwrap_or("unknown");
        println!(
            "{provider}: file={file_name}, expires_at={}, refresh_token={}",
            format_expires_at(token.expires_at),
            token.refresh_token.is_some()
        );
    }
    Ok(())
}

fn delete_oauth(provider: &str, index: usize) -> Result<()> {
    let path = oauth_path(provider, index);
    if !path.exists() {
        bail!("OAuth credential file not found: {}", path.display());
    }

    print!("Delete {}? [y/N] ", path.display());
    io::stdout().flush().context("flush prompt")?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .context("read confirmation")?;
    if !matches!(answer.trim(), "y" | "Y" | "yes" | "YES") {
        println!("Aborted.");
        return Ok(());
    }

    fs::remove_file(&path).with_context(|| format!("delete {}", path.display()))?;
    println!("Deleted {}", path.display());
    Ok(())
}

fn run_env(command: EnvCommands) -> Result<()> {
    match command {
        EnvCommands::Export => export_env(),
    }
}

fn export_env() -> Result<()> {
    let manager = CredentialManager::from_env();
    let mut providers: Vec<_> = manager
        .credentials
        .iter()
        .map(|entry| entry.key().clone())
        .collect();
    providers.sort();

    for provider in providers {
        let Some(entries) = manager.credentials.get(&provider) else {
            continue;
        };
        for (index, entry) in entries.iter().enumerate() {
            println!(
                "export {}_API_KEY_{}=\"{}\"",
                provider.to_ascii_uppercase(),
                index + 1,
                escape_shell_double_quoted(&entry.key)
            );
        }
    }
    Ok(())
}

fn oauth_flow(provider: &str) -> Result<Box<dyn OAuthFlow>> {
    if provider.contains("gemini") || provider.contains("antigravity") {
        return Ok(Box::new(GoogleOAuthFlow));
    }
    if provider.contains("qwen") {
        return Ok(Box::new(QwenOAuthFlow));
    }
    if provider.contains("iflow") {
        return Ok(Box::new(IflowOAuthFlow));
    }
    bail!("no OAuth flow is registered for provider '{provider}'")
}

fn next_oauth_path(provider: &str) -> Result<PathBuf> {
    for index in 1.. {
        let path = oauth_path(provider, index);
        if !path.exists() {
            return Ok(path);
        }
    }
    unreachable!()
}

fn oauth_path(provider: &str, index: usize) -> PathBuf {
    Path::new(OAUTH_CREDS_DIR).join(format!("{provider}_oauth_{index}.json"))
}

fn oauth_provider_from_file(file_name: &str) -> Option<&str> {
    file_name
        .split_once("_oauth_")
        .map(|(provider, _)| provider)
}

fn mask_key(key: &str) -> String {
    let chars: Vec<_> = key.chars().collect();
    if chars.len() <= 8 {
        return "****".to_owned();
    }
    let first: String = chars.iter().take(4).collect();
    let last: String = chars.iter().skip(chars.len() - 4).collect();
    format!("{first}...{last}")
}

fn key_id(key: &str) -> String {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn format_expires_at(expires_at: Option<u64>) -> String {
    match expires_at {
        Some(timestamp) => timestamp.to_string(),
        None => "unknown".to_owned(),
    }
}

fn escape_shell_double_quoted(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}
