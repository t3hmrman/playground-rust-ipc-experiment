use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{bail, Context as _, Result};
use conv::ValueFrom as _;
use tracing::{debug, info};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

use ipc_benchmark::shmem::raw_sync::RawSyncParent;
use ipc_benchmark::{ParentProcess, Pinger};

const DEFAULT_TEST_DURATION_SECONDS: u64 = 10;

fn main() -> Result<()> {
    tracing_subscriber::Registry::default()
        .with(EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer())
        .try_init()
        .context("failed to build tracing")?;

    debug!("creating parent child...");
    let mut parent = RawSyncParent::new();

    let child_name = "child-1";

    debug!("resolving bin path...");
    let bin_path = std::env::var("RAW_SYNC_CHILD_BIN_PATH")
        .map(PathBuf::from)
        .context("missing env var RAW_SYNC_CHILD_BIN_PATH")?;
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

    let test_duration_seconds = std::env::var("TEST_DURATION_SECONDS")
        .context("missing env var")
        .and_then(|v| v.parse::<u64>().context("failed to parse"))
        .unwrap_or(DEFAULT_TEST_DURATION_SECONDS);
    let test_duration = Duration::from_secs(test_duration_seconds);

    // NOTE: we can't spawn this into another thread, because the Shmem values *cannot* be moved over
    // (it *might* be possible, but at least isn't implemented now)
    let start = Instant::now();
    debug!("starting loop of pings to child process (child is NOT threaded)");
    let mut invocations: u64 = 0;
    let roundtrips = loop {
        parent
            .roundtrip_ping(child_name)
            .context("failed to ping")?;
        invocations += 1;
        // Break if we're over


        if Instant::now().duration_since(start) > test_duration {
            break Ok(invocations) as Result<u64, anyhow::Error>;
        }
    }?;

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
