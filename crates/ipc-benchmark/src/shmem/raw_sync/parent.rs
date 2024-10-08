//! Parent-specific IPC implementation over `raw_sync`

use std::collections::HashMap;
use std::io::Write;
use std::process::Stdio;
use std::sync::RwLock;

use anyhow::{anyhow, Context as _, Result};
use tracing::debug;
use uuid::Uuid;

use crate::shmem::raw_sync::{
    RawSyncInit, RawSyncInitResponse, ShmemHandle, DEFAULT_SHARED_MEM_RAW_SYNC_SLAB_SIZE_BYTES,
};
use crate::{get_system_time_millis, ParentProcess, PingMessage, Pinger, PongMessage};

/// ID of a child process that this parent will communicate with
type ChildId = String;

/// Shared memory setup for a bi-directional pipe between parent and child processes
struct SharedMemoryInfo {
    /// Parent handle on shared memory
    parent_write_handle: ShmemHandle,

    /// ID of the child
    child_id: String,

    /// Reference to the shared memory region which the child process
    /// will *write* to.
    child_write_handle: ShmemHandle,
}

/// A parent process that performs IPC via shared memory, in particular using `raw_sync`
#[allow(missing_debug_implementations)]
pub struct RawSyncParent {
    /// UUID of this shared memory parent
    uuid: Uuid,

    /// Children processes connected to this parent
    children: RwLock<HashMap<ChildId, SharedMemoryInfo>>,
}

impl RawSyncParent {
    /// Create a new [`RawSyncParent`]
    #[must_use]
    #[allow(dead_code)]
    pub fn new() -> Self {
        RawSyncParent {
            uuid: Uuid::now_v7(),
            children: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for RawSyncParent {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentProcess for RawSyncParent {
    fn id(&self) -> String {
        self.uuid.to_string()
    }

    fn spawn_child(
        &mut self,
        child_name: impl AsRef<str>,
        mut child_cmd: std::process::Command,
    ) -> Result<std::process::Child> {
        let child_name = child_name.as_ref();

        // Create a shmem segment for child process use
        //
        // NOTE: the parent will create this shared memory region, but
        // *avoid* writing to it, only listening on it for when the child process writes.
        let shmem_size = std::env::var("SHARED_MEM_RAW_SYNC_SLAB_SIZE_BYTES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(DEFAULT_SHARED_MEM_RAW_SYNC_SLAB_SIZE_BYTES);
        let mut child_write_handle = ShmemHandle::new(shmem_size)?;

        // Spawn the child
        debug!("spawning child");
        let mut child = child_cmd
            .stdin(Stdio::piped())
            .spawn()
            .context("failed to spawn child process")?;

        // Create and send initialization message to the child over STDIN
        let init_msg = RawSyncInit {
            write_handle: child_write_handle.to_serialized(),
        };
        debug!(init_msg = ?init_msg, "writing init to child STDIN");
        let mut child_stdin = child.stdin.take().context("failed to get child STDIN")?;
        child_stdin
            .write(&serde_json::to_vec(&init_msg).context("failed to serialize init msg")?)
            .context("failed to write init msg")?;
        child_stdin
            .write(b"\r\n")
            .context("failed to write new line")?;
        child_stdin.flush().context("failed to flush child STDIN")?;

        // Wait & receive the shared memory region information for the child via shared memory,
        // confirming that child->parent send path is at least temporarily working
        debug!("waiting on write signal for init response from child");
        child_write_handle.wait_for_write_signal()?;

        // Read the init response
        let RawSyncInitResponse {
            write_handle,
            child_id,
        } = child_write_handle.read_message()?;
        let parent_write_handle = ShmemHandle::from_serialized(write_handle)
            .context("failed to build parent handle from child init response")?;
        debug!(
            parent_write_handle = ?parent_write_handle,
            child_id,
            "successfully parsed init response from child"
        );

        // Save the shared memory information the child
        let mut children = self
            .children
            .write()
            .map_err(|e| anyhow!("failed to get children for writing: {e}"))?;
        children.insert(
            child_name.into(),
            SharedMemoryInfo {
                parent_write_handle,
                child_write_handle,
                child_id,
            },
        );
        Ok(child)
    }
}

impl Pinger for RawSyncParent {
    fn roundtrip_ping(&self, child_name: impl AsRef<str>) -> anyhow::Result<()> {
        let child = child_name.as_ref();
        debug!(child = child, "retrieving channel for child");

        let mut children = self
            .children
            .write()
            .map_err(|e| anyhow!("failed to get shared mem for writing: {e}"))?;

        let SharedMemoryInfo {
            child_id,
            parent_write_handle,
            child_write_handle,
        } = children
            .get_mut(child)
            .with_context(|| format!("failed to find child [{child}]"))?;

        // Signal writing as busy
        debug!("signaling to start ping write");
        parent_write_handle.write_message(&PingMessage {
            sender_id: self.id(),
            receiver_id: child_id.clone(),
            sent_at_ms: get_system_time_millis()?,
        })?;

        // Wait until child ready
        debug!("waiting for child to signal incoming message");
        child_write_handle.wait_for_write_signal()?;

        // Read child message
        debug!("reading pong");
        let PongMessage {
            sender_id,
            receiver_id,
            ..
        } = child_write_handle.read_message()?;
        debug!("successfully read pong");
        assert!(&sender_id == child_id);
        assert!(receiver_id == self.id());

        Ok(())
    }
}
