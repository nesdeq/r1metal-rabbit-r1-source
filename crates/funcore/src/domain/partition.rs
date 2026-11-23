//! What the data partition holds for the parts above storage.

use super::uses;
use crate::openai::key;
use crate::signal::device;
use crate::storage::CHAT_PRIVATE;

/// Directories of the private directory, made once the safe is open.
pub const PRIVATE_DIRECTORIES: [&str; 7] = [
    key::DIRECTORY,
    uses::DIRECTORY,
    CHAT_PRIVATE,
    device::DIRECTORY,
    device::SESSIONS,
    device::SENDER_KEYS,
    device::CONVERSATIONS,
];
