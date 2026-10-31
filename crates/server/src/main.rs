//! The `coffer` binary: a thin shell around [`coffer_server::cli`].

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    coffer_server::cli::run(
        std::env::args_os(),
        |name| std::env::var(name).ok(),
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
    .await
}
