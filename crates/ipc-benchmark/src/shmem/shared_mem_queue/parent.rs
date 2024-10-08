//! Parent-specific IPC implementation over `mmap`-backed files (using [`shared_mem_queue`])

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Write;
use std::process::{Child, Command, Stdio};

use anyhow::{ensure, Context as _, Result};
use memmap::MmapMut;
use shared_mem_queue::SharedMemQueue;
use tracing::{debug, info};
use uuid::Uuid;

use crate::shmem::shared_mem_queue::SharedMemQueueHandle;
use crate::shmem::shared_mem_queue::{
    SharedMemQueueInit, SharedMemQueueInitResponse, SharedRegionInfo,
};
use crate::{get_system_time_millis, ParentProcess, PingMessage, Pinger, PongMessage, RpcPong};

/// ID of a child process (as reported by the child)
type ChildId = String;

/// Name of a child process (known at start time)
type ChildName = String;

/// Bi-directional channel for communication
struct SharedMemQueueChannel {
    /// Self-reported ID of the child
    child_id: ChildId,

    /// Shared mem queue that parents should write to in order to communicate
    parent: SharedMemQueue,

    /// Shared mem queue that children will write to in order to communicate (parents must read from this)
    child: SharedMemQueue,

    /// File that contains the shared region
    ///
    /// NOTE: this information must be held to ensure that the file is not dropped
    /// and can still be written to.
    _shared_region_file: File,

    /// MMap'd region that contains messages going to the child
    ///
    /// As the SharedMemQueue uses a pointer to this mmap, we hold it in this
    /// structure to prevent dropping
    _to_child_region_mmap: MmapMut,

    /// MMap'd region that contains messages coming from the child
    ///
    /// As the SharedMemQueue uses a pointer to this mmap, we hold it in this
    /// structure to prevent dropping
    _from_child_region_mmap: MmapMut,
}

/// A parent process that performs IPC via shared memory, in particular using [`shared_mem_queue`]
pub struct SharedMemQueueParent {
    /// UUID of the parent process
    uuid: Uuid,

    /// Channels for writing to parents by child ID
    ///
    /// SAFETY: We're safe using a `RefCell` here because this structure
    /// is very much *not* multi-threaded.
    channels: HashMap<ChildName, RefCell<SharedMemQueueChannel>>,
}

impl std::fmt::Debug for SharedMemQueueParent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedMemQueueParent")
            .field("uuid", &self.uuid)
            .finish()
    }
}

impl Default for SharedMemQueueParent {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedMemQueueParent {
    /// Create a [`SharedMemQueueParent`]
    #[must_use]
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            uuid: Uuid::now_v7(),
            channels: HashMap::new(),
        }
    }
}

/// Size of shared region (file) to create.
///
/// Each side (child, parent) will only be able to send *half* of this amount
///
/// Due to a bug in `shared-memory-queue` we must use *at least* 32MB, as otherwise we run
/// out of space, and attempt to write to invalid regions of the shared file.
///
/// In release mode, we can send 10x as many messages, so we use 320MB
const DEFAULT_SHARED_REGION_LEN_BYTES: usize = 320 * 1024 * 1024;

impl ParentProcess for SharedMemQueueParent {
    fn id(&self) -> String {
        self.uuid.to_string()
    }

    fn spawn_child(&mut self, name: impl AsRef<str>, mut cmd: Command) -> Result<Child> {
        // Create a temp directory that will hold the file which will hold the write region
        // NOTE that we *cannot* use `tempdir` here because we need the file to persist
        let shared_region_file_name = format!("region.parent-{}.managed", self.uuid);
        let shared_region_file_path = std::env::temp_dir().join(shared_region_file_name);
        let shared_region_len_bytes: usize =
            std::env::var("SHARED_MEM_QUEUE_SHARED_REGION_LEN_BYTES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_SHARED_REGION_LEN_BYTES);
        let shared_region_offset_bytes: u64 = 0;
        info!(shared_region_len_bytes, "determined shared mem queue size");

        // Create the file that will be used for the memory mapping
        debug!(
            shared_region_file_path = %shared_region_file_path.display(),
            "creating shared region file"
        );
        let shared_region_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&shared_region_file_path)
            .with_context(|| {
                format!(
                    "failed to create new region file for writing as parent @ [{}]",
                    shared_region_file_path.display()
                )
            })?;
        shared_region_file
            .set_len(u64::try_from(shared_region_len_bytes).with_context(|| {
                format!("failed to convert shared region length [{shared_region_len_bytes}] to u64")
            })?)
            .context("failed to set shared region file size")?;

        // Create a memory mapped region in the parent, based on the file
        debug!("mmapping shared file");
        let mut from_child_region_mmap = unsafe {
            memmap::MmapOptions::new()
                .offset(shared_region_offset_bytes)
                .len(shared_region_len_bytes)
                .map_mut(&shared_region_file)
                .context("failed to create mmap")?
        };

        let region_half_len = shared_region_len_bytes / 2;

        // Create the queues for both parent and child
        debug!("creating queue for child to write to");
        let mut from_child =
            unsafe { SharedMemQueue::create(from_child_region_mmap.as_mut_ptr(), region_half_len) };

        // Create a message that will inform the child of the shared mmap'd file
        let init_msg = SharedMemQueueInit {
            parent_id: self.uuid.to_string(),
            parent_region: Some(SharedRegionInfo {
                file_path: shared_region_file_path.clone(),
                offset: shared_region_offset_bytes,
                len: region_half_len,
            }),
            child_region: SharedRegionInfo {
                file_path: shared_region_file_path,
                offset: shared_region_offset_bytes
                    .checked_add(u64::try_from(region_half_len)?)
                    .context("overflowed region offset calculation")?,
                len: region_half_len,
            },
        };

        // Spawn the child
        debug!("spawning child");
        let mut child = cmd
            .stdin(Stdio::piped())
            .spawn()
            .context("failed to spawn child process")?;

        // Send the init message over stdin
        debug!("writing init to child STDIN");
        let mut child_stdin = child.stdin.take().context("failed to get child STDIN")?;
        child_stdin
            .write(&serde_json::to_vec(&init_msg).context("failed to serialize init msg")?)
            .context("failed to write init msg")?;
        child_stdin
            .write(b"\r\n")
            .context("failed to write new line")?;
        child_stdin.flush().context("failed to flush child STDIN")?;

        // At this point, the child should have access to the created queue in the parent to write to
        //
        // We attempt to receive the response (the first message from the child) via the shared memory region.

        let mut reader = SharedMemQueueHandle::<SharedMemQueueInitResponse>::new(&mut from_child);
        let init_resp: SharedMemQueueInitResponse = reader
            .blocking_read()
            .context("failed to deserialize init response message")?;
        ensure!(
            init_resp.parent_id == self.uuid.to_string(),
            "parent ID reported by child did not match"
        );

        let mut to_child_region_mmap = unsafe {
            memmap::MmapOptions::new()
                .offset(shared_region_offset_bytes)
                .len(shared_region_len_bytes)
                .map_mut(&shared_region_file)
                .context("failed to create mmap")?
        };

        debug!(
            child_name = name.as_ref(),
            "creating & attaching queue for sending messages to child"
        );
        let to_child = unsafe {
            SharedMemQueue::attach(
                to_child_region_mmap.as_mut_ptr().add(region_half_len),
                shared_region_len_bytes,
            )
        };

        // Save information to local registry
        debug!(child_name = name.as_ref(), "saving child information");
        self.channels.insert(
            name.as_ref().into(),
            RefCell::new(SharedMemQueueChannel {
                child_id: init_resp.child_id,
                parent: to_child,
                child: from_child,
                _shared_region_file: shared_region_file,
                _to_child_region_mmap: to_child_region_mmap,
                _from_child_region_mmap: from_child_region_mmap,
            }),
        );

        Ok(child)
    }
}

impl Pinger for SharedMemQueueParent {
    fn roundtrip_ping(&self, child_name: impl AsRef<str>) -> anyhow::Result<()> {
        let child = child_name.as_ref();
        debug!(child = child, "retrieving channel for child");
        let mut chan = self
            .channels
            .get(child)
            .context("failed to find child with given name")?
            .borrow_mut();

        let child_id = chan.child_id.clone();
        debug!(child_id, child, "found channel for child");

        // Build the ping message
        debug!(child, "sending ping message to child");
        let mut outgoing_handle = SharedMemQueueHandle::<PingMessage>::new(&mut chan.parent);
        outgoing_handle
            .blocking_write(&PingMessage {
                sender_id: self.uuid.to_string(),
                receiver_id: child_id.clone(),
                sent_at_ms: get_system_time_millis()?,
            })
            .context("failed to send ping message to child")?;
        debug!(child, "successfully sent ping message to child");

        debug!(child, "reading pong message from child");
        let mut reader = SharedMemQueueHandle::<PongMessage>::new(&mut chan.child);
        let pong_msg: PongMessage = reader
            .blocking_read()
            .context("failed to deserialize pong message")?;

        ensure!(pong_msg.sender_id() == child_id, "child ID matches");
        ensure!(
            pong_msg.receiver_id() == self.uuid.to_string(),
            "parent ID matches"
        );

        Ok(())
    }
}
