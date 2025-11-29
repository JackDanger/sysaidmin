mod allowlist;
mod api;
mod app;
mod config;
mod conversation;
mod executor;
mod hooks;
mod logger;
mod models;
mod parser;
mod session;
mod task;
mod tokenizer;
mod transcript;
mod tui;

use std::panic;
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use log::{error, info, warn, debug, trace};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Explicitly set the Anthropic model (skips interactive selection)
    #[arg(long)]
    model: Option<String>,
}

static PANIC_OCCURRED: AtomicBool = AtomicBool::new(false);

fn setup_panic_handler() {
    panic::set_hook(Box::new(|panic_info| {
        PANIC_OCCURRED.store(true, Ordering::SeqCst);
        
        let location = panic_info.location()
            .map(|loc| format!("{}:{}:{}", loc.file(), loc.line(), loc.column()))
            .unwrap_or_else(|| "unknown location".to_string());
        
        let message = panic_info.payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| {
                panic_info.payload()
                    .downcast_ref::<String>()
                    .map(|s| s.clone())
            })
            .unwrap_or_else(|| "unknown panic".to_string());
        
        error!("PANIC OCCURRED at {}: {}", location, message);
        
        let backtrace = std::backtrace::Backtrace::capture();
        error!("Backtrace:\n{}", backtrace);
        
        eprintln!("\n=== PANIC DETECTED ===");
        eprintln!("Location: {}", location);
        eprintln!("Message: {}", message);
        eprintln!("Check sysaidmin.log for full details");
        eprintln!("=====================\n");
    }));
}

#[cfg(unix)]
fn setup_signal_handlers() {
    use signal_hook::consts::signal::*;
    use signal_hook::flag;
    
    // Only register signals that are safe to register.
    // SIGSEGV, SIGBUS, SIGILL, SIGFPE are forbidden and handled by the OS.
    // SIGABRT is also typically forbidden.
    // We can safely register SIGTERM and SIGINT for graceful shutdown.
    let signals = [SIGTERM, SIGINT];
    for sig in &signals {
        let signal_occurred = Arc::new(AtomicBool::new(false));
        let signal_occurred_clone = signal_occurred.clone();
        
        match flag::register(*sig, signal_occurred_clone) {
            Ok(_) => {
                info!("Registered handler for signal {} ({})", sig, 
                      if *sig == SIGTERM { "SIGTERM" } else { "SIGINT" });
            }
            Err(e) => {
                warn!("Failed to register handler for signal {}: {}", sig, e);
            }
        }
        
        // Spawn a thread to check for signal and log
        let signal_occurred_check = signal_occurred.clone();
        let sig_num = *sig;
        let sig_name = if sig_num == SIGTERM { "SIGTERM" } else { "SIGINT" };
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if signal_occurred_check.load(Ordering::SeqCst) {
                info!("Signal {} ({}) received - initiating graceful shutdown", sig_num, sig_name);
                let backtrace = std::backtrace::Backtrace::capture();
                debug!("Backtrace at signal:\n{}", backtrace);
            }
        });
    }
    
    // Note: For crashes (SIGSEGV, SIGBUS, SIGILL, SIGFPE), the panic handler
    // will catch them if they cause panics, but the OS handles the actual signals.
    info!("Signal handlers registered (SIGTERM, SIGINT). Crashes will be logged via panic handler.");
}

#[cfg(not(unix))]
fn setup_signal_handlers() {
    // Signal handling not available on non-Unix platforms
    warn!("Signal handlers not available on this platform");
}

fn main() {
    // Initialize logging first, before anything else
    let log_path = env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("sysaidmin.log");
    
    if let Err(e) = logger::FileLogger::init(log_path.clone()) {
        eprintln!("CRITICAL: Failed to initialize logger: {}", e);
        eprintln!("Attempting to continue without file logging...");
    }
    
    info!("=== SYSAIDMIN STARTING ===");
    info!("Log file: {}", log_path.display());
    info!("PID: {}", process::id());
    info!("Working directory: {:?}", env::current_dir());
    info!("Command line args: {:?}", env::args().collect::<Vec<_>>());
    
    // Set up panic handler
    setup_panic_handler();
    info!("Panic handler installed");
    
    // Set up signal handlers
    setup_signal_handlers();
    info!("Signal handlers installed");
    
    // Run main logic with comprehensive error handling
    let result = std::panic::catch_unwind(|| {
        match run_main() {
            Ok(()) => {
                info!("=== SYSAIDMIN EXITING NORMALLY ===");
                Ok(())
            }
            Err(e) => {
                error!("=== SYSAIDMIN EXITING WITH ERROR ===");
                error!("Error: {:?}", e);
                error!("Error chain:");
                let mut source = e.source();
                let mut depth = 0;
                while let Some(err) = source {
                    error!("  [{}] {}", depth, err);
                    source = err.source();
                    depth += 1;
                }
                Err(e)
            }
        }
    });
    
    match result {
        Ok(Ok(())) => {
            info!("Main function completed successfully");
            process::exit(0);
        }
        Ok(Err(e)) => {
            error!("Main function returned error: {:?}", e);
            process::exit(1);
        }
        Err(panic_payload) => {
            error!("Main function panicked (this should have been caught by panic handler)");
            if let Some(s) = panic_payload.downcast_ref::<&str>() {
                error!("Panic message: {}", s);
            } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                error!("Panic message: {}", s);
            } else {
                error!("Panic payload: {:?}", panic_payload);
            }
            process::exit(101);
        }
    }
}

fn run_main() -> Result<()> {
    trace!("Parsing command line arguments");
    let cli = Cli::parse();
    debug!("CLI args parsed: model={:?}", cli.model);
    
    trace!("Loading configuration");
    let mut config = config::AppConfig::load()
        .context("Failed to load application configuration")?;
    info!("Configuration loaded successfully");
    debug!("Config: dry_run={}, offline_mode={}, model={}", 
           config.dry_run, config.offline_mode, config.model);
    
    trace!("Selecting model");
    let selected_model = models::select_model(&config, cli.model)
        .context("Failed to select model")?;
    config.model = selected_model;
    info!("Model selected: {}", config.model);
    
    trace!("Initializing allowlist");
    let allowlist_cfg = config.allowlist.clone();
    let allowlist = allowlist::Allowlist::from_config(allowlist_cfg)
        .context("Failed to initialize allowlist")?;
    info!("Allowlist initialized");
    
    trace!("Creating API client");
    let client = api::AnthropicClient::new(&config)
        .context("Failed to create API client")?;
    info!("API client created (offline_mode={})", config.offline_mode);
    
    trace!("Creating executor");
    let executor = executor::Executor::new(config.dry_run);
    info!("Executor created (dry_run={})", config.dry_run);
    
    trace!("Creating session store");
    let session = session::SessionStore::new(config.session_root.clone())
        .context("Failed to create session store")?;
    info!("Session store created at: {}", config.session_root.display());
    
    trace!("Creating application instance");
    let mut app = app::App::new(config, client, allowlist, executor, session);
    info!("Application instance created");
    
    trace!("Starting TUI");
    tui::run(&mut app)
        .context("TUI exited with error")?;
    
    info!("TUI completed successfully");
    Ok(())
}
