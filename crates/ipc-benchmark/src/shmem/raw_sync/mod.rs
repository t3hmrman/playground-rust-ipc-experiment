/*!
Process IPC using shared memory, using [`raw_sync`][0] in combination with [`shared_memory`][1].

It's possible (and even likely) that a more efficient implementation could be written with more details and a more tailored solution to a given problem domain, but the assumption here is that `raw_sync` and `shared_memory` are reasonably efficient.

We choose *not* to use `ipmpsc`[2] here becuase [it doesn't yet reliably support MacOS when `shared_memory` does][3].

[0]: <https://crates.io/crates/raw_sync>
[1]: <https://crates.io/crates/shared_memory>
[2]: <https://crates.io/crates/ipmpsc>
[3]: <https://github.com/dicej/ipmpsc/issues/4>
**/

use anyhow::{anyhow, ensure, Context as _, Result};
use raw_sync::events::{BusyEvent, EventImpl, EventInit as _, EventState};
use raw_sync::Timeout;
use serde::{Deserialize, Serialize};

pub mod child;
pub mod parent;

pub use child::RawSyncChild;
pub use parent::RawSyncParent;
use shared_memory::{Shmem, ShmemConf};
use tracing::debug;

/// Size of the slab used for shared memory
const DEFAULT_SHARED_MEM_RAW_SYNC_SLAB_SIZE_BYTES: usize = 128 * 1024;

/// Information required to initialize a shared memory backed handle
///
/// This is normally used in parent -> child initial communication
#[derive(Debug, Serialize, Deserialize)]
struct RawSyncInit {
    /// A handle to OS shared memory that must be used by the receiver
    /// (of this `RawSyncInit` message) to write
    write_handle: SerializedShmemHandle,
}

/// Information returned from a child upon succcessful initialization
///
/// Unlike the init message, this is normally returned *over* the new communciation channel
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RawSyncInitResponse {
    /// A handle to OS shared memory that must be used by the receiver
    /// (of this `RawSyncInit` message) to write
    pub(crate) write_handle: SerializedShmemHandle,

    /// ID of the child that was initialized
    pub(crate) child_id: String,
}

// TODO: Introduce a Handle type for raw sync stuff
pub(crate) struct ShmemHandle {
    /// Size of shared memory region in bytes
    pub(crate) size_bytes: usize,

    /// `shared_memory` object (built from a [`shared_memory::ShmemConf`]
    pub(crate) shmem: (ShmemConf, Shmem),

    /// Signal used to write signal
    ///
    /// NOTE: signals are *always* located in the first couple bytes of a shared memory region, for simplicity
    pub(crate) write_signal: Box<dyn EventImpl>,
}

impl std::fmt::Debug for ShmemHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShmemHandle")
            .field("size_bytes", &self.size_bytes)
            .field("os_id", &self.get_os_id())
            .finish()
    }
}

impl ShmemHandle {
    /// Create a shared memory, given a certain size, with signaling built in
    pub(crate) fn new(size_bytes: usize) -> Result<Self> {
        // Create a shmem configuration that the child will use to write to
        let shmem_conf = ShmemConf::new().size(size_bytes);
        let mut shmem = shmem_conf
            .clone()
            .create()
            .context("failed to create shared memory")?;
        let shmem_bytes = unsafe { shmem.as_slice_mut() };
        // Use the first two bytes as a busy signaling area for the parent
        // The parent sets and child reads to know when messages are ready
        let (write_signal, _size) = unsafe {
            BusyEvent::new(shmem_bytes.get_mut(0).unwrap(), true)
                .map_err(|e| anyhow!("failed to build signal for parent shmem region: {e}"))?
        };
        write_signal
            .set(EventState::Clear)
            .map_err(|e| anyhow!("failed to set initial busy signal to clear: {e}"))?;

        Ok(Self {
            size_bytes,
            shmem: (shmem_conf, shmem),
            write_signal,
        })
    }

    /// Build a [`ShmemHandle`] from a given OS ID and some information
    ///
    /// # Arguments
    ///
    /// * `os_id` - OS-specific identifier for OS-managed shared memory
    /// * `size_bytes` - Total size of the ShmemHandle in bytes
    ///
    pub(crate) fn from_os_id(os_id: &str, size_bytes: usize) -> Result<Self> {
        let shmem_conf = ShmemConf::new().os_id(os_id);
        let mut shmem = shmem_conf
            .clone()
            .open()
            .with_context(|| format!("failed to open shared memory with OS ID [{os_id}]"))?;

        // Rebuild the signal
        let shmem_bytes = unsafe { shmem.as_slice_mut() };
        // Use the first two bytes as a busy signaling area for the parent
        // The parent sets and child reads to know when messages are ready
        let (signal, _size) = unsafe {
            BusyEvent::new(shmem_bytes.get_mut(0).unwrap(), true)
                .map_err(|e| anyhow!("failed to build signal for parent shmem region: {e}"))?
        };
        signal
            .set(EventState::Clear)
            .map_err(|e| anyhow!("failed to set initial busy signal to clear: {e}"))?;

        Ok(Self {
            size_bytes,
            shmem: (shmem_conf, shmem),
            write_signal: signal,
        })
    }

    /// Build a [`ShmemHandle`] from a [`SerializedShmemHandle`]
    pub(crate) fn from_serialized(
        SerializedShmemHandle { os_id, size_bytes }: SerializedShmemHandle,
    ) -> Result<Self> {
        Self::from_os_id(&os_id, size_bytes)
    }

    /// Get the OS ID of the associated [`Shmem`]
    ///
    /// NOTE: this cannot be used across operating systems/network boundaries,
    /// i.e. this value must be used on processing operating in the same OS,
    /// as the OS ID is inherently specific to the running OS when called.
    ///
    #[must_use]
    pub(crate) fn get_os_id(&self) -> &str {
        self.shmem.1.get_os_id()
    }

    /// Wait for a signal on the write region
    fn wait_for_write_signal(&mut self) -> Result<()> {
        self.write_signal
            .wait(Timeout::Infinite)
            .map_err(|e| anyhow!("failed to wait for write signal: {e}"))
    }

    /// Clear the write signal
    fn clear_write_signal(&mut self) -> Result<()> {
        self.write_signal
            .set(EventState::Clear)
            .map_err(|e| anyhow!("failed to clear write signal: {e}"))
    }

    /// Create a serialized version of the [`ShmemHandle`] top send
    #[must_use]
    fn to_serialized(&self) -> SerializedShmemHandle {
        SerializedShmemHandle {
            os_id: self.get_os_id().to_string(),
            size_bytes: self.size_bytes,
        }
    }

    /// Read a single message from the write region
    ///
    /// NOTE: messages are assumed to be LE length-prefixed, and the
    /// length-prefix should start *after* those initial 2 bytes (e.g. `bytes[2..10]`)
    fn read_message<'a, T: Deserialize<'a>>(&'a mut self) -> Result<T> {
        let bytes = unsafe { self.shmem.1.as_slice_mut() };
        debug!("reading init response from child");
        let message_len = u64::from_le_bytes(
            bytes[2..10]
                .try_into()
                .context("unexpectedly invalid byte range for LE u64")?,
        );
        assert!(
            (message_len as usize) < (bytes.len() - 2),
            "invalid length headder, message must overflow available space",
        );
        let msg_bytes = &bytes[10..message_len as usize + 10];
        debug!(message_len, "read init response");
        serde_json::from_slice(msg_bytes).context("failed to parse init response JSON")
    }

    /// Get the max message size (not including the `usize`'d length prefix)
    #[must_use]
    fn max_msg_size(&self) -> usize {
        self.size_bytes - 2 - size_of::<usize>()
    }

    /// Write a single message to the write region
    fn write_message<T: Serialize>(&mut self, obj: T) -> Result<usize> {
        // Clear the write-finished signal
        self.write_signal
            .set(EventState::Clear)
            .map_err(|e| anyhow!("failed to set parent write signal: {e}"))?;

        let max_msg_size = self.max_msg_size();
        let bytes = unsafe { self.shmem.1.as_slice_mut() };
        let msg_bytes = serde_json::to_vec(&obj).context("failed to serialize ping message")?;
        let msg_len = msg_bytes.len();

        ensure!(
            msg_len <= max_msg_size,
            "serialized message of len [{msg_len}] is greater than max message size [{}]",
            self.max_msg_size()
        );

        // Write out the length prefix
        bytes[2..10].copy_from_slice(
            &u64::try_from(msg_bytes.len())
                .context("failed to convert msg len to u64")?
                .to_le_bytes(),
        );
        // Write out the message bytes
        bytes[10..msg_bytes.len() + 10].copy_from_slice(&msg_bytes);

        // Trigger the write-finished signal
        self.write_signal
            .set(EventState::Signaled)
            .map_err(|e| anyhow!("failed to set parent write signal: {e}"))?;

        Ok(msg_bytes.len())
    }
}

/// This class exists as a proxy to enable serialization os [`ShmemHandle`]
///
/// Receivers of serialized versions must reconstruct [`ShmemHandle`]s from these values
///
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct SerializedShmemHandle {
    /// ID for the shared memory region
    os_id: String,
    /// Size of the shared memory area in bytes
    size_bytes: usize,
}

impl Serialize for ShmemHandle {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        SerializedShmemHandle {
            os_id: self.get_os_id().to_string(),
            size_bytes: self.size_bytes,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ShmemHandle {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        SerializedShmemHandle::deserialize(deserializer)?
            .try_into()
            .map_err(|e| {
                serde::de::Error::custom(format!(
                    "failed to deserialize proxy (SerializedShmemHandle struct): {e}"
                ))
            })
    }
}

impl TryFrom<SerializedShmemHandle> for ShmemHandle {
    type Error = anyhow::Error;

    fn try_from(value: SerializedShmemHandle) -> Result<Self> {
        Self::from_os_id(&value.os_id, value.size_bytes)
    }
}
