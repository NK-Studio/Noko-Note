//! 웹 버전: 브라우저에서 http://127.0.0.1:3000 으로 쓴다.

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt().with_target(false).init();
    noko_note::install_whisper_logging();

    let cfg = noko_note::Config::from_env()?;
    let _noko = noko_note::Noko::start(cfg).await?;
    tokio::signal::ctrl_c().await?;
    Ok(())
}
