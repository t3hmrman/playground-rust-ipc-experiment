/*!
Implementations for parent-child IPC with shared memory.

There are multiple implementations available named mostly after the crates they depend on:

- [`shared_mem_queue`][crate-shared-mem-queue]
- [`raw_sync`][crate-raw-sync]

[crate-shared-mem-queue]: https://crates.io/crates/shared-mem-queue
[crate-raw-sync]: https://crates.io/crates/raw-sync

**/

pub mod raw_sync;
pub mod shared_mem_queue;
