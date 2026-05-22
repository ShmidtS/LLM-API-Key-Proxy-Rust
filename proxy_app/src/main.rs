use clap::Parser;
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    net::SocketAddr,
    path::Path,
    process,
    time::Duration,
};
use tokio::time::timeout;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{
    Layer,
    filter::{EnvFilter, LevelFilter},
    fmt::{self, format::FmtSpan},
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

#[derive(Debug, Parser)]
struct Args {
    #[arg(long)]
    host: Option<String>,
    #[arg(long)]
    port: Option<u16>,
    #[arg(long, default_value_t = false)]
    enable_raw_logging: bool,
    #[arg(long, help = "Launch interactive tool to add a credential")]
    add_credential: bool,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    std::fs::create_dir_all("logs").unwrap_or_else(|err| {
        eprintln!("failed to create logs directory: {err}");
        process::exit(1);
    });

    let info_file_appender = tracing_appender::rolling::never("logs", "proxy.log");
    let (info_appender, _info_guard): (_, WorkerGuard) =
        tracing_appender::non_blocking(info_file_appender);
    let debug_file_appender = tracing_appender::rolling::never("logs", "proxy_debug.log");
    let (debug_appender, _debug_guard): (_, WorkerGuard) =
        tracing_appender::non_blocking(debug_file_appender);

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("proxy_app=debug,tower_http=debug")
            .add_directive(LevelFilter::INFO.into())
            .add_directive("hyper=warn".parse().unwrap())
            .add_directive("reqwest=warn".parse().unwrap())
            .add_directive("tokio_util=warn".parse().unwrap())
    });

    tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .with_writer(std::io::stdout)
                .with_ansi(true)
                .with_span_events(FmtSpan::NONE)
                .with_filter(LevelFilter::INFO),
        )
        .with(
            fmt::layer()
                .with_writer(info_appender)
                .with_ansi(false)
                .json()
                .with_filter(LevelFilter::INFO),
        )
        .with(
            fmt::layer()
                .with_writer(debug_appender)
                .with_ansi(false)
                .json()
                .with_filter(LevelFilter::DEBUG),
        )
        .init();

    ensure_env_file().unwrap_or_else(|err| {
        tracing::error!("failed to create .env: {err}");
        process::exit(1);
    });

    if args.add_credential {
        add_credential_interactive().unwrap_or_else(|err| {
            tracing::error!("failed to add credential: {err}");
            process::exit(1);
        });
        process::exit(0);
    }

    let mut config = proxy_config::load_from_env().unwrap_or_else(|err| {
        tracing::error!("failed to load config: {err}");
        process::exit(1);
    });
    if config.auth_enabled
        && config
            .proxy_api_key
            .as_deref()
            .is_none_or(|key| key.trim().is_empty())
    {
        eprintln!(
            "Error: PROXY_API_KEY is not set. Run with --add-credential to add credentials, or set PROXY_API_KEY in .env"
        );
        process::exit(1);
    }
    if let Some(host) = args.host {
        config.host = host;
    }
    if let Some(port) = args.port {
        config.port = port;
    }
    if args.enable_raw_logging {
        config.enable_raw_logging = true;
    }
    let addr: SocketAddr = format!("{}:{}", config.host, config.port)
        .parse()
        .unwrap_or_else(|err| {
            tracing::error!("failed to parse socket address: {err}");
            process::exit(1);
        });
    let graceful_shutdown_timeout_secs = config.graceful_shutdown_timeout_secs;
    let state = proxy_app::state::AppState::from_config(config);
    let app = proxy_app::build_app_with_state(state);
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    tracing::info!("listening on {}", addr);
    tracing::info!("LLM API Key Proxy Rust app started");
    let server = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal());

    match timeout(Duration::from_secs(graceful_shutdown_timeout_secs), server).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => tracing::error!("server error: {err}"),
        Err(_) => tracing::warn!("graceful shutdown timed out, forcing exit"),
    }
}

fn ensure_env_file() -> io::Result<()> {
    if !Path::new(".env").exists() {
        let proxy_api_key = uuid::Uuid::new_v4().simple().to_string();
        fs::write(".env", format!("PROXY_API_KEY={proxy_api_key}\n"))?;
        println!("Created .env with generated PROXY_API_KEY");
    }
    Ok(())
}

fn add_credential_interactive() -> io::Result<()> {
    print!("Provider name (e.g. openai): ");
    io::stdout().flush()?;
    let mut provider = String::new();
    io::stdin().read_line(&mut provider)?;
    let provider = provider.trim().to_ascii_uppercase();

    print!("API key: ");
    io::stdout().flush()?;
    let mut key = String::new();
    io::stdin().read_line(&mut key)?;
    let key = key.trim();

    let mut env_file = OpenOptions::new().create(true).append(true).open(".env")?;
    writeln!(env_file, "{provider}_API_KEY={key}")?;
    println!("Added credential for provider {provider} to .env");
    Ok(())
}

async fn shutdown_signal() {
    if let Err(err) = tokio::signal::ctrl_c().await {
        tracing::error!("failed to listen for shutdown signal: {err}");
    }
}
