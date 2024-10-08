/*!
Process IPC using `mmap()` backed files, via [`shared_mem_queue`].

`shared_mem_queue` uses *only* `mmap`, which has support on unix and Windows based systems.

Generally this method causes the two processes to `mmap()` the *same* file, without requiring
that the file on disk actually changes -- we're less interested in crash resistance for the
file undergoing changes, and more for using the memory region as fast buffer (almost like a single packet)

[0]: <https://crates.io/crates/shared_mem_queue>
**/

use std::io::{BufWriter, Write};
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use bytes::{BufMut, BytesMut};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tracing::debug;

mod child;
pub use child::SharedMemQueueChild;

mod parent;
pub use parent::SharedMemQueueParent;
use shared_mem_queue::SharedMemQueue;

/// Information related to a shared region
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
struct SharedRegionInfo {
    /// This is a path to a file on disk that contains the shared region for both the parent and the child
    ///
    /// (i.e. children should mmap this path and *read* from it)
    file_path: PathBuf,

    /// Offset from the start of the region
    offset: u64,

    /// Length of the shared region that can be written to
    len: usize,
}

/// Message sent to child processes over STDIN that contains
/// information necessary for the child to connect to and synchronize with the parent
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
struct SharedMemQueueInit {
    /// UUID of the parent
    parent_id: String,

    /// Information describing the shared region into which parents should write
    /// in order to send messages to children
    ///
    /// Parents may choose *not* to share the locations of their regions with children
    /// (which could possibly be a different file all together)
    parent_region: Option<SharedRegionInfo>,

    /// Information the child can use to determine where to being writing
    child_region: SharedRegionInfo,
}

/// Message sent from the child process (normally via shared memory) that contains
/// information about the child and whether setup was successful (which is implied)
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
struct SharedMemQueueInitResponse {
    /// UUID of the parent
    parent_id: String,

    /// UUID of the child
    child_id: String,
}

/// MemQueueReader is a wrapper around a [`SharedMemQueue`] that maintains
/// a buffer that is as large as the space required for the memqueue to read,
/// to avoid allocations when processing messages.
///
/// MemQueueReaders can only process one message at a time, and clear internal buffers
/// after every operation.
struct SharedMemQueueHandle<'a, T>
where
    T: Sized + Serialize + DeserializeOwned,
{
    /// The shared queue that messages will be read from
    queue: &'a mut SharedMemQueue,
    /// Scratch buffer that will contain
    buf: Option<BytesMut>,
    /// Market for the relevant T
    _t: std::marker::PhantomData<T>,
    // TODO: customizable serialize/deserialize?
}

impl<'a, T> SharedMemQueueHandle<'a, T>
where
    T: Sized + Serialize + DeserializeOwned,
{
    /// Create a new SharedMemQueueHandle from an existing [`SharedMemQueue`]
    fn new(queue: &'a mut SharedMemQueue) -> SharedMemQueueHandle<'a, T> {
        let buf = BytesMut::with_capacity(queue.space());
        Self {
            queue,
            buf: Some(buf),
            _t: std::marker::PhantomData,
        }
    }

    /// Convert this [`SharedMemQueueHandle`] into one of a different type
    fn into_other<T2>(self) -> SharedMemQueueHandle<'a, T2>
    where
        T2: Sized + Serialize + DeserializeOwned,
    {
        SharedMemQueueHandle::new(self.queue)
    }

    /// Perform a blocking read of an object the queue stored in this [`SharedMemQueueHandle`]
    ///
    /// NOTE: the data that is written into the queue must Serialized and be `u64` length prefixed.
    fn blocking_read(&mut self) -> Result<T> {
        // Read the length-prefix
        debug!(
            "[SharedMemQueueHandle::blocking_read] reading length-prefix of underlying queue..."
        );
        let mut buf = self.buf.take().context("missing buf")?;
        buf.resize(8, 0);
        self.queue.blocking_read(&mut buf[0..8]);
        let len = u64::from_le_bytes(
            buf[0..8]
                .try_into()
                .context("unexpectedly invalid slice length when reading len")?,
        );
        debug!(
            len,
            "[SharedMemQueueHandle::blocking_read] successfully read length"
        );

        // Read the rest of the actual message
        debug!(
            len,
            "[SharedMemQueueHandle::blocking_read] reading len bytes from queue into inner buf"
        );
        let len = usize::try_from(len).context("failed to convert u64 len into usize")?;
        let data_start = 8;
        let data_end = len + 8;
        buf.resize(data_end, 0u8);
        self.queue.blocking_read(&mut buf[data_start..data_end]);
        debug!(
            len,
            "[SharedMemQueueHandle::blocking_read] successfully read bytes into inner"
        );

        // Read the object from the remainig slice
        debug!(
            type_name = std::any::type_name::<T>(),
            "[SharedMemQueueHandle::blocking_read] deserializing bytes into type (JSON)"
        );
        let result = serde_json::from_slice(&buf[data_start..data_end])
            .with_context(|| format!("failed to read from slice [{}->{}]", data_start, data_end))?;
        debug!(
            type_name = std::any::type_name::<T>(),
            "[SharedMemQueueHandle::blocking_read] successfully deserialized type (JSON)"
        );

        // Clear the bytes before we start working with it
        buf.clear();
        self.buf = Some(buf);

        Ok(result)
    }

    /// Perform a blocking write of an object the queue stored in this [`SharedMemQueueHandle`]
    ///
    /// NOTE: the data that is written into the queue must Serialized and be `u64` length prefixed.
    fn blocking_write(&mut self, obj: &T) -> Result<()> {
        debug!(
            type_name = std::any::type_name::<T>(),
            "[SharedMemQueueHandle::blocking_write] writing object into internal buffer"
        );
        let buf = self
            .buf
            .take()
            .context("missing buf during blocking write")?;
        let mut writer = BufWriter::new(buf.writer());

        // Write placeholder for u64 len (we'll fill this in later)
        writer
            .write(&[0u8; 8])
            .context("failed to write length placeholder during blocking write")?;

        // Write the serialized object in
        serde_json::to_writer(&mut writer, obj)
            .context("failed to write to internal buffer during blocking write")?;

        writer
            .flush()
            .context("flush failed during blocking write")?;

        // Convert back into bytes mut
        let mut buf = writer
            .into_inner()
            .context("failed to convert writer back into BytesMut")?
            .into_inner();

        // Fill in the length
        let obj_bytes_len = buf.len() - 8;
        buf[0..8].swap_with_slice(&mut u64::to_le_bytes(
            u64::try_from(obj_bytes_len)
                .context("failed to convert usize to u64 during blocking write")?,
        ));

        debug!(
            type_name = std::any::type_name::<T>(),
            msg_bytes_written = obj_bytes_len,
            "[SharedMemQueueHandle::blocking_write] successfully wrote serialized object to internal buffer",
        );

        debug!(
            bytes_written = obj_bytes_len + 8,
            "[SharedMemQueueHandle::blocking_write] writing length-prefixed bytes to shared message queue"
        );
        self.queue.blocking_write(&buf[0..obj_bytes_len + 8]);

        buf.clear();
        self.buf = Some(buf);
        Ok(())
    }
}
