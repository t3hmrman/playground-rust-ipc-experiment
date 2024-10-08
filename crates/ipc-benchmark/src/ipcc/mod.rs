/*!
Implementation of IPC between parent and child processes via [`ipc-channel`].

`ipc-channel` seems to be a robust IPC mechanism, with support for Linux, Mac, and Windows environments.
*/

pub mod child;
pub mod parent;
