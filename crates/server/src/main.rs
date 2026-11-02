//! The `coffer` binary: a thin shell around [`coffer_server::cli`].

use std::process::ExitCode;

fn main() -> ExitCode {
    // First, so that it covers building the runtime too.
    coffer_server::cli::install_panic_hook();
    run()
}

#[tokio::main]
async fn run() -> ExitCode {
    coffer_server::cli::run(
        std::env::args_os(),
        |name| std::env::var(name).ok(),
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
    .await
}
