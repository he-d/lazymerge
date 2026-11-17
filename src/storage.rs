//! Native folder comparison and guarded, atomic file writes.
use crate::model::{Document, Session, Status, compare_bytes};
use anyhow::{Context, Result, bail, ensure};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use std::{
    collections::BTreeMap,
    fs::{self, Metadata, Permissions},
    hash::{DefaultHasher, Hash, Hasher},
    io::{ErrorKind, Write},
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct Roots {
    pub paths: [PathBuf; 2],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub path: PathBuf,
    pub is_dir: bool,
    pub present: [bool; 2],
    pub sizes: [Option<u64>; 2],
    pub status: Status,
    pub error: Option<String>,
}

#[derive(Debug)]
pub struct CopyPlan {
    pub count: usize,
    pub to: usize,
    pub selected: PathBuf,
    files: Vec<CopyFile>,
}

#[derive(Debug)]
struct CopyFile {
    path: PathBuf,
    source: Fingerprint,
    target: Option<Fingerprint>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Fingerprint {
    size: usize,
    hash: u64,
}

impl Fingerprint {
    fn of(bytes: &[u8]) -> Self {
        let mut hasher = DefaultHasher::new();
        bytes.hash(&mut hasher);
        Self {
            size: bytes.len(),
            hash: hasher.finish(),
        }
    }
}

impl Roots {
    pub fn new(left: &Path, right: &Path) -> Result<Self> {
        let paths = [
            left.canonicalize()
                .with_context(|| format!("Cannot open left folder {}", left.display()))?,
            right
                .canonicalize()
                .with_context(|| format!("Cannot open right folder {}", right.display()))?,
        ];
        for path in &paths {
            ensure!(path.is_dir(), "{} is not a folder", path.display());
        }
        ensure!(
            !paths[0].starts_with(&paths[1]) && !paths[1].starts_with(&paths[0]),
            "Choose two separate folders: the folders must not be equal or contain each other"
        );
        Ok(Self { paths })
    }

    /// Validate every component before accessing it; symlinks are intentionally
    /// displayed as errors rather than followed outside the chosen folders.
    fn checked_path(&self, side: usize, relative: &Path) -> Result<PathBuf> {
        ensure!(side < 2, "Invalid side");
        ensure!(!relative.as_os_str().is_empty(), "A file path is required");
        let mut path = self.paths[side].clone();
        let root_meta = fs::symlink_metadata(&path)?;
        ensure!(
            root_meta.is_dir() && !root_meta.file_type().is_symlink(),
            "Folder root changed: {}",
            path.display()
        );
        let components: Vec<_> = relative.components().collect();
        for (i, component) in components.iter().enumerate() {
            let Component::Normal(name) = component else {
                bail!("Invalid relative path: {}", relative.display());
            };
            path.push(name);
            match fs::symlink_metadata(&path) {
                Ok(meta) => {
                    ensure!(
                        !meta.file_type().is_symlink(),
                        "Symbolic links are not followed: {}",
                        path.display()
                    );
                    if i + 1 < components.len() {
                        ensure!(meta.is_dir(), "File/folder conflict: {}", path.display());
                    } else {
                        ensure!(
                            meta.is_file() || meta.is_dir(),
                            "Unsupported file type: {}",
                            path.display()
                        );
                    }
                }
                Err(error) if error.kind() == ErrorKind::NotFound => (),
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("Cannot inspect {}", path.display()));
                }
            }
        }
        Ok(path)
    }

    fn read(&self, side: usize, relative: &Path) -> Result<Option<Vec<u8>>> {
        let path = self.checked_path(side, relative)?;
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| format!("Cannot read {}", path.display())),
        }
    }

    pub fn open(&self, path: &Path, ignore_ws: bool) -> Result<Session> {
        let bytes = [self.read(0, path)?, self.read(1, path)?];
        ensure!(
            bytes.iter().any(Option::is_some),
            "File no longer exists: {}",
            path.display()
        );
        let docs = bytes.map(Document::from_bytes);
        Ok(Session::new(path.to_path_buf(), docs, ignore_ws))
    }

    pub fn disk_changed(&self, session: &Session) -> [bool; 2] {
        std::array::from_fn(|side| {
            self.read(side, &session.path)
                .map(|bytes| bytes != session.docs[side].baseline)
                .unwrap_or(true)
        })
    }

    pub fn save(&self, session: &mut Session, side: usize, overwrite: bool) -> Result<()> {
        ensure!(side < 2, "Invalid side");
        let doc = &session.docs[side];
        ensure!(
            !doc.readonly && !doc.binary,
            "This encoding is read-only; copy the whole file from the folder tree instead"
        );
        if !doc.dirty() {
            return Ok(());
        }
        let current = self.read(side, &session.path)?;
        ensure!(
            overwrite || current == doc.baseline,
            "{} changed on disk; confirm before overwriting",
            session.path.display()
        );
        let other = session.docs[1 - side].clone();
        session.docs[side].adopt_format(&other);
        let bytes = session.docs[side].encode();
        let permissions = match self.existing_permissions(side, &session.path)? {
            Some(permissions) => Some(permissions),
            None => self.existing_permissions(1 - side, &session.path)?,
        };
        self.write(
            side,
            &session.path,
            &bytes,
            permissions,
            current.as_deref().map(Fingerprint::of),
        )?;
        let doc = &mut session.docs[side];
        doc.baseline = Some(bytes);
        doc.exists = true;
        session.stale[side] = false;
        Ok(())
    }

    fn existing_permissions(&self, side: usize, relative: &Path) -> Result<Option<Permissions>> {
        let path = self.checked_path(side, relative)?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                ensure!(
                    metadata.is_file(),
                    "File/folder conflict: {}",
                    path.display()
                );
                Ok(Some(metadata.permissions()))
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn write(
        &self,
        side: usize,
        relative: &Path,
        bytes: &[u8],
        permissions: Option<Permissions>,
        expected: Option<Fingerprint>,
    ) -> Result<()> {
        let destination = self.checked_path(side, relative)?;
        let parent = destination
            .parent()
            .context("File has no parent directory")?;
        let relative_parent = relative.parent().unwrap_or(Path::new(""));
        let mut parent_path = self.paths[side].clone();
        for component in relative_parent.components() {
            let Component::Normal(name) = component else {
                bail!("Invalid destination path")
            };
            parent_path.push(name);
            match fs::create_dir(&parent_path) {
                Ok(()) => (),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => (),
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("Cannot create {}", parent_path.display()));
                }
            }
            let meta = fs::symlink_metadata(&parent_path)?;
            ensure!(
                meta.is_dir() && !meta.file_type().is_symlink(),
                "Destination folder changed: {}",
                parent_path.display()
            );
        }
        self.checked_path(side, relative)?;
        let mut temp = tempfile::Builder::new()
            .prefix(".lazymerge-")
            .tempfile_in(parent)
            .with_context(|| format!("Cannot write to {}", parent.display()))?;
        temp.write_all(bytes)?;
        if let Some(permissions) = permissions {
            temp.as_file().set_permissions(permissions)?;
        }
        temp.as_file().sync_all()?;
        // Rename the completed file in the same directory, so readers cannot
        // observe a half-written file if writing fails.
        self.checked_path(side, relative)?;
        ensure!(
            self.read(side, relative)?.as_deref().map(Fingerprint::of) == expected,
            "Destination changed while saving: {}. Reload and try again",
            relative.display()
        );
        if expected.is_some() {
            temp.persist(&destination)
                .map_err(|error| error.error)
                .with_context(|| format!("Cannot save {}", destination.display()))?;
        } else {
            // An absent destination must remain absent right up to creation.
            temp.persist_noclobber(&destination)
                .map_err(|error| error.error)
                .with_context(|| format!("Cannot create {}", destination.display()))?;
        }
        Ok(())
    }

    pub fn copy_plan(&self, nodes: &[Node], selected: &Path, to: usize) -> Result<CopyPlan> {
        ensure!(to < 2, "Invalid side");
        let node = nodes
            .iter()
            .find(|node| node.path == selected)
            .context("Select a file or folder first")?;
        ensure!(
            node.present[1 - to],
            "The source side does not contain this file or folder"
        );
        ensure!(
            node.error.is_none(),
            "Cannot copy {}: {}",
            selected.display(),
            node.error.as_deref().unwrap_or("")
        );
        if node.is_dir {
            for child in nodes
                .iter()
                .filter(|child| child.path.starts_with(selected) && child.present[1 - to])
            {
                ensure!(
                    child.error.is_none(),
                    "Cannot copy {}: {}",
                    child.path.display(),
                    child.error.as_deref().unwrap_or("")
                );
            }
        }
        let mut files = Vec::new();
        for file in nodes
            .iter()
            .filter(|file| !file.is_dir && file.present[1 - to])
        {
            if node.is_dir {
                if !file.path.starts_with(selected) || !file.status.is_difference() {
                    continue;
                }
            } else if file.path != selected || matches!(file.status, Status::Same) {
                continue;
            }
            ensure!(
                file.error.is_none(),
                "Cannot copy {}: {}",
                file.path.display(),
                file.error.as_deref().unwrap_or("")
            );
            let source = self
                .read(1 - to, &file.path)?
                .with_context(|| format!("Source disappeared: {}", file.path.display()))?;
            let target = self.read(to, &file.path)?;
            files.push(CopyFile {
                path: file.path.clone(),
                source: Fingerprint::of(&source),
                target: target.as_deref().map(Fingerprint::of),
            });
        }
        Ok(CopyPlan {
            count: files.len(),
            to,
            selected: selected.to_path_buf(),
            files,
        })
    }

    pub fn execute_copy(&self, plan: CopyPlan) -> Result<usize> {
        // Preflight all files before starting, then check each again at writing.
        for file in &plan.files {
            self.validate_copy(file, plan.to)?;
        }
        let mut copied = 0;
        for file in plan.files {
            let source = self
                .validate_copy(&file, plan.to)
                .with_context(|| format!("Copied {copied} files before stopping"))?;
            let permissions = self
                .existing_permissions(plan.to, &file.path)?
                .or(self.existing_permissions(1 - plan.to, &file.path)?);
            self.write(plan.to, &file.path, &source, permissions, file.target)
                .with_context(|| format!("Copied {copied} files before stopping"))?;
            copied += 1;
        }
        Ok(copied)
    }

    fn validate_copy(&self, file: &CopyFile, to: usize) -> Result<Vec<u8>> {
        let source = self
            .read(1 - to, &file.path)?
            .with_context(|| format!("Source disappeared: {}", file.path.display()))?;
        let target = self.read(to, &file.path)?;
        ensure!(
            Fingerprint::of(&source) == file.source,
            "Source changed after confirmation was requested: {}. Reload and try again",
            file.path.display()
        );
        ensure!(
            target.as_deref().map(Fingerprint::of) == file.target,
            "Destination changed after confirmation was requested: {}. Reload and try again",
            file.path.display()
        );
        Ok(source)
    }
}

struct Patterns {
    set: GlobSet,
    empty: bool,
}

impl Patterns {
    fn new(input: &str) -> Result<Self> {
        let mut builder = GlobSetBuilder::new();
        let mut empty = true;
        for pattern in input
            .split([';', ','])
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let pattern = if pattern == "*.*" { "*" } else { pattern };
            builder.add(
                GlobBuilder::new(pattern)
                    .case_insensitive(true)
                    .literal_separator(false)
                    .build()
                    .with_context(|| format!("Invalid filter pattern {pattern:?}"))?,
            );
            empty = false;
        }
        Ok(Self {
            set: builder.build()?,
            empty,
        })
    }

    fn matches(&self, path: &Path) -> bool {
        self.set.is_match(path)
            || path
                .file_name()
                .is_some_and(|name| self.set.is_match(Path::new(name)))
    }
}

#[derive(Default)]
struct RawNode {
    meta: [Option<Metadata>; 2],
    errors: Vec<String>,
}

pub fn scan(roots: &Roots, include: &str, exclude: &str, ignore_ws: bool) -> Result<Vec<Node>> {
    let include = Patterns::new(include)?;
    let exclude = Patterns::new(exclude)?;
    let mut raw = BTreeMap::<PathBuf, RawNode>::new();
    for (side, root_path) in roots.paths.iter().enumerate() {
        let root = fs::symlink_metadata(root_path)?;
        ensure!(
            root.is_dir() && !root.file_type().is_symlink(),
            "Folder root changed: {}",
            root_path.display()
        );
        walk(root_path, Path::new(""), side, &include, &exclude, &mut raw)?;
    }
    let mut nodes = BTreeMap::new();
    for (path, raw) in raw {
        let present = raw.meta.each_ref().map(Option::is_some);
        let is_dir = raw.meta.iter().flatten().any(Metadata::is_dir);
        let mut errors = raw.errors;
        if let [Some(left), Some(right)] = &raw.meta
            && left.is_dir() != right.is_dir()
        {
            errors.push("A file on one side is a folder on the other".to_string());
        }
        let sizes = raw.meta.each_ref().map(|meta| {
            meta.as_ref()
                .filter(|meta| meta.is_file())
                .map(Metadata::len)
        });
        let status = if !errors.is_empty() {
            Status::Error
        } else if !present[1] {
            Status::LeftOnly
        } else if !present[0] {
            Status::RightOnly
        } else if is_dir {
            Status::Same
        } else {
            match (roots.read(0, &path), roots.read(1, &path)) {
                (Ok(Some(left)), Ok(Some(right))) => compare_bytes(&left, &right, ignore_ws),
                (left, right) => {
                    errors.push(format!(
                        "Cannot compare file: {}",
                        left.err()
                            .or_else(|| right.err())
                            .map_or_else(|| "file disappeared".to_string(), |e| format!("{e:#}"))
                    ));
                    Status::Error
                }
            }
        };
        nodes.insert(
            path.clone(),
            Node {
                path,
                is_dir,
                present,
                sizes,
                status,
                error: (!errors.is_empty()).then(|| errors.join("; ")),
            },
        );
    }
    // Aggregate bottom-up, preserving one-sided and error directory states.
    let mut paths: Vec<_> = nodes.keys().cloned().collect();
    paths.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for path in paths {
        let child = &nodes[&path];
        let child_status = child.status;
        if let Some(parent) = path.parent().and_then(|parent| nodes.get_mut(parent))
            && matches!(parent.status, Status::Same | Status::Minor)
        {
            if child_status.is_difference() {
                parent.status = Status::Different;
            } else if matches!(child_status, Status::Minor) {
                parent.status = Status::Minor;
            }
        }
    }
    let mut children: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for path in nodes.keys() {
        children
            .entry(path.parent().unwrap_or(Path::new("")).to_path_buf())
            .or_default()
            .push(path.clone());
    }
    for paths in children.values_mut() {
        paths.sort_by(|a, b| {
            nodes[b]
                .is_dir
                .cmp(&nodes[a].is_dir)
                .then_with(|| {
                    a.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_lowercase()
                        .cmp(
                            &b.file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .to_lowercase(),
                        )
                })
                .then_with(|| a.cmp(b))
        });
    }
    fn flatten(
        path: &Path,
        children: &BTreeMap<PathBuf, Vec<PathBuf>>,
        nodes: &mut BTreeMap<PathBuf, Node>,
        out: &mut Vec<Node>,
    ) {
        if let Some(paths) = children.get(path) {
            for path in paths {
                if let Some(node) = nodes.remove(path) {
                    out.push(node);
                }
                flatten(path, children, nodes, out);
            }
        }
    }
    let mut out = Vec::new();
    flatten(Path::new(""), &children, &mut nodes, &mut out);
    Ok(out)
}

fn walk(
    root: &Path,
    relative: &Path,
    side: usize,
    include: &Patterns,
    exclude: &Patterns,
    nodes: &mut BTreeMap<PathBuf, RawNode>,
) -> Result<()> {
    let entries = match fs::read_dir(root.join(relative)) {
        Ok(entries) => entries,
        Err(error) if !relative.as_os_str().is_empty() => {
            nodes
                .entry(relative.to_path_buf())
                .or_default()
                .errors
                .push(format!("Cannot read folder: {error}"));
            return Ok(());
        }
        Err(error) => {
            return Err(error).with_context(|| format!("Cannot read folder {}", root.display()));
        }
    };
    for entry in entries {
        let entry = entry
            .with_context(|| format!("Cannot read entry in {}", root.join(relative).display()))?;
        let path = relative.join(entry.file_name());
        if exclude.matches(&path) {
            continue;
        }
        let metadata = match fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(error) => {
                nodes
                    .entry(path)
                    .or_default()
                    .errors
                    .push(format!("Cannot read entry: {error}"));
                continue;
            }
        };
        if !metadata.is_dir() && !include.empty && !include.matches(&path) {
            continue;
        }
        let is_dir = metadata.is_dir();
        let node = nodes.entry(path.clone()).or_default();
        if metadata.file_type().is_symlink() {
            node.errors
                .push("Symbolic link skipped (links are never followed)".to_string());
        } else if !metadata.is_file() && !is_dir {
            node.errors
                .push("Unsupported file type skipped".to_string());
        }
        node.meta[side] = Some(metadata);
        if is_dir {
            walk(root, &path, side, include, exclude, nodes)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folders() -> (tempfile::TempDir, Roots) {
        let temp = tempfile::tempdir().unwrap();
        let left = temp.path().join("left");
        let right = temp.path().join("right");
        fs::create_dir(&left).unwrap();
        fs::create_dir(&right).unwrap();
        let roots = Roots::new(&left, &right).unwrap();
        (temp, roots)
    }

    #[test]
    fn compares_filters_and_aggregates_tree() {
        let (_temp, roots) = folders();
        fs::create_dir(roots.paths[0].join("nested")).unwrap();
        fs::create_dir(roots.paths[1].join("nested")).unwrap();
        fs::create_dir(roots.paths[0].join("node_modules")).unwrap();
        fs::write(roots.paths[0].join("node_modules/ignored.rs"), b"ignored").unwrap();
        fs::write(roots.paths[0].join("nested/a.RS"), b"a\r\n").unwrap();
        fs::write(roots.paths[1].join("nested/a.RS"), b"a\n").unwrap();
        fs::write(roots.paths[0].join("only.rs"), b"a").unwrap();
        fs::write(roots.paths[0].join("skip.txt"), b"a").unwrap();
        let nodes = scan(&roots, "*.rs", "node_modules", false).unwrap();
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[0].path, Path::new("nested"));
        assert!(matches!(nodes[0].status, Status::Minor));
        assert!(matches!(nodes[1].status, Status::Minor));
        assert!(matches!(nodes[2].status, Status::LeftOnly));
    }

    #[test]
    fn save_checks_external_changes_and_preserves_format() {
        let (_temp, roots) = folders();
        fs::write(roots.paths[0].join("file"), b"old\n").unwrap();
        fs::write(roots.paths[1].join("file"), b"\xef\xbb\xbfold\r\n").unwrap();
        let mut session = roots.open(Path::new("file"), false).unwrap();
        session.docs[1].lines[0] = "new".to_string();
        fs::write(roots.paths[1].join("file"), b"external\r\n").unwrap();
        assert_eq!(roots.disk_changed(&session), [false, true]);
        assert!(roots.save(&mut session, 1, false).is_err());
        assert_eq!(
            fs::read(roots.paths[1].join("file")).unwrap(),
            b"external\r\n"
        );
        roots.save(&mut session, 1, true).unwrap();
        assert_eq!(
            fs::read(roots.paths[1].join("file")).unwrap(),
            b"\xef\xbb\xbfnew\r\n"
        );
        assert!(!session.docs[1].dirty());
    }

    #[test]
    fn copy_is_raw_and_rejects_changed_destination() {
        let (_temp, roots) = folders();
        fs::write(roots.paths[0].join("binary"), [0, 1, 2]).unwrap();
        let nodes = scan(&roots, "*", "", false).unwrap();
        let plan = roots.copy_plan(&nodes, Path::new("binary"), 1).unwrap();
        assert_eq!(plan.count, 1);
        fs::write(roots.paths[1].join("binary"), b"external").unwrap();
        assert!(roots.execute_copy(plan).is_err());
        assert_eq!(
            fs::read(roots.paths[1].join("binary")).unwrap(),
            b"external"
        );
        let plan = roots
            .copy_plan(
                &scan(&roots, "*", "", false).unwrap(),
                Path::new("binary"),
                1,
            )
            .unwrap();
        roots.execute_copy(plan).unwrap();
        assert_eq!(fs::read(roots.paths[1].join("binary")).unwrap(), [0, 1, 2]);
    }

    #[test]
    fn folder_copy_preflights_every_file_before_writing() {
        let (_temp, roots) = folders();
        fs::create_dir(roots.paths[0].join("folder")).unwrap();
        fs::write(roots.paths[0].join("folder/first"), b"first").unwrap();
        fs::write(roots.paths[0].join("folder/second"), b"second").unwrap();
        let nodes = scan(&roots, "*", "", false).unwrap();
        let plan = roots.copy_plan(&nodes, Path::new("folder"), 1).unwrap();
        assert_eq!(plan.count, 2);
        fs::write(roots.paths[0].join("folder/second"), b"changed").unwrap();
        assert!(roots.execute_copy(plan).is_err());
        assert!(!roots.paths[1].join("folder/first").exists());
    }

    #[test]
    fn saving_a_missing_side_creates_parents_and_adopts_format() {
        let (_temp, roots) = folders();
        fs::create_dir(roots.paths[0].join("folder")).unwrap();
        fs::write(
            roots.paths[0].join("folder/file"),
            b"\xef\xbb\xbfsource\r\n",
        )
        .unwrap();
        let mut session = roots.open(Path::new("folder/file"), false).unwrap();
        session.merge(1, true, false, true).unwrap();
        assert!(session.docs[1].dirty());
        roots.save(&mut session, 1, false).unwrap();
        assert_eq!(
            fs::read(roots.paths[1].join("folder/file")).unwrap(),
            b"\xef\xbb\xbfsource\r\n"
        );
        assert!(!session.docs[1].dirty());
    }

    #[test]
    fn rejects_overlapping_roots_and_parent_escape() {
        let (_temp, roots) = folders();
        assert!(Roots::new(&roots.paths[0], &roots.paths[0]).is_err());
        fs::create_dir(roots.paths[0].join("nested")).unwrap();
        assert!(Roots::new(&roots.paths[0], &roots.paths[0].join("nested")).is_err());
        assert!(roots.open(Path::new("../outside"), false).is_err());
    }

    #[test]
    fn missing_empty_file_is_only_created_after_merge_and_save() {
        let (_temp, roots) = folders();
        fs::write(roots.paths[0].join("empty"), b"").unwrap();
        let mut session = roots.open(Path::new("empty"), false).unwrap();
        roots.save(&mut session, 1, false).unwrap();
        assert!(!roots.paths[1].join("empty").exists());
        session.merge(1, true, false, true).unwrap();
        assert!(session.docs[1].dirty());
        assert!(!roots.paths[1].join("empty").exists());
        roots.save(&mut session, 1, false).unwrap();
        assert_eq!(fs::read(roots.paths[1].join("empty")).unwrap(), b"");
        assert!(!session.docs[1].dirty());
    }

    #[test]
    fn legacy_source_merges_text_into_utf8_without_changing_source() {
        let (_temp, roots) = folders();
        let source = [0xff, 0xfe, b'A', 0, 0xac, 0x20, b'\r', 0, b'\n', 0];
        fs::write(roots.paths[0].join("file"), source).unwrap();
        fs::write(roots.paths[1].join("file"), b"\xef\xbb\xbfold\r\n").unwrap();
        let mut session = roots.open(Path::new("file"), false).unwrap();
        assert!(session.docs[0].readonly);
        session.merge(1, true, false, true).unwrap();
        roots.save(&mut session, 1, false).unwrap();
        assert_eq!(fs::read(roots.paths[0].join("file")).unwrap(), source);
        assert_eq!(
            fs::read(roots.paths[1].join("file")).unwrap(),
            "\u{feff}A€\r\n".as_bytes()
        );
        assert!(roots.save(&mut session, 0, false).is_err());
    }

    #[test]
    fn type_conflicts_are_errors_and_never_overwritten() {
        let (_temp, roots) = folders();
        fs::write(roots.paths[0].join("conflict"), b"file").unwrap();
        fs::create_dir(roots.paths[1].join("conflict")).unwrap();
        let nodes = scan(&roots, "*", "", false).unwrap();
        assert!(matches!(nodes[0].status, Status::Error));
        assert!(roots.copy_plan(&nodes, Path::new("conflict"), 1).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn saves_preserve_existing_or_inherit_source_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let (_temp, roots) = folders();
        fs::write(roots.paths[0].join("script"), b"#!/bin/sh\n").unwrap();
        fs::set_permissions(roots.paths[0].join("script"), Permissions::from_mode(0o755)).unwrap();
        let mut session = roots.open(Path::new("script"), false).unwrap();
        session.merge(1, true, false, true).unwrap();
        roots.save(&mut session, 1, false).unwrap();
        assert_eq!(
            fs::metadata(roots.paths[1].join("script"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        fs::set_permissions(roots.paths[1].join("script"), Permissions::from_mode(0o640)).unwrap();
        session.docs[1].lines.push("echo hello".to_string());
        roots.save(&mut session, 1, false).unwrap();
        assert_eq!(
            fs::metadata(roots.paths[1].join("script"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
    }

    #[cfg(unix)]
    #[test]
    fn save_rejects_a_destination_parent_replaced_by_symlink() {
        use std::os::unix::fs::symlink;
        let (temp, roots) = folders();
        fs::create_dir(roots.paths[0].join("nested")).unwrap();
        fs::write(roots.paths[0].join("nested/file"), b"source").unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("file"), b"untouched").unwrap();
        let mut session = roots.open(Path::new("nested/file"), false).unwrap();
        session.merge(1, true, false, true).unwrap();
        symlink(&outside, roots.paths[1].join("nested")).unwrap();
        assert!(roots.save(&mut session, 1, true).is_err());
        assert_eq!(fs::read(outside.join("file")).unwrap(), b"untouched");
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_visible_but_never_followed() {
        use std::os::unix::fs::symlink;
        let (temp, roots) = folders();
        let outside = temp.path().join("outside");
        fs::write(&outside, b"untouched").unwrap();
        symlink(&outside, roots.paths[1].join("linked")).unwrap();
        fs::write(roots.paths[0].join("linked"), b"replacement").unwrap();
        let nodes = scan(&roots, "*", "", false).unwrap();
        assert!(matches!(nodes[0].status, Status::Error));
        assert!(roots.open(Path::new("linked"), false).is_err());
        assert!(roots.copy_plan(&nodes, Path::new("linked"), 1).is_err());
        assert_eq!(fs::read(&outside).unwrap(), b"untouched");
    }
}
