use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use oxicale_storage::{Store, StoreOptions};
use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::EnvFilter;

use crate::auth::upsert_password_file;
use crate::config::{AuthBackend, Config};

#[derive(Parser, Debug)]
#[command(
    name = "oxicale",
    version,
    about = "A small self-hosted CalDAV/CardDAV server"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Validate the effective configuration and exit.
    #[arg(long)]
    check_config: bool,
    /// Verify the storage data directory and exit.
    #[arg(long)]
    verify_storage: bool,
    /// Rebuild the derived index and exit.
    #[arg(long)]
    rebuild_index: bool,
    /// Data directory for the flag-only commands.
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Configuration file.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Import a Radicale data directory (requires --from and --to).
    #[arg(long)]
    migrate_radicale: bool,
    /// Source directory for --migrate-radicale.
    #[arg(long)]
    from: Option<PathBuf>,
    /// Target data directory for --migrate-radicale.
    #[arg(long)]
    to: Option<PathBuf>,
    /// Do not write changes when used with --migrate-radicale.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the HTTP server.
    Run {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long = "server-bind")]
        server_bind: Vec<String>,
        #[arg(long = "storage-data-dir")]
        storage_data_dir: Option<PathBuf>,
        #[arg(long = "auth-backend")]
        auth_backend: Option<String>,
        #[arg(long = "log-level")]
        log_level: Option<String>,
    },
    /// Validate configuration and print the effective values.
    CheckConfig {
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Verify item files, metadata and index consistency.
    VerifyStorage {
        #[arg(long)]
        data_dir: Option<PathBuf>,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Rebuild the SQLite index from canonical files.
    RebuildIndex {
        #[arg(long)]
        data_dir: Option<PathBuf>,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Manage local users.
    User {
        #[command(subcommand)]
        command: UserCommand,
    },
    /// Migration commands.
    Migrate {
        #[command(subcommand)]
        command: MigrateCommand,
    },
    /// Copy a data directory to a backup location.
    Export {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
}

#[derive(Subcommand, Debug)]
enum UserCommand {
    /// Add a user or replace their password.
    Add {
        username: String,
        #[arg(long, default_value = "/etc/oxicale/users")]
        password_file: PathBuf,
        #[arg(long)]
        password: Option<String>,
    },
    /// Change a user's password.
    Passwd {
        username: String,
        #[arg(long, default_value = "/etc/oxicale/users")]
        password_file: PathBuf,
        #[arg(long)]
        password: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum MigrateCommand {
    /// Import a Radicale collection-root directory.
    Radicale {
        #[arg(long)]
        from: PathBuf,
        #[arg(long)]
        to: PathBuf,
        #[arg(long)]
        dry_run: bool,
    },
}

pub fn run() -> Result<(), String> {
    let cli = Cli::parse();
    if cli.check_config {
        return run_check_config(cli.config.as_deref());
    }
    if cli.verify_storage {
        return run_verify_storage(cli.data_dir.as_deref(), cli.config.as_deref());
    }
    if cli.rebuild_index {
        return run_rebuild_index(cli.data_dir.as_deref(), cli.config.as_deref());
    }
    if cli.migrate_radicale {
        let from = cli
            .from
            .as_deref()
            .ok_or_else(|| "--migrate-radicale requires --from".to_string())?;
        let to = cli
            .to
            .as_deref()
            .ok_or_else(|| "--migrate-radicale requires --to".to_string())?;
        let report = crate::import::migrate(from, to, cli.dry_run)?;
        print!("{}", report.render());
        return Ok(());
    }
    let Some(command) = cli.command else {
        return Err("no command given; try --help".to_string());
    };
    match command {
        Command::Run {
            config,
            server_bind,
            storage_data_dir,
            auth_backend,
            log_level,
        } => run_server(
            config.as_deref(),
            server_bind,
            storage_data_dir,
            auth_backend,
            log_level,
        ),
        Command::CheckConfig { config } => run_check_config(config.as_deref()),
        Command::VerifyStorage { data_dir, config } => {
            run_verify_storage(data_dir.as_deref(), config.as_deref())
        }
        Command::RebuildIndex { data_dir, config } => {
            run_rebuild_index(data_dir.as_deref(), config.as_deref())
        }
        Command::User { command } => match command {
            UserCommand::Add {
                username,
                password_file,
                password,
            }
            | UserCommand::Passwd {
                username,
                password_file,
                password,
            } => {
                let password = match password {
                    Some(password) => password,
                    None => read_password()?,
                };
                upsert_password_file(&password_file, &username, &password)
                    .map_err(|error| error.to_string())?;
                println!("password updated for {username}");
                Ok(())
            }
        },
        Command::Migrate { command } => match command {
            MigrateCommand::Radicale { from, to, dry_run } => {
                let report = crate::import::migrate(&from, &to, dry_run)?;
                print!("{}", report.render());
                Ok(())
            }
        },
        Command::Export { data_dir, output } => export(&data_dir, &output),
    }
}

fn load(config: Option<&Path>) -> Result<Config, String> {
    let loaded = Config::load(config)?;
    for warning in &loaded.warnings {
        eprintln!("warning: {warning}");
    }
    Ok(loaded.config)
}

fn run_check_config(config: Option<&Path>) -> Result<(), String> {
    let config = load(config)?;
    let text = toml::to_string_pretty(&config)
        .map_err(|error| format!("cannot render configuration: {error}"))?;
    print!("{text}");
    Ok(())
}

fn data_dir_from(config: Option<&Path>, explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(explicit) = explicit {
        return Ok(explicit.to_path_buf());
    }
    Ok(load(config)?.storage.data_dir)
}

fn run_verify_storage(data_dir: Option<&Path>, config: Option<&Path>) -> Result<(), String> {
    let data_dir = data_dir_from(config, data_dir)?;
    let store = Store::open_with_options(&data_dir, StoreOptions::default())
        .map_err(|error| error.to_string())?;
    let report = store.verify_storage().map_err(|error| error.to_string())?;
    println!(
        "principals={} collections={} items={}",
        report.principals, report.collections, report.items
    );
    if report.errors.is_empty() {
        println!("storage verified");
        Ok(())
    } else {
        for error in &report.errors {
            eprintln!("error: {error}");
        }
        Err("storage verification failed".to_string())
    }
}

fn run_rebuild_index(data_dir: Option<&Path>, config: Option<&Path>) -> Result<(), String> {
    let data_dir = data_dir_from(config, data_dir)?;
    let store = Store::open_with_options(&data_dir, StoreOptions::default())
        .map_err(|error| error.to_string())?;
    let report = store.rebuild_index().map_err(|error| error.to_string())?;
    println!(
        "rebuilt index: principals={} collections={} items={}",
        report.principals, report.collections, report.items
    );
    for error in &report.errors {
        eprintln!("warning: {error}");
    }
    Ok(())
}

fn run_server(
    config_path: Option<&Path>,
    server_bind: Vec<String>,
    storage_data_dir: Option<PathBuf>,
    auth_backend: Option<String>,
    log_level: Option<String>,
) -> Result<(), String> {
    let mut config = load(config_path)?;
    if !server_bind.is_empty() {
        config.server.bind = server_bind;
    }
    if let Some(data_dir) = storage_data_dir {
        config.storage.data_dir = data_dir;
    }
    if let Some(backend) = auth_backend {
        config.auth.backend = match backend.as_str() {
            "local" => AuthBackend::Local,
            "trusted_header" => AuthBackend::TrustedHeader,
            "none" => AuthBackend::None,
            "deny" => AuthBackend::Deny,
            other => return Err(format!("unknown auth backend {other:?}")),
        };
    }
    if let Some(level) = log_level {
        config.logging.level = level;
    }
    config.validate()?;
    init_logging(&config)?;
    let runtime = tokio::runtime::Runtime::new().map_err(|error| error.to_string())?;
    runtime.block_on(crate::server::run(config))
}

fn init_logging(config: &Config) -> Result<(), String> {
    let filter =
        EnvFilter::try_new(&config.logging.level).unwrap_or_else(|_| EnvFilter::new("info"));
    let writer = match config.logging.target {
        crate::config::LogTarget::Stdout => BoxMakeWriter::new(std::io::stdout),
        crate::config::LogTarget::Stderr => BoxMakeWriter::new(std::io::stderr),
        crate::config::LogTarget::File => {
            let path = config.logging.file.clone();
            if path.is_empty() {
                return Err("logging.file is empty".to_string());
            }
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|error| format!("cannot open log file {path}: {error}"))?;
            BoxMakeWriter::new(move || {
                file.try_clone()
                    .unwrap_or_else(|_| std::fs::File::create("/dev/null").unwrap())
            })
        }
    };
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false);
    match config.logging.format {
        crate::config::LogFormat::Json => builder.json().try_init(),
        crate::config::LogFormat::Text => builder.try_init(),
    }
    .map_err(|error| format!("cannot initialise logging: {error}"))
}

fn read_password() -> Result<String, String> {
    eprint!("New password: ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    let password = line.trim_end_matches(['\r', '\n']).to_string();
    if password.is_empty() {
        return Err("password must not be empty".to_string());
    }
    Ok(password)
}

fn export(data_dir: &Path, output: &Path) -> Result<(), String> {
    if !data_dir.is_dir() {
        return Err(format!(
            "data directory {} does not exist",
            data_dir.display()
        ));
    }
    std::fs::create_dir_all(output).map_err(|error| error.to_string())?;
    copy_dir(data_dir, output)?;
    println!("exported {} to {}", data_dir.display(), output.display());
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(from).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        let target = to.join(entry.file_name());
        if path.is_dir() {
            std::fs::create_dir_all(&target).map_err(|error| error.to_string())?;
            copy_dir(&path, &target)?;
        } else {
            std::fs::copy(&path, &target).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}
