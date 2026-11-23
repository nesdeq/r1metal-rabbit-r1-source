//! The build on macOS: the same build, run in the Linux of the Lima VM that `scripts/setupcodebase.sh` makes from `lima/r1metal.yaml`, which sees the workspace at its path on the Mac.

use super::process::run;
use crate::{Result, workspace_root};
use std::path::Path;
use std::process::Command;

// The VM's name.
pub(super) const VM: &str = "r1metal";

// The command that runs `r1` in the VM, started if it is not running, from the workspace, which the VM sees at its path on the Mac.
fn in_vm(root: &Path) -> Command {
    let mut command = Command::new("limactl");
    command.args(["shell", "--start", "--workdir"]).arg(root).args([VM, "cargo", "r1"]);
    command
}

// Runs `r1` with these words in the VM.
pub(crate) fn r1(words: &[&str]) -> Result<()> {
    run(in_vm(&workspace_root()).args(words))
}

// Builds in the VM into the paths given; each must lie in the workspace, the one directory of the Mac the VM sees.
pub(super) fn build(stock: &Path, out: &Path, overlay_out: &Path, extras: bool) -> Result<()> {
    let root = &workspace_root();
    let mut command = in_vm(root);
    command.arg("build");
    for (flag, path) in [("--stock", stock), ("--out", out), ("--overlay", overlay_out)] {
        let path = std::path::absolute(path)?;
        if !path.starts_with(root) {
            return Err(format!("{}: the VM sees only the workspace, {}", path.display(), root.display()).into());
        }
        command.arg(flag).arg(path);
    }
    if extras {
        command.arg("--enable-extras");
    }
    run(&mut command)
}
