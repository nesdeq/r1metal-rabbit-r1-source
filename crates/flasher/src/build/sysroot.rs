//! What the firmware's C code is compiled against: the headers of musl, the C library the Rust target links, and those of our kernel.

use super::kernel;
use super::process::{checksum, fetch_archive, run};
use crate::{Result, of};
use std::fs;
use std::path::Path;
use std::process::Command;

// The release of musl that the Rust target `aarch64-unknown-linux-musl` brings as its `libc.a`, and the checksum of its archive.
const MUSL_URL: &str = "https://musl.libc.org/releases/musl-1.2.5.tar.gz";
const MUSL_SHA256: &str = "a9a118bbe84d8764da0ea0d28b3ab3fae8477fc7e4085d90102b8596fc7c75e4";
// The archive stays where it was fetched to, so that a backup can keep it; its source is unpacked beside it.
pub(super) const ARCHIVE: &str = "kernel/toolchain/musl-1.2.5.tar.gz";
const SOURCE: &str = "kernel/toolchain/musl-1.2.5";
// Where `tools/cc-aarch64-musl` has the compiler look for headers.
const SYSROOT: &str = "kernel/toolchain/sysroot";
// Where the kernel's build prepares the headers it installs.
const STAGING: &str = "kernel/out/headers";
// Records what the sysroot was made from, so that it is made once.
const MADE: &str = "kernel/toolchain/sysroot/r1metal-made";

// Makes the sysroot under `workspace` unless it is there: musl's headers for the device's architecture, and those the kernel source with our patches gives programs.
pub(super) fn prepare(workspace: &Path) -> Result<()> {
    let (archive, source, sysroot) = (workspace.join(ARCHIVE), workspace.join(SOURCE), workspace.join(SYSROOT));
    // The kernel's part is named by its pin, every patch on it and the commit they left it at: a patch may change a header programs see.
    let made = format!("{MUSL_SHA256}\n{}\n", checksum(&workspace.join(kernel::APPLIED))?);
    if fs::read_to_string(workspace.join(MADE)).is_ok_and(|have| have == made) {
        return Ok(());
    }
    fetch_archive(MUSL_URL, MUSL_SHA256, &archive)?;
    eprintln!("making the headers of musl and of the kernel in {}", sysroot.display());
    for directory in [&source, &sysroot] {
        if directory.exists() {
            fs::remove_dir_all(directory).map_err(|e| of(directory, e))?;
        }
        fs::create_dir_all(directory).map_err(|e| of(directory, e))?;
    }
    run(Command::new("tar").arg("-xzf").arg(&archive).arg("-C").arg(&source).arg("--strip-components=1"))?;
    let destination = format!("DESTDIR={}", sysroot.display());
    let musl = ["ARCH=aarch64", "prefix=/usr", &destination, "install-headers"];
    run(Command::new("make").args(["-s", "-C"]).arg(&source).args(musl))?;
    let staging = workspace.join(STAGING);
    fs::create_dir_all(&staging).map_err(|e| of(&staging, e))?;
    let out = format!("O={}", staging.display());
    let installed = format!("INSTALL_HDR_PATH={}", sysroot.join("usr").display());
    let headers = [&out, "ARCH=arm64", &installed, "headers_install"];
    run(Command::new("make").args(["-s", "-C"]).arg(workspace.join(kernel::SOURCE)).args(headers))?;
    let record = workspace.join(MADE);
    fs::write(&record, made).map_err(|e| of(&record, e))
}
