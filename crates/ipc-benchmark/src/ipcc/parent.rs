//! Parent-specific IPC implementation over [`ipc-channel`]

use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::str::FromStr;

use anyhow::{ensure, Context as _, Result};
use ipc_channel::ipc::{IpcOneShotServer, IpcReceiver, IpcSender};
use tracing::debug;
use uuid::{NoContext, Timestamp, Uuid};

use crate::{
    get_system_time_millis, ChildId, ChildName, IpcChannelChildInit, IpcChannelChildInitResponse,
    IpcChannelInitComplete, ParentProcess, PingMessage, Pinger, PongMessage, RawStringPongMessage,
    RpcMessageComplexity, RpcPong,
};

/// Map of child process IDs to IPC senders/receivers (i.e. a usable channel)
type ChildChannelMap = HashMap<ChildId, (IpcSender<Vec<u8>>, IpcReceiver<Vec<u8>>)>;

/// Contains the implementation of the [`ParentProcess`] trait over IPC (via `ipc-channel`)
///
/// This process uses [`ipc-channel`] for communication,
/// thus it expects to receive setup that matches that expectation
#[derive(Debug, Default)]
pub struct IpcChannelParent {
    /// UUID of the parent process
    ///
    /// Note this is *not* a platform-specific PID
    uuid: uuid::Uuid,

    /// Child processes, ordered by human readable name
    children_names: HashMap<ChildName, ChildId>,

    /// Child processes, ordered by human readable name
    children: ChildChannelMap,

    /// Complexity of RPC messages to send
    rpc_message_complexity: RpcMessageComplexity,
}

impl IpcChannelParent {
    /// Create a new [`IpcChannelParent`]
    pub fn new() -> Self {
        Self {
            uuid: Uuid::new_v7(Timestamp::now(NoContext)),
            children_names: HashMap::new(),
            children: HashMap::new(),
            rpc_message_complexity: RpcMessageComplexity::from_env_or_default(std::env::vars()),
        }
    }
}

impl ParentProcess for IpcChannelParent {
    /// Return a unique identifier for the process (UUID)
    fn id(&self) -> String {
        self.uuid.to_string()
    }

    /// Spawn child process
    fn spawn_child(
        &mut self,
        name: impl AsRef<str>,
        mut cmd: Command,
    ) -> Result<std::process::Child> {
        debug!("spawning child process...");
        let mut child = cmd
            .stdin(Stdio::piped())
            .spawn()
            .context("failed to spawn child process")?;

        // Create an IPC channel server, and server that listens...?
        debug!("creating server for IPC oneshot setup (parent->child)...");
        let (server, server_name) =
            IpcOneShotServer::<Vec<u8>>::new().context("failed to build IPC server")?;

        // Send information over STDIN
        debug!("sending init payload...");
        let mut child_stdin = child.stdin.take().context("failed to get child STDIN")?;
        let init_parent_id = self.id();
        let _ = std::thread::spawn(move || {
            child_stdin
                .write_all(
                    &serde_json::to_vec(&IpcChannelChildInit::new(init_parent_id, server_name))
                        .context("failed to serialize init payload")?,
                )
                .context("failed to write init payload to child stdin")?;
            child_stdin.flush().context("failed to flush stdin")?;
            Ok(()) as Result<()>
        })
        .join();

        debug!("completing oneshot server setup to child...");
        let (from_child_receiver, first_msg) = server
            .accept()
            .context("parent process server failed to accept bytes from child")?;

        debug!("receiving init response from child...");
        let init_resp = serde_json::from_slice::<IpcChannelChildInitResponse>(&first_msg)
            .context("failed to InitResponse from first child message")?;

        // Connect to the IPC channel created by the child, and send the init complete message
        debug!("sending init complete to child...");
        let sender = IpcSender::<Vec<u8>>::connect(init_resp.ipc_server_name.clone())
            .context("failed to connect to child IPC server from parent")?;

        sender
            .send(
                serde_json::to_vec(&IpcChannelInitComplete::new(
                    &self.id(),
                    &init_resp.child_id,
                ))
                .context("failed to serialize init complete message")?,
            )
            .context("failed to send init complete to child from parent")?;

        // Save all the information for this duplex connection
        let child_id = init_resp.child_id();
        self.children_names
            .insert(name.as_ref().into(), child_id.into());
        self.children
            .insert(child_id.into(), (sender, from_child_receiver));
        debug!("successfully set spawned & saved child");

        Ok(child)
    }
}

impl Pinger for IpcChannelParent {
    fn roundtrip_ping(&self, name: impl AsRef<str>) -> Result<()> {
        let name = name.as_ref();

        // Retrieve the child
        let child_id = self
            .children_names
            .get(name)
            .with_context(|| format!("failed to find child with name [{name}]"))?;
        let (sender, receiver) = self
            .children
            .get(child_id)
            .with_context(|| format!("failed to find sender for child w/ id [{child_id}]"))?;

        // Build payload, depending on message complexity
        let payload: Vec<u8> = match self.rpc_message_complexity {
            RpcMessageComplexity::RawString => format!("{}|{}|ping", self.id(), child_id).into(),
            RpcMessageComplexity::Json => serde_json::to_vec(&PingMessage::new(
                self.id(),
                child_id.into(),
                get_system_time_millis()?,
            ))
            .context("failed to serialize ping")?,
        };

        // Send ping payload
        sender
            .send(payload)
            .context("failed to send ping from parent")?;

        // Receive pong bytes
        let pong_bytes = receiver.recv().context("failed to receive ping")?;

        // Check the returned bytes (this is essentially "processing")
        // depending on complexity required
        match self.rpc_message_complexity {
            // If we were dealing with raw strings, then we can just check
            RpcMessageComplexity::RawString => {
                let pong_msg = RawStringPongMessage::from_str(
                    std::str::from_utf8(&pong_bytes)
                        .context("failed to parse pong message from pong bytes")?,
                )?;
                ensure!(
                    pong_msg.receiver_id() == self.id(),
                    "receiver_id is parent process"
                );
                ensure!(
                    pong_msg.sender_id() == child_id,
                    "sender_id is child process"
                );
            }
            RpcMessageComplexity::Json => {
                let pong_msg = serde_json::from_slice::<PongMessage>(&pong_bytes)
                    .context("failed to decode pong message")?;
                let pong_sender_id = pong_msg.sender_id();
                let pong_receiver_id = pong_msg.receiver_id();
                let parent_id = self.id();
                ensure!(
                    pong_sender_id == child_id,
                    "pong message sender_id [{pong_sender_id}] does not match child id [{child_id}]",
                );
                ensure!(
                    pong_receiver_id == parent_id,
                    "pong receiver_id [{pong_receiver_id}] should be parent ID [{parent_id}]"
                );
            }
        };

        Ok(())
    }
}
