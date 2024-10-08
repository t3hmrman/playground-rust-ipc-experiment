//! Child-specific IPC implementation over [`ipc-channel`]

use std::{
    io::{stdin, Read},
    str::FromStr,
};

use anyhow::{ensure, Context as _, Result};
use ipc_channel::ipc::{IpcOneShotServer, IpcSender};
use tracing::debug;
use uuid::{NoContext, Timestamp, Uuid};

use crate::{
    get_system_time_millis, ChildProcess, IpcChannelChildInit, IpcChannelChildInitResponse,
    IpcChannelInitComplete, PingMessage, PongMessage, RawStringPingMessage, RpcMessageComplexity,
    RpcPing,
};

/// Contains the implementation of the [`ChildProcess`] trait over IPC (via `ipc-channel`)
///
/// This process uses [`ipc-channel`] for communication,
/// thus it expects to receive setup that matches that expectation
#[derive(Debug)]
pub struct IpcChannelChild {
    /// UUID that identifies this child
    uuid: uuid::Uuid,

    /// Complexity that should be used for RPC messages
    ///
    /// NOTE: the parent that is sending should have an identical value set
    rpc_message_complexity: RpcMessageComplexity,
}

impl IpcChannelChild {
    /// Build a new [`IpcChannelChild`] with a random UUID
    pub fn new() -> Self {
        Self {
            uuid: Uuid::new_v7(Timestamp::now(NoContext)),
            rpc_message_complexity: RpcMessageComplexity::from_env_or_default(std::env::vars()),
        }
    }
}

impl Default for IpcChannelChild {
    fn default() -> Self {
        Self::new()
    }
}

impl ChildProcess for IpcChannelChild {
    /// Return a unique identifier for the process (UUID)
    fn id(&self) -> String {
        self.uuid.to_string()
    }

    /// Run the child process
    fn run(self) -> Result<()> {
        debug!("reading stdin for init payload...");
        // Parse out the spawn payload from bytes on STDIN
        let mut init_payload = {
            let mut buf = String::new();
            stdin()
                .read_to_string(&mut buf)
                .context("failed to read from STDIN")?;
            serde_json::from_str::<IpcChannelChildInit>(&buf)
                .context("failed to parse IPC init payload")?
        };
        let parent_id = init_payload.parent_id().to_string();

        debug!("creating server for IPC oneshot setup (child->parent)...");
        let (server, server_name) =
            IpcOneShotServer::<Vec<u8>>::new().context("failed to build IPC server")?;

        debug!(
            server_name = init_payload.ipc_server_name(),
            "sending init response payload..."
        );
        let sender = IpcSender::<Vec<u8>>::connect(init_payload.ipc_server_name().into())
            .context("failed to connect to parent [{parent_id}] IPC server from child")?;
        sender
            .send(
                serde_json::to_vec(&IpcChannelChildInitResponse::new(
                    &self.id(),
                    &parent_id,
                    &server_name,
                ))
                .context("failed to serialize child init response")?,
            )
            .context("failed to send child init response")?;

        debug!("listening for init complete from parent...");
        // Listen for init complete message
        let (from_parent_receiver, first_msg) = server
            .accept()
            .context("parent process server failed to accept bytes from child")?;
        let init_complete = serde_json::from_slice::<IpcChannelInitComplete>(&first_msg)
            .context("failed to convert")?;

        ensure!(init_complete.parent_id() == parent_id, "parent ID matchees");
        ensure!(init_complete.child_id() == self.id(), "child ID matchees");

        // Now that we're initialized, Run forever listening for messages and handling them
        debug!("starting forever listen loop...");
        loop {
            if let Ok(msg_bytes) = from_parent_receiver.recv() {
                // Handle the ping message
                let sender_id = match self.rpc_message_complexity {
                    RpcMessageComplexity::RawString => {
                        let ping_msg = RawStringPingMessage::from_str(
                            std::str::from_utf8(&msg_bytes)
                                .context("failed to convert incoming bytes to str")?,
                        )?;
                        ensure!(ping_msg.receiver_id() == self.id(), "invalid receiver ID");
                        ping_msg.sender_id().to_string()
                    }
                    RpcMessageComplexity::Json => {
                        let ping_msg = serde_json::from_slice::<PingMessage>(&msg_bytes)
                            .context("failed to parse ping msg in child")?;
                        ensure!(ping_msg.receiver_id() == self.id(), "invalid receiver ID");
                        ping_msg.sender_id().to_string()
                    }
                };

                // Send pong
                let pong_bytes: Vec<u8> = match self.rpc_message_complexity {
                    RpcMessageComplexity::RawString => {
                        format!("{}|{}|pong", self.id(), sender_id).into()
                    }
                    RpcMessageComplexity::Json => {
                        let pong_msg =
                            PongMessage::new(self.id(), sender_id, get_system_time_millis()?);
                        serde_json::to_vec(&pong_msg).context("failed to serialize pong message")?
                    }
                };

                sender
                    .send(pong_bytes)
                    .context("failed to send pong message")?;
            }
        }
    }
}
