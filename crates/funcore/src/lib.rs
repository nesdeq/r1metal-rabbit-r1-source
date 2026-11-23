//! r1metal's platform-free core: interface, formats, protocols and register sequences, no OS calls.

#![no_std]
#![cfg_attr(not(test), deny(unsafe_code))]

extern crate alloc;

pub mod base;
pub mod cell;
pub mod codec;
pub mod crypto;
pub mod domain;
#[cfg(test)]
mod fixtures;
pub mod fs;
pub mod gfx;
pub mod hw;
pub mod input;
pub mod logs;
pub mod net;
pub mod openai;
pub mod signal;
pub mod storage;
pub mod ui;
pub mod wifi;

pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");
