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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ninerouter=info".into()),
        )
        .init();

    let _conn = db::open(&db::data_file()?)?;

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", config::port())).await?;
    tracing::info!("listening on {}", listener.local_addr()?);
    axum::serve(listener, server::router()).await?;
    Ok(())
}
