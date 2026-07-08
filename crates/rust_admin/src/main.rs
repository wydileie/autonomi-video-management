#[tokio::main]
async fn main() -> anyhow::Result<()> {
    rust_admin::run().await
}
