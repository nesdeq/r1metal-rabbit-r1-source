use super::{files, safe};
use crate::log;
use r1_funcore::domain::backup::{self, Found, Step};
use r1_funcore::storage::{PRIVATE, RESTORE};
use std::fs::{self, File};
use std::io;
use std::path::Path;

fn remove(path: &Path) -> io::Result<()> {
    let removed = match fs::symlink_metadata(path) {
        Ok(entry) if entry.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) => Err(e),
    };
    match removed {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn move_to(from: &Path, to: &Path) -> io::Result<()> {
    if let Some(directory) = to.parent() {
        fs::create_dir_all(directory)?;
    }
    fs::rename(from, to)
}

fn run(root: &Path, steps: &[Step]) -> io::Result<()> {
    for step in steps {
        match step {
            Step::Remove(path) => remove(&root.join(path))?,
            Step::Move { from, to } => move_to(&root.join(from), &root.join(to))?,
            Step::Sync => File::open(root)?.sync_all()?,
            Step::Trim => files::trim(root),
        }
    }
    Ok(())
}

pub(super) fn stage(root: &Path) -> io::Result<()> {
    run(root, &backup::stage_steps())?;
    fs::create_dir(root.join(backup::staging()))?;
    for directory in backup::staged_enclosed() {
        safe::make_enclosed(root, &root.join(directory))?;
    }
    File::open(root)?.sync_all()
}

pub(super) fn commit(root: &Path) -> io::Result<()> {
    run(root, &backup::commit_steps())
}

// The confirmed files' entries that count as staged; none where there is no such directory.
fn names(directory: &Path) -> io::Result<Option<Vec<String>>> {
    let is_empty = |entry: &fs::DirEntry| fs::read_dir(entry.path()).is_ok_and(|mut entries| entries.next().is_none());
    let to_name = |entry: fs::DirEntry| {
        let name = entry.file_name().into_string().ok()?;
        backup::is_staged(&name, is_empty(&entry)).then_some(name)
    };
    match fs::read_dir(directory) {
        Ok(entries) => Ok(Some(entries.flatten().filter_map(to_name).collect())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn find(root: &Path) -> io::Result<Found> {
    Ok(Found {
        is_staging: root.join(backup::staging()).try_exists()?,
        staged: names(&root.join(RESTORE))?,
        has_private: root.join(PRIVATE).try_exists()?,
        ..Found::default()
    })
}

fn is_alike(root: &Path, directory: &Path) -> io::Result<Option<bool>> {
    match safe::is_alike(root, directory) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        told => told,
    }
}

pub(super) fn settle(root: &Path) -> io::Result<()> {
    let found = find(root)?;
    found.to_unconfirmed_line().iter().for_each(|line| log!("{line}"));
    run(root, &found.settle_steps())
}

pub(super) fn apply(root: &Path) -> io::Result<()> {
    let mut found = find(root)?;
    found.is_same_safe = is_alike(root, &root.join(RESTORE).join(PRIVATE))?;
    for entry in backup::device_entries() {
        if root.join(&entry).try_exists()? {
            found.own.push(entry);
        }
    }
    found.to_other_safe_line().iter().for_each(|line| log!("{line}"));
    run(root, &found.apply_steps())?;
    found.to_applied_line().iter().for_each(|line| log!("{line}"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::scratch;
    use super::*;

    fn tree(root: &Path, files: &[(&str, &str)]) {
        for (path, content) in files {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
    }

    fn listed(directory: &Path, prefix: &str, lines: &mut Vec<String>) {
        for entry in fs::read_dir(directory).unwrap().flatten() {
            let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
            if entry.path().is_dir() {
                listed(&entry.path(), &format!("{name}/"), lines);
            } else {
                lines.push(format!("{name} {}", fs::read_to_string(entry.path()).unwrap()));
            }
        }
    }

    fn lines(root: &Path) -> Vec<String> {
        let mut lines = Vec::new();
        listed(root, "", &mut lines);
        lines.sort();
        lines
    }

    #[test]
    fn steps_on_files() {
        let root = scratch("restore-steps");
        tree(&root, &[("camera/pictures/old.jpg", "old"), (".restore/camera/pictures/new.jpg", "new"), ("kept", "k")]);
        let steps = [
            Step::Remove("camera".into()),
            Step::Remove("camera".into()),
            Step::Remove("kept".into()),
            Step::Move { from: ".restore/camera".into(), to: "camera".into() },
            Step::Move { from: "camera/pictures/new.jpg".into(), to: "deep/in/new.jpg".into() },
            Step::Remove(".restore".into()),
            Step::Sync,
        ];
        run(&root, &steps).unwrap();
        assert_eq!(
            lines(&root),
            ["deep/in/new.jpg new"],
            "a tree or a file goes, gone already too; a move makes its way"
        );
        assert!(
            run(&root, &[Step::Move { from: "nowhere".into(), to: "anywhere".into() }]).is_err(),
            "what is to move is there, or the step fails"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn find_partition_state() {
        let root = scratch("restore-found");
        let found = find(&root).unwrap();
        assert_eq!((found.is_staging, found.staged, found.has_private), (false, None, false), "an empty partition");
        tree(
            &root,
            &[(".restore.new/camera/a.jpg", "a"), (".restore/chat/core.md", "c"), (".r1metal/openai/key", "k")],
        );
        fs::create_dir_all(root.join(".restore/replaced")).unwrap();
        let mut found = find(&root).unwrap();
        found.staged.as_mut().unwrap().sort();
        assert_eq!(
            (found.is_staging, found.staged.as_deref(), found.has_private),
            (true, Some(&["chat".to_string(), "replaced".to_string()][..]), true)
        );
        assert_eq!(is_alike(&root, &root.join(".restore/.r1metal")).unwrap(), None, "no staged private directory");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn apply_public_roots() {
        let root = scratch("restore-apply");
        tree(
            &root,
            &[
                ("camera/pictures/old.jpg", "old"),
                ("chat/core.md", "stays"),
                (".r1metal/openai/key", "own"),
                (".restore/camera/videos/new.mp4", "new"),
                (".restore/stray/x", "x"),
                (".restore.new/chat/core.md", "never confirmed"),
            ],
        );
        // As the staging makes a root the backup then holds nothing of.
        fs::create_dir_all(root.join(".restore/chat")).unwrap();
        settle(&root).unwrap();
        assert!(!root.join(".restore.new").exists(), "what nothing confirmed goes before the safe is looked at");
        apply(&root).unwrap();
        assert_eq!(
            lines(&root),
            [".r1metal/openai/key own", "camera/videos/new.mp4 new", "chat/core.md stays"],
            "a root the restore holds takes its root's place whole, one it holds none of stays, and a name no backup holds goes"
        );
        apply(&root).unwrap();
        assert_eq!(lines(&root).len(), 3, "with no restore confirmed, nothing is touched");
        fs::remove_dir_all(root).unwrap();
    }
}
