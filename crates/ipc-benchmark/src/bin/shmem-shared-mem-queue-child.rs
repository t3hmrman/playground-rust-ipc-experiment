use anyhow::{Context as _, Result};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

use ipc_benchmark::shmem::shared_mem_queue::SharedMemQueueChild;
use ipc_benchmark::ChildProcess as _;

fn main() -> Result<()> {
    tracing_subscriber::Registry::default()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer())
        .try_init()
        .context("failed to build tracing")?;

    SharedMemQueueChild::new().run()
}
