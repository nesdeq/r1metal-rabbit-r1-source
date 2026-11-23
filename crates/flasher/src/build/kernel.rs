//! Our kernel: Rabbit's published tree at a pinned commit plus `kernel/patches`, Rabbit's WiFi drivers built in, compiled with LLVM's own release.

use super::process::{fetch_archive, run};
use crate::{Result, of, part};
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub(super) const UPSTREAM: &str = "https://github.com/rabbit-hmi-oss/android_kernel_rabbit_mt6765";
// Head of Rabbit's `r1` branch; `make olddefconfig` keeps all 4401 options of the rabbitOS v0.8.293 kernel.
const PIN: &str = "8167c8c1087f057d2ef302fc93b47554291687ec";
// Rabbit's kernel modules, whose WMT, WiFi adaptor and gen4m drivers the kernel's own build links into its tree and builds in.
pub(super) const MODULES_UPSTREAM: &str = "https://github.com/rabbit-hmi-oss/android_kernel_modules_rabbit_mt6765";
// Head of that repository's `r1` branch.
const MODULES_PIN: &str = "0afdf059a0dfa17becc4bbe47363280a42224c2f";
// Where the kernel's build looks for the modules: beside its tree, under this name.
pub(super) const MODULES: &str = "kernel/vendor/mediatek/kernel_modules";
const MODULE_PATCHES: &str = "kernel/module-patches";
// Where LLVM publishes its 23.1.3 release of clang, lld and its tools.
const TOOLCHAIN_RELEASE: &str = "https://github.com/llvm/llvm-project/releases/download/llvmorg-23.1.3";
// The release's archive for Linux on x86-64 and on arm64, and the checksum of each. The archive stays where it was fetched to, as musl's does; the release is unpacked beside it.
pub(super) const TOOLCHAIN_X64: &str = "kernel/toolchain/LLVM-23.1.3-Linux-X64.tar.zst";
const TOOLCHAIN_X64_SHA256: &str = "14d2f701eb68fb799001bdea6231048f3990690fa2f406d563555ff8f744daba";
pub(super) const TOOLCHAIN_ARM64: &str = "kernel/toolchain/LLVM-23.1.3-Linux-ARM64.tar.zst";
const TOOLCHAIN_ARM64_SHA256: &str = "1154f37ec15b5654fcb8cc3a8a2352a4cc4eef44bdff7ddd47fb10ea85c82f01";
// The archive for the CPU that builds, and its checksum.
const TOOLCHAIN_ARCHIVE: (&str, &str) = if cfg!(target_arch = "aarch64") {
    (TOOLCHAIN_ARM64, TOOLCHAIN_ARM64_SHA256)
} else {
    (TOOLCHAIN_X64, TOOLCHAIN_X64_SHA256)
};
const TOOLCHAIN: &str = "kernel/toolchain/llvm-23.1.3";
// The archive was packed with a 2 GiB window, which zstd opens only when told to.
const UNPACK: &str = "zstd -d --long=31";
pub(super) const SOURCE: &str = "kernel/src";
const OUT: &str = "kernel/out";
// The configuration of the stock kernel, as it reports it in /proc/config.gz.
const CONFIG: &str = "kernel/config";
// Where r1metal departs from the stock configuration, in the same syntax.
const CHANGES: &str = "kernel/config.changes";
const PATCHES: &str = "kernel/patches";
// Records the commit `kernel/src` stands at with which patch set on it, so that an unchanged set on a tree that still stands there is not applied again.
pub(super) const APPLIED: &str = "kernel/out/r1metal-patches";
const MODULES_APPLIED: &str = "kernel/out/r1metal-module-patches";
// Records which configuration `kernel/out` was set up from, so an unchanged one is not resolved again.
const CONFIGURED: &str = "kernel/out/r1metal-config";
// The compressed image as make names it, and where under the build's directory make leaves it.
const IMAGE_TARGET: &str = "Image.gz";
const IMAGE_DIRECTORY: &str = "arch/arm64/boot";
// Where git keeps a run of `am` that a patch has stopped.
const STOPPED_AM: &str = ".git/rebase-apply";
const MAKE_VARIABLES: [&str; 12] = [
    "ARCH=arm64",
    "CC=clang",
    "LD=ld.lld",
    "AR=llvm-ar",
    "NM=llvm-nm",
    "OBJCOPY=llvm-objcopy",
    "OBJDUMP=llvm-objdump",
    "STRIP=llvm-strip",
    "READELF=llvm-readelf",
    "CLANG_TRIPLE=aarch64-linux-gnu-",
    "CROSS_COMPILE=aarch64-linux-gnu-",
    "LLVM_IAS=1",
];
// The C flags the kernel takes besides its own: the device's CPU. `make` adds that the compiler records the workspace's path as none, where a file under it is named by its absolute path.
const KCFLAGS: &str = "-mcpu=cortex-a53";
// The patches' commits are r1metal's and unsigned, whatever the user's git is set to.
const GIT_SETTINGS: [&str; 6] =
    ["-c", "user.name=r1metal", "-c", "user.email=r1metal@localhost", "-c", "commit.gpgsign=false"];
// Shown in /proc/version instead of the building machine's user and host name.
const BUILDER: &str = "r1metal";
// Set for the build, though empty: without it the release gets a `+` for a tree that stands past its tag.
const LOCALVERSION: &str = "";
// What /proc/version tells as the build's number and time, asked of the workspace's git: the commits of its history and the time of the last, in UTC, as `date` writes a time.
const BUILD_NUMBER: [&str; 3] = ["rev-list", "--count", "HEAD"];
const BUILD_TIME: [&str; 4] = ["log", "-1", "--format=%cd", "--date=format-local:%a %b %e %H:%M:%S UTC %Y"];
// The scripts of the kernel's build sort and compare as in the C locale, whatever this computer's is.
const LOCALE: &str = "C";

fn git(source: &Path) -> Command {
    let mut command = Command::new("git");
    command.arg("-C").arg(source).args(GIT_SETTINGS);
    command
}

// Fetches LLVM's release for this CPU into `workspace` and unpacks it into `dir` unless it is there: under a name of its own, put in place once it is whole.
fn fetch_toolchain(dir: &Path, workspace: &Path) -> Result<()> {
    if dir.join("bin/clang").exists() {
        return Ok(());
    }
    let (archive, sha256) = (workspace.join(TOOLCHAIN_ARCHIVE.0), TOOLCHAIN_ARCHIVE.1);
    let name = archive.file_name().and_then(|name| name.to_str()).ok_or("the toolchain's archive has no name")?;
    fetch_archive(&format!("{TOOLCHAIN_RELEASE}/{name}"), sha256, &archive)?;
    eprintln!("unpacking {} into {}", archive.display(), dir.display());
    let partial = part(dir);
    if partial.exists() {
        fs::remove_dir_all(&partial).map_err(|e| of(&partial, e))?;
    }
    fs::create_dir_all(&partial).map_err(|e| of(&partial, e))?;
    let unpack = format!("--use-compress-program={UNPACK}");
    run(Command::new("tar").args([&unpack, "-xf"]).arg(archive).arg("-C").arg(&partial).arg("--strip-components=1"))?;
    if dir.exists() {
        fs::remove_dir_all(dir).map_err(|e| of(dir, e))?;
    }
    fs::rename(&partial, dir).map_err(|e| of(dir, e))
}

// The search path with the `bin` of LLVM's release under `workspace` first, so that its clang and tools are the ones a build finds.
pub(super) fn tools_first(workspace: &Path) -> Result<OsString> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut search = vec![workspace.join(TOOLCHAIN).join("bin")];
    search.extend(std::env::split_paths(&path));
    Ok(std::env::join_paths(search)?)
}

// Whether the repository in `source` holds a commit.
fn has(source: &Path, commit: &str) -> bool {
    let named = format!("{commit}^{{commit}}");
    let asked = git(source).args(["cat-file", "-e", &named]).stderr(Stdio::null()).status();
    asked.is_ok_and(|status| status.success())
}

// Fetches one commit of a repository into `source`, unless it is there; a repository that was begun earlier gets what its pin names now, and one begun anew drops `applied`, since it carries none of the patches that names.
fn fetch_source(source: &Path, upstream: &str, pin: &str, applied: &Path) -> Result<()> {
    let begun = source.join(".git").exists();
    if begun && has(source, pin) {
        return Ok(());
    }
    eprintln!("fetching {upstream} at {pin} into {}", source.display());
    if !begun {
        if applied.exists() {
            fs::remove_file(applied).map_err(|e| of(applied, e))?;
        }
        fs::create_dir_all(source).map_err(|e| of(source, e))?;
        run(git(source).arg("init").arg("-q"))?;
        run(git(source).args(["remote", "add", "origin", upstream]))?;
    }
    run(git(source).args(["fetch", "--depth", "1", "origin", pin]))?;
    if !begun {
        run(git(source).args(["checkout", "-q", "FETCH_HEAD"]))?;
    }
    Ok(())
}

// The patches in a directory, in the order of their names; none where there is no such directory.
fn patch_files(dir: &Path) -> Vec<PathBuf> {
    let entries = fs::read_dir(dir).into_iter().flatten().flatten();
    let mut files: Vec<PathBuf> =
        entries.map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "patch")).collect();
    files.sort();
    files
}

// The commit the repository in `source` stands at; none where git names none.
fn head(source: &Path) -> Option<String> {
    let named = git(source).args(["rev-parse", "HEAD"]).stderr(Stdio::null()).output().ok()?;
    named.status.success().then(|| String::from_utf8_lossy(&named.stdout).trim().to_string())
}

// Puts the repository in `source` at its pinned commit with every patch applied, unless `applied` says it already is and the repository still stands at the commit that left it at.
fn apply_patches(source: &Path, pin: &str, patches: &[PathBuf], applied: &Path) -> Result<()> {
    let mut wanted = String::from(pin);
    for patch in patches {
        wanted.push('\n');
        wanted.push_str(&fs::read_to_string(patch).map_err(|e| of(patch, e))?);
    }
    let marked = |head: &str| format!("{head}\n{wanted}");
    let stands = head(source).is_some_and(|head| fs::read_to_string(applied).is_ok_and(|have| have == marked(&head)));
    if stands {
        return Ok(());
    }
    eprintln!("putting {} patches on {pin} in {}", patches.len(), source.display());
    // A patch that did not apply leaves its run of `am` behind, and git begins no new one beside it.
    if source.join(STOPPED_AM).exists() {
        run(git(source).args(["am", "-q", "--abort"]))?;
    }
    run(git(source).args(["reset", "-q", "--hard", pin]))?;
    if !patches.is_empty() {
        run(git(source).args(["am", "-q"]).args(patches))?;
    }
    if let Some(parent) = applied.parent() {
        fs::create_dir_all(parent).map_err(|e| of(parent, e))?;
    }
    let head = head(source).ok_or_else(|| of(source, "git names no commit it stands at"))?;
    fs::write(applied, marked(&head)).map_err(|e| of(applied, e))
}

// The symbol a .config line sets or switches off.
fn symbol(line: &str) -> Option<&str> {
    match line.trim().strip_prefix("# CONFIG_") {
        Some(rest) => rest.strip_suffix(" is not set"),
        None => line.trim().strip_prefix("CONFIG_")?.split_once('=').map(|(name, _)| name),
    }
}

// `base` with every symbol that `changes` names taking its line from there.
fn merged(base: &str, changes: &str) -> String {
    let named: HashSet<&str> = changes.lines().filter_map(symbol).collect();
    let kept = base.lines().filter(|line| symbol(line).is_none_or(|name| !named.contains(name)));
    let added = changes.lines().filter(|line| symbol(line).is_some());
    kept.chain(added).flat_map(|line| [line, "\n"]).collect()
}

// What git tells of the workspace's history.
fn told(workspace: &Path, asked: &[&str]) -> Result<String> {
    let output = git(workspace).args(asked).env("TZ", "UTC").output().map_err(|e| format!("git: {e}"))?;
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git {}: {}", asked.join(" "), said.trim()).into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

// The build's number and time as /proc/version tells them: the same on every computer that builds the same commit, where the kernel's own are a count of this computer's builds and the time of this one.
fn stamp(workspace: &Path) -> Result<[(&'static str, String); 2]> {
    Ok([
        ("KBUILD_BUILD_VERSION", told(workspace, &BUILD_NUMBER)?),
        ("KBUILD_BUILD_TIMESTAMP", told(workspace, &BUILD_TIME)?),
    ])
}

fn make(workspace: &Path, stamp: &[(&str, String)], targets: &[&str]) -> Result<()> {
    let (source, out) = (workspace.join(SOURCE), workspace.join(OUT));
    let jobs = std::thread::available_parallelism().map_or(1, |n| n.get());
    run(Command::new("make")
        .env("PATH", tools_first(workspace)?)
        .env("KBUILD_BUILD_USER", BUILDER)
        .env("KBUILD_BUILD_HOST", BUILDER)
        .envs(stamp.iter().map(|(name, value)| (name, value)))
        .env("LOCALVERSION", LOCALVERSION)
        .env("LC_ALL", LOCALE)
        .arg("-C")
        .arg(source)
        .arg(format!("O={}", out.display()))
        .arg(format!("-j{jobs}"))
        .args(MAKE_VARIABLES)
        .arg(format!("KCFLAGS={KCFLAGS} -ffile-prefix-map={}/=", workspace.display()))
        .args(targets))
}

// Builds the kernel under `workspace` and returns the compressed image.
pub(super) fn build(workspace: &Path) -> Result<Vec<u8>> {
    let (toolchain, source, out) = (workspace.join(TOOLCHAIN), workspace.join(SOURCE), workspace.join(OUT));
    let modules = workspace.join(MODULES);
    let (applied, modules_applied) = (workspace.join(APPLIED), workspace.join(MODULES_APPLIED));
    let stamp = stamp(workspace)?;
    fetch_toolchain(&toolchain, workspace)?;
    fetch_source(&source, UPSTREAM, PIN, &applied)?;
    fetch_source(&modules, MODULES_UPSTREAM, MODULES_PIN, &modules_applied)?;
    apply_patches(&source, PIN, &patch_files(&workspace.join(PATCHES)), &applied)?;
    apply_patches(&modules, MODULES_PIN, &patch_files(&workspace.join(MODULE_PATCHES)), &modules_applied)?;
    fs::create_dir_all(&out).map_err(|e| of(&out, e))?;
    let text = |name: &str| fs::read_to_string(workspace.join(name)).map_err(|e| of(&workspace.join(name), e));
    let config = merged(&text(CONFIG)?, &text(CHANGES)?);
    let (configured, resolved) = (workspace.join(CONFIGURED), out.join(".config"));
    if !resolved.exists() || fs::read_to_string(&configured).ok().as_deref() != Some(config.as_str()) {
        fs::write(&resolved, &config).map_err(|e| of(&resolved, e))?;
        make(workspace, &stamp, &["olddefconfig"])?;
        fs::write(&configured, &config).map_err(|e| of(&configured, e))?;
    }
    make(workspace, &stamp, &[IMAGE_TARGET])?;
    crate::read(&out.join(IMAGE_DIRECTORY).join(IMAGE_TARGET))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Commits `text` as the one file of a repository and returns the commit's name.
    fn commit(repository: &Path, text: &str) -> String {
        fs::write(repository.join("file"), text).unwrap();
        run(git(repository).args(["add", "file"])).unwrap();
        run(git(repository).args(["commit", "-q", "-m", text.trim()])).unwrap();
        let named = git(repository).args(["rev-parse", "HEAD"]).output().unwrap();
        String::from_utf8(named.stdout).unwrap().trim().to_string()
    }

    #[test]
    fn fetch_and_patch() {
        let root = std::env::temp_dir().join(format!("r1-kernel-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (upstream, source, applied) = (root.join("upstream"), root.join("source"), root.join("applied"));
        // A git set to sign every commit by a signer that always fails, which the build's settings override.
        let signing = |repository: &Path| {
            for setting in [["commit.gpgsign", "true"], ["gpg.program", "false"]] {
                run(git(repository).arg("config").args(setting)).unwrap();
            }
        };
        fs::create_dir_all(&upstream).unwrap();
        run(git(&upstream).args(["init", "-q"])).unwrap();
        run(git(&upstream).args(["config", "uploadpack.allowAnySHA1InWant", "true"])).unwrap();
        signing(&upstream);
        let [first, second, third] = ["one\n", "two\n", "three\n"].map(|text| commit(&upstream, text));
        let patches = [root.join("0001-three.patch")];
        let made = git(&upstream).args(["format-patch", "-1", "--stdout", &third]).output().unwrap();
        fs::write(&patches[0], made.stdout).unwrap();
        let from = upstream.to_str().unwrap();
        fetch_source(&source, from, &first, &applied).unwrap();
        signing(&source);
        assert!(has(&source, &first) && !has(&source, &second), "one commit is fetched, and no other");
        assert_eq!(fs::read_to_string(source.join("file")).unwrap(), "one\n");
        assert!(apply_patches(&source, &first, &patches, &applied).is_err(), "the patch is for the second");
        assert!(source.join(STOPPED_AM).exists() && !applied.exists());
        fetch_source(&source, from, &second, &applied).unwrap();
        assert!(has(&source, &second), "a repository that is there gets the commit its pin names now");
        apply_patches(&source, &second, &patches, &applied).unwrap();
        assert_eq!(
            fs::read_to_string(source.join("file")).unwrap(),
            "three\n",
            "the run that stopped is out of the way"
        );
        fs::write(source.join("file"), "left alone\n").unwrap();
        apply_patches(&source, &second, &patches, &applied).unwrap();
        assert_eq!(
            fs::read_to_string(source.join("file")).unwrap(),
            "left alone\n",
            "an unchanged set is applied once"
        );
        run(git(&source).args(["reset", "-q", "--hard", &second])).unwrap();
        apply_patches(&source, &second, &patches, &applied).unwrap();
        assert_eq!(
            fs::read_to_string(source.join("file")).unwrap(),
            "three\n",
            "a tree that no longer stands where its patches left it gets them again, whatever its marker tells"
        );
        fs::remove_dir_all(&source).unwrap();
        fetch_source(&source, from, &second, &applied).unwrap();
        assert!(!applied.exists(), "a tree begun anew carries none of the patches its marker named");
        signing(&source);
        apply_patches(&source, &second, &patches, &applied).unwrap();
        assert_eq!(fs::read_to_string(source.join("file")).unwrap(), "three\n", "and gets them");
        assert_eq!(patch_files(&root).len(), 1);
        assert!(patch_files(&root.join("nowhere")).is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn config_lines_name_their_symbol() {
        assert_eq!(symbol("CONFIG_NR_CPUS=64"), Some("NR_CPUS"));
        assert_eq!(symbol("CONFIG_CMDLINE=\"a=b\""), Some("CMDLINE"));
        assert_eq!(symbol("# CONFIG_PSI is not set"), Some("PSI"));
        assert_eq!(symbol("# Timers subsystem"), None);
        assert_eq!(symbol(""), None);
    }

    #[test]
    fn config_changes_merge() {
        let stock =
            "#\n# General setup\n#\nCONFIG_NR_CPUS=64\nCONFIG_PSI=y\n# CONFIG_JUMP_LABEL is not set\nCONFIG_SMP=y\n";
        let changes = "# fewer cores\nCONFIG_NR_CPUS=8\n\n# CONFIG_PSI is not set\nCONFIG_JUMP_LABEL=y\nCONFIG_NEW=y\n";
        let expected = "#\n# General setup\n#\nCONFIG_SMP=y\nCONFIG_NR_CPUS=8\n# CONFIG_PSI is not set\nCONFIG_JUMP_LABEL=y\nCONFIG_NEW=y\n";
        assert_eq!(merged(stock, changes), expected);
        assert_eq!(merged(stock, ""), stock);
    }
}
