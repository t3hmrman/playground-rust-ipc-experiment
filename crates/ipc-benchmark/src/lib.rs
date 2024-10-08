//! This crate contains
//!
//! Inspired by lots of wondering and [the post by 3tilley][3tilley-post].
//!
//! [3tilley-post]: <https://3tilley.github.io/posts/simple-ipc-ping-pong>

#![deny(
    missing_docs,
    clippy::missing_docs_in_private_items,
    missing_debug_implementations,
    rustdoc::broken_intra_doc_links,
    rustdoc::private_intra_doc_links,
    rustdoc::missing_crate_level_docs,
    rustdoc::missing_doc_code_exmaples,
    rustdoc::invalid_codeblock_attributes,
    rustdoc::invalid_html_tags,
    rustdoc::invalid_rust_codeblocks,
    rustdoc::bare_urls,
    rustdoc::unescaped_backticks,
    rustdoc::redundant_explicit_links
)]

use std::{process::Command, time::SystemTime};

use anyhow::{bail, ensure, Context as _, Result};
use ipc_channel::ipc::IpcBytesSender;
use serde::{Deserialize, Serialize};

pub mod ipcc;
pub mod shmem;

pub use raw_sync::*;
pub use shared_memory::*;

/// ENV variable for setting RPC message complexity
const ENV_VAR_RPC_MESSAGE_COMPLEXITY: &str = "RPC_MESSAGE_COMPLEXITY";

/// Human-friendly name of a child process
type ChildName = String;

/// ID of a child process
type ChildId = String;

/// Complexity of the RPC message(s) that will be sent.
///
/// This is normally convfigured via ENV ("RPC_MESSAGE_COMPLEXITY"), and parsed
/// into this structure for easy usage from code.
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
#[serde(rename_all = "kebab-case")]
pub enum RpcMessageComplexity {
    /// Raw strings (ex. for ping-pong this means "ping" and "pong" as payloads)
    RawString,
    /// JSON is the default since it's the more likely production use case
    #[default]
    Json,
}

impl std::str::FromStr for RpcMessageComplexity {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "json" => Ok(Self::Json),
            "raw-string" => Ok(Self::RawString),
            _ => bail!("invalid RpcMessageComplexity value [{s}]"),
        }
    }
}

impl RpcMessageComplexity {
    /// Retreive from env or use the default (Json)
    pub fn from_env_or_default(values: impl Iterator<Item = (String, String)>) -> Self {
        for (k, v) in values {
            if k == ENV_VAR_RPC_MESSAGE_COMPLEXITY {
                return <Self as std::str::FromStr>::from_str(&v).unwrap_or(Self::default());
            }
        }
        Self::default()
    }
}

/// Child process that can be used for testing IPC
pub trait ChildProcess {
    /// ID of the child process
    fn id(&self) -> String;

    /// Execute as the running process.
    ///
    /// This command is expected to never return, as child processes
    /// should handle messages forever.
    fn run(self) -> Result<()>;
}

/// Parent process that can be used for testing IPC
pub trait ParentProcess {
    /// ID of the parent
    fn id(&self) -> String;

    /// Spawn the child process
    fn spawn_child(&mut self, name: impl AsRef<str>, cmd: Command) -> Result<std::process::Child>;

    /// Perform setup with a spawned child process, if necessary.
    ///
    /// Note that in most cases, it is not necessary to do any additional setup (ex. handshaking)
    /// after successful creation of a child process.
    fn setup_child(&mut self, _name: impl AsRef<str>) -> Result<()> {
        Ok(())
    }
}

/// Enables ping-pong interaction between parent and child
pub trait Pinger: ParentProcess {
    /// Invoke a 'ping' from the parent, and receive a 'pong' from the child
    ///
    /// The actual details of what a ping/pong consist of depend on parent/child implementations,
    /// but usually means sending (i.e. serializing and transferring) a 'ping' message, and
    /// doing the same for a 'pong' message.
    fn roundtrip_ping(&self, child_process_name: impl AsRef<str>) -> Result<()>;
}

/// Message sent in a ping
///
/// The fields in this message aren't important but in serialization/deserialization do
/// offer some trivial work for parents and clients to perform, which is more in line with real
/// use cases.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[non_exhaustive]
pub struct PingMessage {
    /// Sender of the ping message
    sender_id: String,
    /// Intended receiver
    receiver_id: String,
    /// When the message was sent
    ///
    /// Time elapsed since the unix epoch in milliseconds
    sent_at_ms: u128,
}

impl PingMessage {
    /// Create a new [`PingMessage`]
    pub fn new(sender_id: String, receiver_id: String, sent_at_ms: u128) -> Self {
        Self {
            sender_id,
            receiver_id,
            sent_at_ms,
        }
    }

    /// Retrieve the sender (ID) of the message
    #[must_use]
    pub fn sender_id(&self) -> &str {
        &self.sender_id
    }

    /// Retrieve the intended receiver (ID) of the message
    #[must_use]
    pub fn receiver_id(&self) -> &str {
        &self.receiver_id
    }

    /// Retrieve when the message was sent
    #[must_use]
    pub fn sent_at_ms(&self) -> u128 {
        self.sent_at_ms
    }
}

/// Message sent in a pong
///
/// The fields in this message aren't important but in serialization/deserialization do
/// offer some trivial work for parents and clients to perform, which is more in line with real
/// use cases.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[non_exhaustive]
pub struct PongMessage {
    /// Sender of the pong message
    sender_id: String,
    /// Intended receiver
    receiver_id: String,
    /// When the message was sent
    ///
    /// Time elapsed since the UNIX epoch in milliseconds
    sent_at_ms: u128,
}

impl PongMessage {
    /// Create a new [`PongMessage`]
    pub fn new(sender_id: String, receiver_id: String, sent_at_ms: u128) -> Self {
        Self {
            sender_id,
            receiver_id,
            sent_at_ms,
        }
    }

    /// Retrieve when the message was sent
    #[must_use]
    pub fn sent_at_ms(&self) -> u128 {
        self.sent_at_ms
    }
}

/// Trait that represents all responses that qualify as an "ping" over RPC
///
/// This trait exists so that both simple and complex sending patterns (ex. raw strings vs JSON)
/// Can return objects that confirm to the same interface and be usable downstream
trait RpcPing {
    /// Retreive the sender ID for the RPC message
    fn sender_id(&self) -> &str;

    /// Retreive the receiver ID for the RPC message
    fn receiver_id(&self) -> &str;
}

/// Trait that represents all responses that qualify as an "pong" over RPC
///
/// This trait exists so that both simple and complex sending patterns (ex. raw strings vs JSON)
/// Can return objects that confirm to the same interface and be usable downstream
trait RpcPong {
    /// Retreive the sender ID for the RPC message
    fn sender_id(&self) -> &str;

    /// Retreive the receiver ID for the RPC message
    fn receiver_id(&self) -> &str;
}

/// Simple version of a pong message, represented by a string
struct RawStringPongMessage(String);

impl std::str::FromStr for RawStringPongMessage {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.split('|').collect::<Vec<&str>>()[..] {
            [sender, receiver, pong] => {
                ensure!(pong == "pong", "pong was present");
                ensure!(!sender.is_empty(), "sender is not empty");
                ensure!(!receiver.is_empty(), "receiver is not empty");
                Ok(Self(s.to_string()))
            }
            _ => bail!("failed to parse RawStringPongMessage from str [{s}]"),
        }
    }
}

impl RpcPong for RawStringPongMessage {
    /// NOTE: this can return empty string if missing
    fn sender_id(&self) -> &str {
        self.0
            .split_once('|')
            .map(|(fst, _)| fst)
            .unwrap_or_default()
    }

    fn receiver_id(&self) -> &str {
        if let Some((_fst, snd)) = self.0.split_once('|') {
            if let Some((fst, _snd)) = snd.split_once('|') {
                return fst;
            }
        }
        ""
    }
}

/// Simple version of a ping message, represented by a string
struct RawStringPingMessage(String);

impl std::str::FromStr for RawStringPingMessage {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.split('|').collect::<Vec<&str>>()[..] {
            [sender, receiver, ping] => {
                ensure!(ping == "ping", "ping was present");
                ensure!(!sender.is_empty(), "sender is not empty");
                ensure!(!receiver.is_empty(), "receiver is not empty");
                Ok(Self(s.to_string()))
            }
            _ => bail!("failed to parse RawStringPingMessage from str [{s}]"),
        }
    }
}

impl RpcPing for RawStringPingMessage {
    /// NOTE: this can return empty string if missing
    fn sender_id(&self) -> &str {
        self.0
            .split_once('|')
            .map(|(fst, _)| fst)
            .unwrap_or_default()
    }

    fn receiver_id(&self) -> &str {
        if let Some((_fst, snd)) = self.0.split_once('|') {
            if let Some((fst, _snd)) = snd.split_once('|') {
                return fst;
            }
        }
        ""
    }
}

impl RpcPong for PongMessage {
    /// Retrieve the sender (ID) of the message
    #[must_use]
    fn sender_id(&self) -> &str {
        &self.sender_id
    }

    /// Retrieve the intended receiver (ID) of the message
    #[must_use]
    fn receiver_id(&self) -> &str {
        &self.receiver_id
    }
}

/// Required to make deserialize work properly
///
/// https://github.com/servo/ipc-channel/issues/238
#[derive(Debug, Serialize, Deserialize)]
struct IpcBytesSenderWrapper(IpcBytesSender);

impl From<IpcBytesSender> for IpcBytesSenderWrapper {
    fn from(value: IpcBytesSender) -> Self {
        Self(value)
    }
}

impl AsMut<IpcBytesSender> for IpcBytesSenderWrapper {
    fn as_mut(&mut self) -> &mut IpcBytesSender {
        &mut self.0
    }
}

/// Payload used to initialize an [`ipc-channel`]-based child process
#[derive(Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub struct IpcChannelChildInit {
    /// ID of the parent process
    parent_id: String,
    /// Name of the [`IPCOneshotServer`] that should be used
    ipc_server_name: String,
}

impl IpcChannelChildInit {
    /// Create a new [`IpcChannelChildInit`]
    pub fn new(parent_id: impl AsRef<str>, ipc_server_name: impl AsRef<str>) -> Self {
        Self {
            parent_id: parent_id.as_ref().into(),
            ipc_server_name: ipc_server_name.as_ref().into(),
        }
    }

    /// Retrieve the parent process ID
    ///
    /// Note that this is *not* the platform-specific PID
    #[must_use]
    pub fn parent_id(&self) -> &str {
        &self.parent_id
    }

    /// Retrieve the name of the IPC server that should be used
    #[must_use]
    pub fn ipc_server_name(&mut self) -> &str {
        &self.ipc_server_name
    }
}

/// Payload sent in response to a [`IpcChannelChildInit`] in order to establish
/// bi-directional comms with a given subprocess (from the parent)
#[derive(Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub struct IpcChannelChildInitResponse {
    /// ID of the parent process
    parent_id: String,
    /// ID of the child process (sending the response)
    child_id: String,
    /// IPC server name that should be used to send (from the parent)
    ipc_server_name: String,
}

impl IpcChannelChildInitResponse {
    /// Create a new [`IpcChannelChildInitResponse`]
    pub fn new(child_id: &str, parent_id: &str, ipc_server_name: &str) -> Self {
        Self {
            parent_id: parent_id.into(),
            child_id: child_id.into(),
            ipc_server_name: ipc_server_name.into(),
        }
    }

    /// Retrieve the parent process ID
    ///
    /// Note that this is *not* the platform-specific PID
    #[must_use]
    pub fn parent_id(&self) -> &str {
        &self.parent_id
    }

    /// Retrieve the child process ID
    ///
    /// Note that this is *not* the platform-specific PID
    #[must_use]
    pub fn child_id(&self) -> &str {
        &self.child_id
    }

    /// Retrieve the IPC server name to use
    #[must_use]
    pub fn ipc_server_name(&self) -> &str {
        &self.ipc_server_name
    }
}

/// Message that indicates IPC channel setup complete between parent & child
///
/// This message is sent *last*, on a channel created by the child and sent
/// to the parent.
#[derive(Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub struct IpcChannelInitComplete {
    /// ID of the parent
    parent_id: String,
    /// ID of the child
    child_id: String,
}

impl IpcChannelInitComplete {
    /// Create a new [`IpcChannelInitComplete`]
    pub fn new(parent_id: &str, child_id: &str) -> Self {
        Self {
            parent_id: parent_id.into(),
            child_id: child_id.into(),
        }
    }

    /// Retrieve the parent process ID
    ///
    /// Note that this is *not* the platform-specific PID
    #[must_use]
    pub fn parent_id(&self) -> &str {
        &self.parent_id
    }

    /// Retrieve the child process ID
    ///
    /// Note that this is *not* the platform-specific PID
    #[must_use]
    pub fn child_id(&self) -> &str {
        &self.child_id
    }
}

/// Retrieve current system time as milliseconds since the UNIX epoch
pub fn get_system_time_millis() -> Result<u128> {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|v| v.as_millis())
        .context("failed to retrieve system time")
}
