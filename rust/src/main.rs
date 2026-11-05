#[tokio::main]
async fn main() -> anyhow::Result<()> {
    vibemk::cli::run(std::env::args_os()).await
}
