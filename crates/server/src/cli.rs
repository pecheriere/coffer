//! The `coffer` command line: `serve` and the host operator's commands.
//!
//! [`run`] takes its arguments, environment and output streams as
//! parameters so the commands can be driven in-process by tests.

use std::ffi::OsString;
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use coffer_domain::identity::Username;
use coffer_domain::time::SystemClock;
use coffer_store::{Store, StoreError};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use crate::appearance::Appearance;
use crate::auth::{AgentTokens, AuthError, Identity, Sessions};
use crate::config::{Config, ConfigError};
use crate::http::{self, AppState};
use crate::secrets::{SecretError, ServerKey};

mod healthcheck;

const EXIT_FAILURE: u8 = 1;
const EXIT_USAGE: u8 = 2;

#[derive(Debug, Parser)]
#[command(name = "coffer", version, about = "Self-hosted household finance hub")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(flatten)]
    Configured(ConfiguredCommand),
    /// Check the running server: exit 0 if healthy, 1 otherwise.
    ///
    /// Sends `GET /healthz` to `COFFER_LISTEN` (loopback when it binds every
    /// interface) and expects 200 within 5 seconds. Reads no other
    /// configuration, so it works without access to the data directory.
    Healthcheck,
}

/// The commands that read the whole configuration and use the data
/// directory.
#[derive(Debug, Subcommand)]
enum ConfiguredCommand {
    /// Run the server.
    Serve,
    /// Print a single-use link that creates the first admin.
    BootstrapLink,
    /// Print a single-use link that lets an admin who lost every passkey
    /// register a new one.
    RecoverAdmin {
        /// Username of the admin.
        username: String,
    },
    /// Write a consistent copy of the database to a new file (mode 0600).
    Backup {
        /// Where to write the copy; it must not exist yet.
        dest: PathBuf,
    },
}

/// Why a command failed, and with which exit code.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("{0}")]
    Config(#[from] ConfigError),
    #[error("{0}")]
    Failed(String),
}

impl CliError {
    fn failed(err: &(dyn std::error::Error + 'static)) -> Self {
        let mut message = err.to_string();
        let mut source = err.source();
        while let Some(cause) = source {
            message.push_str(": ");
            message.push_str(&cause.to_string());
            source = cause.source();
        }
        Self::Failed(message)
    }

    fn exit_code(&self) -> ExitCode {
        match self {
            Self::Config(_) => ExitCode::from(EXIT_USAGE),
            Self::Failed(_) => ExitCode::from(EXIT_FAILURE),
        }
    }
}

macro_rules! from_error {
    ($($ty:ty),*) => {$(
        impl From<$ty> for CliError {
            fn from(err: $ty) -> Self {
                Self::failed(&err)
            }
        }
    )*};
}
from_error!(StoreError, AuthError, SecretError, std::io::Error);

/// Runs `coffer` with `args` (the first is the program name) and the
/// environment seen through `env`. Results go to `out`, diagnostics to
/// `err`. Exit code 0 is success, 1 a failed command, 2 bad usage or
/// configuration.
pub async fn run(
    args: impl IntoIterator<Item = OsString>,
    env: impl Fn(&str) -> Option<String>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(usage) => {
            let text = usage.render().to_string();
            // Nothing sensible is left to do if the terminal is gone.
            let _ = if usage.use_stderr() {
                err.write_all(text.as_bytes())
            } else {
                out.write_all(text.as_bytes())
            };
            return u8::try_from(usage.exit_code())
                .map_or(ExitCode::from(EXIT_USAGE), ExitCode::from);
        }
    };
    let result = match cli.command {
        Command::Configured(command) => execute(command, &env, out).await,
        Command::Healthcheck => healthcheck(&env).await,
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            let _ = writeln!(err, "coffer: {error}");
            error.exit_code()
        }
    }
}

/// Probes the running server. Every failure, a bad `COFFER_LISTEN`
/// included, is exit code 1: container health checks reserve 2.
async fn healthcheck(env: &impl Fn(&str) -> Option<String>) -> Result<ExitCode, CliError> {
    let listen =
        Config::listen_from_env(env).map_err(|error| CliError::Failed(error.to_string()))?;
    healthcheck::probe(listen, healthcheck::TIMEOUT)
        .await
        .map_err(|error| CliError::failed(&error))?;
    Ok(ExitCode::SUCCESS)
}

async fn execute(
    command: ConfiguredCommand,
    env: &impl Fn(&str) -> Option<String>,
    out: &mut dyn Write,
) -> Result<ExitCode, CliError> {
    let config = Config::from_env(env)?;
    #[cfg(unix)]
    restrict_umask();
    match command {
        ConfiguredCommand::Serve => {
            init_tracing(env)?;
            serve(config).await?;
            Ok(ExitCode::SUCCESS)
        }
        ConfiguredCommand::BootstrapLink => {
            let state = open_state(config).await?;
            match state.identity.bootstrap_link().await? {
                Some(link) => {
                    writeln!(out, "{link}")?;
                    Ok(ExitCode::SUCCESS)
                }
                None => Err(CliError::Failed("an admin already exists".to_owned())),
            }
        }
        ConfiguredCommand::RecoverAdmin { username } => {
            let username = Username::parse(&username)
                .map_err(|_| CliError::Failed("no such admin".to_owned()))?;
            let state = open_state(config).await?;
            match state.identity.cli_admin_reenroll(&username).await {
                Ok(link) => {
                    writeln!(out, "{link}")?;
                    Ok(ExitCode::SUCCESS)
                }
                Err(AuthError::NotFound | AuthError::Denied(_)) => {
                    Err(CliError::Failed("no such admin".to_owned()))
                }
                Err(other) => Err(other.into()),
            }
        }
        ConfiguredCommand::Backup { dest } => {
            if config.data_dir.try_exists()? {
                check_private_dir(&config.data_dir)?;
            }
            // Never create or migrate a database just to back it up.
            if !config.database_path().try_exists()? {
                return Err(CliError::Failed(
                    "no database found in the data directory".to_owned(),
                ));
            }
            let store = Store::open(&config.database_path()).await?;
            backup(&store, &dest).await?;
            writeln!(out, "backup written to {}", dest.display())?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Opens the database, creating the data directory (mode 0700) if needed
/// and refusing one that group or others can access.
async fn open_store(config: &Config) -> Result<Store, CliError> {
    let mut dir = std::fs::DirBuilder::new();
    dir.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut dir, 0o700);
    dir.create(&config.data_dir)?;
    check_private_dir(&config.data_dir)?;
    Ok(Store::open(&config.database_path()).await?)
}

/// Refuses a data directory that group or others can access: it holds the
/// database and, by default, the server key.
fn check_private_dir(dir: &Path) -> Result<(), CliError> {
    #[cfg(unix)]
    {
        let mode = std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(dir)?.permissions());
        if mode & 0o077 != 0 {
            let shown = dir.display();
            return Err(CliError::Failed(format!(
                "data directory {shown} must not be accessible by group or others; \
                 run `chmod 700 {shown}`"
            )));
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// Makes every file this process creates readable by its owner only.
/// `SQLite` creates the database and its `-wal` and `-shm` files itself,
/// with mode 0644 less the umask and no way to ask for another, so the umask
/// is what keeps credential hashes and financial data private. Set before
/// any file is opened.
#[cfg(unix)]
fn restrict_umask() {
    rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
}

/// Opens the database and key and builds every service.
async fn open_state(config: Config) -> Result<AppState, CliError> {
    let store = open_store(&config).await?;
    let key = Arc::new(ServerKey::load_or_create(&config.secret_key_file)?);
    let clock = Arc::new(SystemClock);
    let identity = Arc::new(Identity::new(
        store.clone(),
        Arc::clone(&key),
        clock.clone(),
        config.clone(),
    )?);
    let sessions = Arc::new(Sessions::new(
        store.clone(),
        Arc::clone(&key),
        clock.clone(),
        config.clone(),
    ));
    let agent_tokens = Arc::new(AgentTokens::new(store.clone(), key, clock.clone()));
    let appearance = Arc::new(Appearance::new(store.clone(), clock.clone()));
    Ok(AppState {
        sessions,
        agent_tokens,
        identity,
        appearance,
        store,
        config,
        clock,
    })
}

/// Copies the database to `dest` with mode 0600 and without ever replacing
/// an existing file. The copy is staged in a private (0700) directory next
/// to `dest`, then copied into a `dest` created exclusively with mode 0600,
/// so it is never readable by others and needs no hard-link support.
async fn backup(store: &Store, dest: &Path) -> Result<(), CliError> {
    let exists = || CliError::Failed("backup destination already exists".to_owned());
    if dest.try_exists()? {
        return Err(exists());
    }
    let parent = match dest.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let staging = tempfile::Builder::new()
        .prefix(".coffer-backup-")
        .tempdir_in(parent)?;
    let staged = staging.path().join("backup.db");
    store.backup_to(&staged).await?;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut target = options.open(dest).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            exists()
        } else {
            error.into()
        }
    })?;
    let copied = std::fs::File::open(&staged)
        .and_then(|mut source| std::io::copy(&mut source, &mut target))
        .and_then(|_| target.sync_all());
    if let Err(error) = copied {
        drop(target);
        // A partial backup must not be mistaken for a good one.
        let _ = std::fs::remove_file(dest);
        return Err(error.into());
    }
    Ok(())
}

/// `RUST_LOG` filter (default `info`); JSON lines when `COFFER_LOG_FORMAT`
/// is `json`. Logs go to stderr.
fn init_tracing(env: &impl Fn(&str) -> Option<String>) -> Result<(), CliError> {
    let filter = env("RUST_LOG")
        .and_then(|spec| EnvFilter::try_new(spec).ok())
        .unwrap_or_else(|| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_writer(std::io::stderr);
    let installed = if env("COFFER_LOG_FORMAT").as_deref() == Some("json") {
        builder.json().try_init()
    } else {
        builder.try_init()
    };
    installed.map_err(|error| CliError::Failed(error.to_string()))
}

async fn serve(config: Config) -> Result<(), CliError> {
    let state = open_state(config.clone()).await?;
    let listener = TcpListener::bind(config.listen).await?;
    serve_with(state, listener, shutdown_signal()).await
}

/// Serves `state` on `listener` until `shutdown` resolves, then drains
/// in-flight requests and returns.
///
/// Logs the setup link when no admin exists. That is the only place a setup
/// link is logged: with no channel to the operator, and the link
/// single-use and short-lived, the log is it.
///
/// # Errors
///
/// When the setup link cannot be issued or the listener fails.
pub async fn serve_with(
    state: AppState,
    listener: TcpListener,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), CliError> {
    if let Some(link) = state.identity.bootstrap_link().await? {
        tracing::info!(%link, "no admin exists yet; open this single-use link to create the first admin");
    }
    let listen = listener.local_addr()?;
    tracing::info!(%listen, public_url = %state.config.public_url, "coffer is serving");
    axum::serve(listener, http::router(state))
        .with_graceful_shutdown(shutdown)
        .await?;
    tracing::info!("coffer stopped");
    Ok(())
}

/// Resolves on ctrl-c or SIGTERM.
async fn shutdown_signal() {
    let ctrl_c = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory as _;

    use super::*;

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }
}
