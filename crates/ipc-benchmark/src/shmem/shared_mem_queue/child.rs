//! Child-specific IPC implementation over `mmap`-backed files (using [`shared_mem_queue`])

use std::io::BufRead;
use std::{fs::OpenOptions, io::stdin};

use anyhow::{ensure, Context as _, Result};
use shared_mem_queue::SharedMemQueue;
use tracing::debug;
use uuid::Uuid;

use crate::shmem::shared_mem_queue::{
    SharedMemQueueHandle, SharedMemQueueInit, SharedMemQueueInitResponse,
};
use crate::{get_system_time_millis, ChildProcess, PingMessage, PongMessage};

/// A child process that performs IPC via shared memory, in particular using [`shared_mem_queue`]
#[derive(Debug)]
pub struct SharedMemQueueChild {
    /// UUID that should uniquely identify this process
    uuid: Uuid,
}

impl Default for SharedMemQueueChild {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedMemQueueChild {
    /// Build a new [`SharedMemQueueChild`] with a random UUID
    #[must_use]
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            uuid: Uuid::now_v7(),
        }
    }
}

impl ChildProcess for SharedMemQueueChild {
    fn id(&self) -> String {
        self.uuid.to_string()
    }

    /// Execute as the running process.
    ///
    /// This command is expected to never return, as child processes
    /// should handle messages forever.
    fn run(self) -> Result<()> {
        debug!("child process running");

        debug!("reading shmem queue init from STDIN");
        let mut stdin = stdin().lock();
        let mut s = String::new();
        stdin.read_line(&mut s)?;

        // We expect to receive an init message on STDIN
        let SharedMemQueueInit {
            parent_id,
            parent_region,
            child_region,
        } = serde_json::from_slice(s.as_bytes())
            .context("failed to read init message from STDIN")?;

        let to_parent_region = parent_region
            .context("parent didn't provide region information, which is currently unsupported")?;

        debug!(
            parent_shared_region_file_path = %to_parent_region.file_path.display(),
            "building parent mmap file",
        );
        let to_parent_mmap_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&to_parent_region.file_path)
            .with_context(|| {
                format!(
                    "failed to create new file for reading from parent @ [{}]",
                    to_parent_region.file_path.display()
                )
            })?;
        let mut to_parent_mmap = unsafe {
            memmap::MmapOptions::new()
                .offset(to_parent_region.offset)
                .len(to_parent_region.len)
                .map_mut(&to_parent_mmap_file)
                .context("failed to create parent read mmap")?
        };

        // Create the queues for both parent and child
        debug!("building parent shared mem queue");
        let mut to_parent =
            unsafe { SharedMemQueue::attach(to_parent_mmap.as_mut_ptr(), to_parent_region.len) };

        // NOTE: before sending the init response back we *must* create the
        // as the mmap that the child will write to, and the parent will read from
        debug!("building child mmap");
        let from_parent_mmap_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&child_region.file_path)
            .with_context(|| {
                format!(
                    "failed to create new file for reading from child @ [{}]",
                    child_region.file_path.display()
                )
            })?;
        let mut from_parent_mmap = unsafe {
            memmap::MmapOptions::new()
                .len(child_region.len)
                .offset(child_region.offset)
                .map_mut(&from_parent_mmap_file)
                .context("failed to create child read mmap")?
        };

        debug!("building shared mem queue for messages received from parent");
        let mut from_parent =
            unsafe { SharedMemQueue::create(from_parent_mmap.as_mut_ptr(), child_region.len) };

        let mut to_parent_handle =
            SharedMemQueueHandle::<SharedMemQueueInitResponse>::new(&mut to_parent);
        to_parent_handle
            .blocking_write(&SharedMemQueueInitResponse {
                parent_id: parent_id.clone(),
                child_id: self.id(),
            })
            .context("failed to write init response to parent")?;
        debug!("successfully wrote init response to parent");

        // From here, we expect to only send pongs, so we'll reuse the handle for a different type
        let mut to_parent_handle: SharedMemQueueHandle<PongMessage> = to_parent_handle.into_other();

        // Enter reading/writing loop
        debug!("entering read loop...");
        loop {
            debug!("attempting to read ping");
            let mut reader = SharedMemQueueHandle::<PingMessage>::new(&mut from_parent);
            let PingMessage {
                sender_id,
                receiver_id,
                ..
            } = reader
                .blocking_read()
                .context("failed to deserialize ping message")?;
            ensure!(sender_id == parent_id, "sender should be parent");
            ensure!(receiver_id == self.id(), "receiver should be child");
            debug!(parent_id, "successfully received ping from parent");

            // Build & write pong back to the parent
            to_parent_handle
                .blocking_write(&PongMessage {
                    sender_id: self.id(),
                    receiver_id: parent_id.clone(),
                    sent_at_ms: get_system_time_millis()?,
                })
                .context("failed to send pong to parent")?;
        }
    }
}
