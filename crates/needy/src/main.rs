mod cli;
mod client;
mod daemon;
mod docker;
mod intercept;
mod model;
mod process;
mod runner;
mod store;
mod tunnel;
mod wire;
mod workspace;

fn main() -> std::process::ExitCode {
    use clap::Parser;
    let cli = cli::Cli::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("create Tokio runtime");
    let result = runtime.block_on(cli::run(cli));
    // Stdin uses a blocking reader. A completed remote process must not wait
    // for the user's still-open terminal before the local command can exit.
    runtime.shutdown_background();
    match result {
        Ok(code) => std::process::ExitCode::from(code.clamp(0, 255) as u8),
        Err(error) => {
            eprintln!("needy: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
