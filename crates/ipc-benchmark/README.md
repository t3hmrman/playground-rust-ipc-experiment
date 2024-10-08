# IPC benchmark

This folder contains an experiment into IPC benchmarking to see just how fast some libraries in the ecosystem can go in a typical parent/child process situation:

- [`ipc-channel`][ipc-channel] (*without* [`ipc-rpc`][ipc-rpc]) (see: [`./src/ipcc`](./src/ipcc))
- [`shared_memory`][shared_memory] + [`shared-memory-queue`][shared-mem-queue] (see: [`./src/shmem/shared_mem_queue`](./src/shmem/shared_mem_queue))
- [`shared_memory`][shared_memory] + [`raw_sync`][raw-sync] (see: [`./src/shmem/raw_sync`](./src/shmem/raw_sync))

Obviously, `shared_memory` requires much more additional implementation than `ipc-channel`/`ipc-rpc`, but given the results 3tilley saw, it's worth checking out as it's *obviously* the fastest implementation, and that is likely to hold true.

The code in this crate is likely slightly more complex than strictly necessary (ex. we could ping-pong with fewer fields), but it's meant to represent a slightly more production-ready codebase.

[ipc-channel]: https://crates.io/crates/ipc-channel
[shared_memory]: http://crates.io/crates/shared_memory
[shmem]: https://crates.io/crates/shmem
[shared-mem-queue]: https://crates.io/crates/shared-mem-queue
[ipc-rpc]: https://crates.io/crates/ipc-rpc
[raw-sync]: https://crates.io/crates/raw-sync
[3tilley-code]: https://github.com/3tilley/rust-experiments/blob/master/ipc/src/shmem.rs

## Prior art & new methodology

The implementation for the `shared_memory` + `raw_sync` implementation mostly copied from [the code shared by 3tilley][3tilley-code], which saved a ton of time.

This experiment as a whole was inspired by the the long standing question of "which IPC is best" and a desire to reproduce 3tilley's results independently.

One major difference between this project and 3tilley's is that numbers with JSON serialization are always present -- i.e. we do not simply write `"ping"` and `"pong"`. All examples include JSON serialization in the hot path, with serialization/deserialization of payloads (see: `PingMessage`, etc). Some examples *also* include basic writing of data (ex. `"ping"` and `"pong"`) as well.

Another difference is that all child processes bootstrap their configuration via STDIN -- the parent provides an initial configuration payload (serialized JSON) over STDIN after first startup, then switches to another memory sharing method when possible.

The hope is that we'll be able to see how a more fully featured or robust application would fare with the naive starting point of JSON for serialization. There are of course better choices out there -- gRPC, Postcard, even msgpack -- but the idea is to keep the load roughly the same, and more than a trivial amount of computation.

While the code has not been aggressively optimized, it's representative of a reasonable first hack at trying to make these methods work in a somewhat robust manner -- though things like message chunking/segmentation/framing are not supported.

One of the goals is to also see how ergonomic each approach proves to be, though the answers to the ergonomics question are somewhat easy to guess up front.

## Quickstart

### IPC via `ipc-channel`

See how many round-trips we can get over a simple IPC channel between parent & child processes in debug mode:

```console
just ipc-ipcc
```

> [!NOTE]
> By default it runs for 10 seconds, you can change this with the `TEST_DURATION_SECONDS` ENV var

Run in release mode for better perf:

```console
BUILD_MODE=release just ipc-ipcc
```

With the following caveats:

- 2022 Macbook Air M2 w/ 8GB Memory
- Single core sender (spawned thread that sends in a tight loop for t seconds)
- Single core receiver (parent process is not multi-threaded)
- Plugged in

A one-off test produces the following results:

| Mode                             | Roundtrips | Time (seconds) | ~Roundtrips/second/core |
|----------------------------------|------------|----------------|-------------------------|
| DEBUG                            | 162,181    | 10             | 16,218                  |
| RELEASE (simple string payloads) | 1,660,272  | 10             | 166,027                 |
| RELEASE (json payloads)          | 1,368,321  | 10             | 136,832                 |

Comparing the results here to the [3tilley's simple IPC post][3tilley-post], we get roundtrips/second over `ipc-channel` which is low level file-handle-passing-over sockets:

> An implementation of the Rust channel API over process boundaries. Under the hood, this API uses Mach ports on Mac and file descriptor passing over Unix sockets on Linux. The serde library is used to serialize values for transport over the wire.

We're probably limited even somewhat here by `serde` use by default (`IpcReceiver<T>` versus `IpcBytesReceiver`), assuming we can create more efficient encoding/decoding than `serde` (and in particular `serde_json`) does by default.

Using `ipc-channel` gets us easy Windows, Mac, and Linux support.

[3tilley-post]: https://3tilley.github.io/posts/simple-ipc-ping-pong/

### IPC via `shared_memory` + `shared-memory-queue`

See how many round-trips we can get over a shared memory region, managed by [`shared-memory-queue`][shared-mem-queue] between a parent & child process in debug mode:

```console
just ipc-shmem-shared-mem-queue
```

> [!NOTE]
> By default it runs for 10 seconds, you can change this with the `TEST_DURATION_SECONDS` ENV var

Run in release mode for better perf:

```console
BUILD_MODE=release just ipc-shmem-shared-mem-queue
```

With the following caveats:

- 2022 Macbook Air M2 w/ 8GB Memory
- Single core sender (spawned thread that sends in a tight loop for t seconds)
- Single core receiver (parent process is not multi-threaded)
- Plugged in

A one-off test produces the following results:

| Mode                    | Roundtrips | Time (seconds) | ~Roundtrips/second/core |
|-------------------------|------------|----------------|-------------------------|
| DEBUG                   | 271,985    | 10             | 27,1989.5               |
| RELEASE (json payloads) | 742,751    | 10             | 74,275                  |

> [!NOTE]
> There is a bug (intended behavior?) in `shared-memory-queue` which causes the shared memory region to *run out of space*.
> While it *should* be actually cycling, in some cases we will actually run out of space.
>
> To be able to properly finish, we must use files of size `33554432` bytes (i.e. 32MiB)

### IPC via `shared_memory` + `raw_sync`

See how many round-trips we can get over explicitly OS-supported shared memory regions with manual signal management (via [`raw-sync`][raw-sync]).

```console
just ipc-shmem-raw-sync
```

> [!NOTE]
> By default it runs for 10 seconds, you can change this with the `TEST_DURATION_SECONDS` ENV var

Run in release mode for better perf:

```console
BUILD_MODE=release just ipc-shmem-raw-sync
```

With the following caveats:

- 2022 Macbook Air M2 w/ 8GB Memory
- Single core sender (spawned thread that sends in a tight loop for t seconds)
- Single core receiver (parent process is not multi-threaded)
- Plugged in

A one-off test produces the following results:

| Mode                    | Roundtrips | Time (seconds) | ~Roundtrips/second/core |
|-------------------------|------------|----------------|-------------------------|
| DEBUG                   | 740,398    | 10             | 74,080.5                |
| RELEASE (json payloads) | 10,325,882 | 10             | 1,032,588.2             |

## Perf ideas

This section contains some ideas on not-yet-explored efficiency/performance gains.

### Easy win: more efficient serialization format than `serde_json`

Given that a type implementing `serde::Serialize` means it can use a *wide variety of implementations*, JSON is not particularly efficient -- we could use other things for this.

It might be nice to default to `serde_json` since it's easy to inspect on the wire, and add features that support other serialization mechanisms.

## Configuration

This project (runner and parent/child processes) can be controlled by environment variables, listed below:

| Variable                                   | Default | Example               | Description                                                                                                                                                     |
|--------------------------------------------|---------|-----------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `IPCC_CHILD_BIN_PATH`                      | N/A     | `/path/to/ipcc-child` | Path to the child binary that should be launched by the parent process (calculated by default in the `Justfile`)                                                |
| `RPC_MESSAGE_COMPLEXITY`                   | `json`  | `raw-string`          | Changes the message complexity for the parent and child (values: `raw-string`, `json`) complexity (note, this does *not* affect initial parent/child handshake) |
| `SHARED_MEM_QUEUE_SHARED_REGION_LEN_BYTES` | 4194304 | `8388608`             | Number of bytes used for the file with the shared region. Child/Parent processes will be able to use *half* of this to send messages.                           |

You can ignore these and read through the quickstart sections below for commands you should be running
