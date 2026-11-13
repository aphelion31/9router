mod api;
mod config;
mod dashboard;
mod db;
mod executors;
mod handlers;
mod oauth;
mod providers;
mod routing;
mod server;
mod translator;

const USAGE: &str = "\
usage: ninerouter [serve] [--port N] [--data-dir P] [--host H]
       ninerouter --version
       ninerouter --help

Flags override PORT, DATA_DIR and HOST. Defaults: port 20127, host 127.0.0.1.";

/// Parsed command line. Flags are applied to the environment before startup.
#[derive(Debug, PartialEq)]
enum Command {
    Serve,
    Version,
    Help,
}

/// Parses `args` (without the program name) and sets `PORT`, `DATA_DIR` and `HOST` from flags.
fn parse_args(args: &[String]) -> Result<Command, String> {
    let mut command = Command::Serve;
    let mut iter = args.iter();
    let mut first = true;
    while let Some(arg) = iter.next() {
        let value = |name: &str, it: &mut std::slice::Iter<String>| {
            it.next().cloned().ok_or_else(|| format!("{name} needs a value"))
        };
        match arg.as_str() {
            "serve" if first => {}
            "--version" | "-V" => return Ok(Command::Version),
            "--help" | "-h" => return Ok(Command::Help),
            "--port" => unsafe_set("PORT", &value("--port", &mut iter)?)?,
            "--data-dir" => unsafe_set("DATA_DIR", &value("--data-dir", &mut iter)?)?,
            "--host" => unsafe_set("HOST", &value("--host", &mut iter)?)?,
            other => return Err(format!("unknown argument: {other}")),
        }
        first = false;
    }
    Ok(command)
}

/// Sets an env var before any threads start (called from `main` prior to the runtime
/// doing work). Validates the port before it is stored.
fn unsafe_set(key: &str, value: &str) -> Result<(), String> {
    if key == "PORT" && value.parse::<u16>().is_err() {
        return Err(format!("invalid port: {value}"));
    }
    // SAFETY: only called from `main` before any other thread is spawned.
    unsafe { std::env::set_var(key, value) };
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse_args(&args) {
        Ok(Command::Version) => {
            println!("ninerouter {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Ok(Command::Help) => {
            println!("{USAGE}");
            Ok(())
        }
        Ok(Command::Serve) => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(serve()),
        Err(message) => {
            eprintln!("ninerouter: {message}\n{USAGE}");
            std::process::exit(2);
        }
    }
}

async fn serve() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ninerouter=info".into()),
        )
        .init();

    let _conn = db::open(&db::data_file()?)?;

    let listener = tokio::net::TcpListener::bind((config::host().as_str(), config::port())).await?;
    tracing::info!("listening on {}", listener.local_addr()?);
    axum::serve(listener, server::router()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_commands_and_rejects_unknown() {
        assert_eq!(parse_args(&args(&[])), Ok(Command::Serve));
        assert_eq!(parse_args(&args(&["serve"])), Ok(Command::Serve));
        assert_eq!(parse_args(&args(&["--version"])), Ok(Command::Version));
        assert_eq!(parse_args(&args(&["-h"])), Ok(Command::Help));
        assert!(parse_args(&args(&["--bogus"])).is_err());
        assert!(parse_args(&args(&["--port"])).is_err());
        assert!(parse_args(&args(&["--port", "abc"])).is_err());
    }

    #[test]
    fn host_defaults_to_loopback() {
        // SAFETY: test-only env mutation; this test does not run concurrently with another reader of HOST.
        unsafe { std::env::remove_var("HOST") };
        assert_eq!(config::host(), "127.0.0.1");
    }
}
