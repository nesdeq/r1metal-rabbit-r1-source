//! The entire userspace of an r1metal device, in one static binary run as /init.

mod about;
#[cfg(test)]
#[path = "../../funcore/src/base/alloc_count.rs"]
mod alloc_count;
mod aloud;
mod app;
mod backlog;
mod bell;
mod calls;
mod camera;
mod cell;
mod chat;
mod clock;
mod cost;
mod crew;
mod crypto;
mod display;
mod gallery;
mod input;
mod magic;
mod media;
mod memos;
mod net;
mod news;
mod openai;
mod pid1;
mod pokedex;
mod pool;
mod power;
mod router;
mod rundown;
mod signal;
mod sound;
mod storage;
mod sync;
mod sys;
mod timekeeper;
mod translator;
mod usb;
mod voiced;
mod web;
mod wifi;

fn main() {
    pid1::run()
}
