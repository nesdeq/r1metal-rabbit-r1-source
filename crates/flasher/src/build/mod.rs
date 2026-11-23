//! The build: our kernel, the firmware against our own sysroot, the boot image that holds both with Rabbit's driver files, and the device tree overlay.

mod blobs;
mod devicetree;
mod kernel;
pub(crate) mod lima;
mod pack;
mod process;
mod sysroot;

pub use pack::Unflashed;

use crate::{Result, of, part, read, release, workspace, workspace_root};
use r1_image::bootimg::BootImage;
use std::fs;
use std::path::Path;

const FIRMWARE_TARGET: &str = "aarch64-unknown-linux-musl";
// Whether this computer can build: LLVM publishes the release that builds the kernel and the firmware's C for Linux on x86-64 and on arm64.
const BUILDS_HERE: bool = cfg!(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")));

// Writes bytes to a file, making the directory it lies in; the file takes its place once it is written whole.
fn write_out(out: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).map_err(|e| of(parent, e))?;
    }
    let partial = part(out);
    fs::write(&partial, bytes).map_err(|e| of(&partial, e))?;
    fs::rename(&partial, out).map_err(|e| of(out, e))
}

/// Builds everything and writes the boot image to `out` and the overlay to `overlay_out`, the firmware with its extras where `extras` says; Rabbit's files come out of the firmware in `stock`.
pub fn build(stock: &Path, out: &Path, overlay_out: &Path, extras: bool) -> Result<()> {
    if cfg!(target_os = "macos") {
        return lima::build(stock, out, overlay_out, extras);
    }
    if !BUILDS_HERE {
        return Err("r1 build needs Linux on x86-64 or arm64, for which LLVM publishes the release it builds with, or macOS, which builds in a Linux VM".into());
    }
    // Rabbit's files are read and changed first, so that a missing or other release stops the build before anything is built.
    let files = blobs::files(stock)?;
    let stock_boot = read(&stock.join(release::image(release::BOOT)))?;
    let trees = devicetree::rewritten(&BootImage::parse(&stock_boot)?.dtb, devicetree::base)?;
    let overlay = devicetree::rewritten(&read(&stock.join(release::image(release::DTBO)))?, devicetree::overlay)?;
    let workspace_root = &workspace_root();
    let kernel = kernel::build(workspace_root)?;
    sysroot::prepare(workspace_root)?;
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let target = workspace("target");
    // The kernel's LLVM compiles and archives the firmware's C; its Rust takes the workspace's flags and compiler wrapper and lands where init is read, whatever the shell sets.
    process::run(
        std::process::Command::new(cargo)
            .current_dir(workspace_root)
            .env("PATH", kernel::tools_first(workspace_root)?)
            .env_remove("RUSTFLAGS")
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .env_remove("RUSTC_WRAPPER")
            .args(["build", "--release", "--locked", "--package", "r1-impshell", "--target", FIRMWARE_TARGET])
            .args(if extras { &["--features", "extras"][..] } else { &[] })
            .arg("--target-dir")
            .arg(&target),
    )?;
    let init = read(&target.join(FIRMWARE_TARGET).join("release/init"))?;
    let image = pack::boot_image(&stock_boot, &kernel, &trees, &init, &files)?;
    write_out(out, &image)?;
    write_out(overlay_out, &overlay)?;
    println!("{}: {} bytes, kernel {} bytes, init {} bytes", out.display(), image.len(), kernel.len(), init.len());
    println!("{}: {} bytes", overlay_out.display(), overlay.len());
    for (name, bytes) in &files {
        println!("  /{}/{name}: {} bytes", r1_funcore::wifi::wmt::FIRMWARE, bytes.len());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // The value a script sets a name to on a line of its own.
    fn set<'a>(script: &'a str, name: &str) -> Option<&'a str> {
        script.lines().find_map(|line| line.strip_prefix(name)?.strip_prefix('='))
    }

    #[test]
    fn script_paths_match() {
        let setup = include_str!("../../../../scripts/setupcodebase.sh");
        let backup = include_str!("../../../../scripts/backupdependencies.sh");
        let both = [
            ("KERNEL", kernel::SOURCE),
            ("MODULES", kernel::MODULES),
            ("TOOLCHAIN_X64", kernel::TOOLCHAIN_X64),
            ("TOOLCHAIN_ARM64", kernel::TOOLCHAIN_ARM64),
            ("MUSL", sysroot::ARCHIVE),
        ];
        let setup_alone = [
            ("KERNEL_URL", kernel::UPSTREAM),
            ("MODULES_URL", kernel::MODULES_UPSTREAM),
            ("STOCK_IMAGES", r1_image::STOCK),
            ("VM", lima::VM),
        ];
        for (name, value) in both.into_iter().chain(setup_alone) {
            assert_eq!(set(setup, name), Some(value), "setupcodebase.sh");
        }
        for (name, value) in both {
            assert_eq!(set(backup, name), Some(value), "backupdependencies.sh");
        }
        let release = Path::new(r1_image::STOCK).parent().and_then(Path::to_str).expect("the release's directory");
        let zip = set(setup, "ZIP").expect("the release's archive");
        assert_eq!(set(backup, "STOCK_ZIP"), Some(format!("{release}/{zip}").as_str()), "beside its images");
    }
}
