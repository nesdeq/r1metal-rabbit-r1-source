//! The LTE modem: the kernel's records, the file service and RPC it asks of the AP, its drives, AT commands, its network interface, the SIM's file, the record of attaches, and the modem's state machine.

pub mod at;
pub mod attaches;
pub mod ccci;
pub mod drives;
pub mod files;
pub mod link;
pub mod modem;
pub mod rpc;
pub mod sim;
pub mod stream;

/// Directory of the SIM's file, the attach record and the modem's NVRAM, under the data partition's private directory.
pub const DIRECTORY: &str = ".r1metal/cell";
