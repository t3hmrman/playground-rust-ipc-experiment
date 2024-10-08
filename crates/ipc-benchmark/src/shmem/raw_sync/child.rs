//! Child-specific IPC implementation over `raw_sync`

use std::io::{stdin, BufRead};

use anyhow::{ensure, Context as _, Result};
use tracing::debug;
use uuid::Uuid;

use crate::shmem::raw_sync::{
    RawSyncInit, RawSyncInitResponse, ShmemHandle, DEFAULT_SHARED_MEM_RAW_SYNC_SLAB_SIZE_BYTES,
};
use crate::{get_system_time_millis, ChildProcess, PingMessage, PongMessage};

/// Parent proceses that uses shared memory as a communication mechanism
#[derive(Debug)]
pub struct RawSyncChild {
    /// UUID that should uniquely identify this process
    uuid: Uuid,
}

impl Default for RawSyncChild {
    fn default() -> Self {
        Self::new()
    }
}

impl RawSyncChild {
    /// Create a new [`RawSyncChild`]
    #[must_use]
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            uuid: Uuid::now_v7(),
        }
    }
}

impl ChildProcess for RawSyncChild {
    fn id(&self) -> String {
        self.uuid.to_string()
    }

    fn run(self) -> Result<()> {
        debug!("child process running");

        debug!("reading shmem raw_sync init from STDIN");
        let mut stdin = stdin().lock();
        let mut s = String::new();
        stdin.read_line(&mut s)?;

        // We expect to receive an init message on STDIN
        let RawSyncInit { write_handle } = serde_json::from_slice(s.as_bytes())
            .context("failed to read init message from STDIN")?;
        let mut write_handle = ShmemHandle::from_serialized(write_handle)?;
        debug!(?write_handle, "received raw sync init");

        debug!("setting child signal to clear");
        write_handle.clear_write_signal()?;

        // Create some shared memory to use for this side (parent will write to this)
        let shmem_size_bytes = std::env::var("SHARED_MEM_RAW_SYNC_SLAB_SIZE_BYTES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(DEFAULT_SHARED_MEM_RAW_SYNC_SLAB_SIZE_BYTES);
        debug!(shmem_size_bytes, "calculated shmem size");
        let mut parent_write_handle = ShmemHandle::new(shmem_size_bytes)?;

        // Write init response message
        debug!("sending init response bytes");
        write_handle
            .write_message(&RawSyncInitResponse {
                write_handle: parent_write_handle.to_serialized(),
                child_id: self.id(),
            })
            .context("failed to write init response from child")?;

        // Enter reading/writing loop
        debug!("entering read loop...");
        loop {
            // Wait for parent to write something
            debug!("waiting on message from parent");
            parent_write_handle.wait_for_write_signal()?;

            // Read an incoming ping message
            debug!("reading ping response from parent");
            let PingMessage {
                sender_id,
                receiver_id,
                ..
            } = parent_write_handle.read_message()?;
            ensure!(receiver_id == self.id(), "receiver should be child");

            // Write message to parent
            write_handle
                .write_message(&PongMessage {
                    sender_id: self.id(),
                    receiver_id: sender_id,
                    sent_at_ms: get_system_time_millis()?,
                })
                .context("failed to serialize pong message")?;
        }
    }
}
