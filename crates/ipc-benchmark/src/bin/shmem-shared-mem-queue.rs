use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context as _, Result};
use conv::ValueFrom as _;
use tracing::{debug, info};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

use ipc_benchmark::shmem::shared_mem_queue::SharedMemQueueParent;
use ipc_benchmark::{ParentProcess, Pinger};

const DEFAULT_TEST_DURATION_SECONDS: u64 = 10;

fn main() -> Result<()> {
    tracing_subscriber::Registry::default()
        .with(EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer())
        .try_init()
        .context("failed to build tracing")?;

    debug!("creating parent child...");
    let mut parent = SharedMemQueueParent::new();

    let child_name = "child-1";

    debug!("resolving bin path...");
    let bin_path = std::env::var("SHARED_MEM_QUEUE_CHILD_BIN_PATH")
        .map(PathBuf::from)
        .context("missing env var SHARED_MEM_QUEUE_CHILD_BIN_PATH")?;
    if !bin_path.exists() {
        bail!("missing binary at path [{}]", bin_path.display());
    }
    if !bin_path.metadata().is_ok_and(|m| m.is_file()) {
        bail!("invalid non-binary file at path [{}]", bin_path.display());
    }

    debug!("spawning child...");
    let mut child_process = parent
        .spawn_child(child_name, Command::new(bin_path))
        .context("failed to spawn child")?;

    let stop = Arc::new(AtomicUsize::new(0));
    let thread_stop = stop.clone();

    debug!("starting thread to send pings to child process");
    let ping_thread = std::thread::spawn(move || {
        let mut invocations: u64 = 0;
        loop {
            parent
                .roundtrip_ping(child_name)
                .context("failed to ping")?;
            invocations += 1;
            if thread_stop.load(Ordering::Relaxed) == 1 {
                return Ok(invocations) as Result<u64, anyhow::Error>;
            }
        }
    });

    let test_duration_seconds = std::env::var("TEST_DURATION_SECONDS")
        .context("missing env var")
        .and_then(|v| v.parse::<u64>().context("failed to parse"))
        .unwrap_or(DEFAULT_TEST_DURATION_SECONDS);
    debug!("waiting {test_duration_seconds} seconds in main thread...");
    std::thread::sleep(std::time::Duration::from_secs(test_duration_seconds));

    debug!("stopping sender thread...");
    stop.store(1, Ordering::Relaxed);
    let roundtrips = ping_thread
        .join()
        .map_err(|_| anyhow!("failed to join pinger thread"))?
        .context("failed to calculate invocations")?;

    debug!("killing child process...");
    child_process
        .kill()
        .context("failed to kill child process")?;

    let roundtrips_per_second = f64::value_from(roundtrips)
        .context("failed to convert roundtrips to f64")?
        / f64::value_from(test_duration_seconds)
            .context("failed to convert test duration to f64")?;

    info!(
        roundtrips,
        test_duration_seconds, roundtrips_per_second, "completed ping-pong round-trips"
    );
    eprintln!("completed [{roundtrips}] ping-pong round-trips [{test_duration_seconds}] seconds ([{roundtrips_per_second}] round-trips/second)");
    Ok(())
}
