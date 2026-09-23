use crate::{Result, envfile};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct Project {
    pub name: String,
    pub files: PathBuf,
}

#[derive(Clone, Debug)]
pub struct Repo {
    pub root: PathBuf,
    pub project: Project,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Source {
    #[default]
    Auto,
    Local,
    Shared,
}

fn source_preferences_path(project: &Project) -> Result<PathBuf> {
    Ok(project
        .files
        .parent()
        .ok_or("project store has no parent")?
        .join("ui-sources"))
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex_bytes(text: &str) -> Result<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err("invalid UI source preferences".into());
    }
    bytes
        .chunks_exact(2)
        .map(|pair| {
            u8::from_str_radix(
                std::str::from_utf8(pair).map_err(|_| "invalid UI source preferences")?,
                16,
            )
            .map_err(|_| "invalid UI source preferences".into())
        })
        .collect()
}

fn source_records(project: &Project) -> Result<Vec<(PathBuf, PathBuf, Source)>> {
    let bytes = match fs::read(source_preferences_path(project)?) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("cannot read UI source preferences: {error}")),
    };
    let text = String::from_utf8(bytes).map_err(|_| "invalid UI source preferences")?;
    text.lines()
        .map(|line| {
            let mut fields = line.split('\t');
            let root = fields.next().ok_or("invalid UI source preferences")?;
            let relative = fields.next().ok_or("invalid UI source preferences")?;
            let source = match fields.next() {
                Some("L") => Source::Local,
                Some("S") => Source::Shared,
                _ => return Err("invalid UI source preferences".into()),
            };
            if fields.next().is_some() {
                return Err("invalid UI source preferences".into());
            }
            let root = PathBuf::from(OsString::from_vec(unhex_bytes(root)?));
            let relative = PathBuf::from(OsString::from_vec(unhex_bytes(relative)?));
            if !root.is_absolute() || !is_env_path(&relative) {
                return Err("invalid UI source preferences".into());
            }
            Ok((root, relative, source))
        })
        .collect()
}

pub fn load_ui_sources(repo: &Repo) -> Result<BTreeMap<PathBuf, Source>> {
    Ok(source_records(&repo.project)?
        .into_iter()
        .filter(|(root, _, _)| *root == repo.root)
        .map(|(_, relative, source)| (relative, source))
        .collect())
}

pub fn save_ui_source(repo: &Repo, relative: &Path, source: Source) -> Result<()> {
    if !is_env_path(relative) {
        return Err("invalid env path".into());
    }
    let _lock = project_lock(&repo.project)?;
    let mut records = source_records(&repo.project)?;
    records.retain(|(root, file, _)| root != &repo.root || file != relative);
    if source != Source::Auto {
        records.push((repo.root.clone(), relative.to_path_buf(), source));
    }
    records.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    let mut text = String::new();
    for (root, file, source) in records {
        let code = match source {
            Source::Local => 'L',
            Source::Shared => 'S',
            Source::Auto => unreachable!(),
        };
        text.push_str(&format!(
            "{}\t{}\t{code}\n",
            hex_bytes(root.as_os_str().as_bytes()),
            hex_bytes(file.as_os_str().as_bytes())
        ));
    }
    write_private_atomic(&source_preferences_path(&repo.project)?, text.as_bytes())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedFile {
    pub path: PathBuf,
    pub shared: bool,
}

impl ResolvedFile {
    pub fn label(&self) -> &'static str {
        if self.shared { "shared" } else { "local" }
    }
}

// All worktree mutations use this check, including paths supplied by the TUI.
// Reject directory symlinks rather than writing through them to another location.
fn destination(root: &Path, relative: &Path) -> Result<PathBuf> {
    if !is_env_path(relative) {
        return Err("invalid env path".into());
    }
    let mut parent = root.to_path_buf();
    for component in relative.parent().unwrap().components() {
        parent.push(component);
        match fs::symlink_metadata(&parent) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => return Err(format!("unsafe parent directory: {}", parent.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(format!("cannot inspect parent directory: {error}")),
        }
    }
    Ok(root.join(relative))
}

pub fn resolve_file(repo: &Repo, relative: &Path, source: Source) -> Result<ResolvedFile> {
    let local = destination(&repo.root, relative)?;
    let shared = managed_file(&repo.project, relative).ok();
    if source == Source::Shared {
        return shared
            .map(|path| ResolvedFile { path, shared: true })
            .ok_or_else(|| format!("{} is not managed", relative.display()));
    }
    if let Some(target) = &shared
        && is_exact_link(&local, target)
    {
        return Ok(ResolvedFile {
            path: target.clone(),
            shared: true,
        });
    }
    match fs::symlink_metadata(&local) {
        Ok(meta) if meta.file_type().is_file() => {
            if shared.is_some() && source == Source::Auto {
                return Err(format!(
                    "{} has both local and shared copies; choose --source local or --source shared (v in the TUI)",
                    relative.display()
                ));
            }
            Ok(ResolvedFile {
                path: local,
                shared: false,
            })
        }
        Ok(_) => Err(format!(
            "{} is not a regular local file or an envrun link",
            relative.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && source != Source::Local => {
            shared
                .map(|path| ResolvedFile { path, shared: true })
                .ok_or_else(|| {
                    format!(
                        "{} does not exist locally or in the store",
                        relative.display()
                    )
                })
        }
        Err(error) => Err(format!("cannot read {}: {error}", relative.display())),
    }
}

pub fn edit_file(
    repo: &Repo,
    relative: &Path,
    source: Source,
    key: &str,
    value: Option<&str>,
) -> Result<()> {
    let file = resolve_file(repo, relative, source)?;
    let before = fs::read(&file.path).map_err(|e| e.to_string())?;
    let after = envfile::edit(&before, key, value)?;
    update_file(repo, relative, source, &file, &before, &after)
}

pub fn update_file(
    repo: &Repo,
    relative: &Path,
    source: Source,
    expected: &ResolvedFile,
    before: &[u8],
    after: &[u8],
) -> Result<()> {
    let _lock = project_lock(&repo.project)?;
    let current = resolve_file(repo, relative, source)?;
    if current != *expected || fs::read(&current.path).map_err(|e| e.to_string())? != before {
        return Err("env file changed; reload before saving".into());
    }
    envfile::read_vars_bytes(after)?;
    write_private_atomic(&current.path, after)
}

fn git(cwd: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(format!("git {} failed", args.join(" ")))
    }
}

pub fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = env::var_os("XDG_CONFIG_HOME") {
        let path = PathBuf::from(dir);
        if !path.is_absolute() {
            return Err("XDG_CONFIG_HOME must be an absolute path".into());
        }
        return Ok(path.join("envrun"));
    }
    let home = env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home).join(".config/envrun"))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub fn project(name: &str) -> Result<Project> {
    if !valid_name(name) {
        return Err("project name must contain only letters, digits, '-' or '_'".into());
    }
    let files = config_dir()?.join("projects").join(name).join("files");
    Ok(Project {
        name: name.to_owned(),
        files,
    })
}

pub fn projects() -> Result<Vec<Project>> {
    let base = config_dir()?.join("projects");
    if !base.exists() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    for entry in fs::read_dir(base).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if let Some(name) = entry.file_name().to_str()
            && valid_name(name)
            && entry.path().join("files").is_dir()
        {
            result.push(project(name)?);
        }
    }
    result.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(result)
}

pub fn repo_root(cwd: &Path) -> Result<PathBuf> {
    let bytes = git(cwd, &["rev-parse", "--show-toplevel"])?;
    let root = String::from_utf8(bytes).map_err(|_| "Git root is not UTF-8")?;
    fs::canonicalize(root.strip_suffix('\n').unwrap_or(&root))
        .map_err(|e| format!("cannot resolve Git root: {e}"))
}

fn git_common_dir(cwd: &Path) -> Result<PathBuf> {
    let mut bytes = git(
        cwd,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    while bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    fs::canonicalize(PathBuf::from(OsString::from_vec(bytes)))
        .map_err(|e| format!("cannot resolve Git common directory: {e}"))
}

fn repo_id(cwd: &Path) -> Result<String> {
    if let Ok(existing) = git(cwd, &["config", "--local", "--get", "envrun.repository-id"]) {
        let id = String::from_utf8(existing).map_err(|_| "repository ID is not UTF-8")?;
        let id = id.trim_end_matches('\n');
        if id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Ok(id.to_owned());
        }
        return Err("invalid envrun repository ID in local Git config".into());
    }
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|e| format!("cannot create repository ID: {e}"))?;
    let id = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    git(cwd, &["config", "--local", "envrun.repository-id", &id])?;
    Ok(id)
}

fn registration_dir(project: &Project) -> Result<PathBuf> {
    Ok(project
        .files
        .parent()
        .ok_or("project store has no parent")?
        .join("repositories"))
}

fn project_lock(project: &Project) -> Result<File> {
    let path = project
        .files
        .parent()
        .ok_or("project store has no parent")?
        .join("project.lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    lock.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    lock.lock().map_err(|e| e.to_string())?;
    Ok(lock)
}

fn register_repo(project: &Project, cwd: &Path) -> Result<()> {
    let common = git_common_dir(cwd)?;
    let directory = registration_dir(project)?;
    let bytes = common.as_os_str().as_bytes();
    if let Ok(existing) = git(cwd, &["config", "--local", "--get", "envrun.repository-id"])
        && let Ok(id) = String::from_utf8(existing)
    {
        let id = id.trim_end_matches('\n');
        if id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            let path = directory.join(id);
            if fs::read(path).is_ok_and(|stored| stored == bytes) {
                return Ok(());
            }
        }
    }
    let _lock = project_lock(project)?;
    let id = repo_id(cwd)?;
    private_dir(&directory)?;
    let path = directory.join(id);
    if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file())
        && fs::read(&path).map_err(|e| e.to_string())? == bytes
    {
        return Ok(());
    }
    write_private_atomic(&path, bytes)
}

pub fn repositories(project: &Project) -> Result<Vec<(String, PathBuf)>> {
    let mut result = Vec::new();
    for entry in fs::read_dir(registration_dir(project)?).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let id = entry
            .file_name()
            .into_string()
            .map_err(|_| "invalid repository registration")?;
        if id.len() != 32
            || !id.bytes().all(|c| c.is_ascii_hexdigit())
            || !entry.file_type().map_err(|e| e.to_string())?.is_file()
        {
            return Err("invalid repository registration".into());
        }
        let bytes = fs::read(entry.path()).map_err(|e| e.to_string())?;
        result.push((id, PathBuf::from(OsString::from_vec(bytes))));
    }
    result.sort();
    Ok(result)
}

pub fn forget_repository(project: &Project, id: &str) -> Result<()> {
    let _lock = project_lock(project)?;
    let (_, common) = repositories(project)?
        .into_iter()
        .find(|(candidate, _)| candidate == id)
        .ok_or("unknown repository ID; run envrun repositories")?;
    match fs::symlink_metadata(&common) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("cannot verify missing repository: {error}")),
        Ok(_) => {
            return Err("repository still exists; its registration cannot be forgotten".into());
        }
    }
    fs::remove_file(registration_dir(project)?.join(id)).map_err(|e| e.to_string())
}

fn worktree_roots(common: &Path) -> Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .arg("--git-dir")
        .arg(common)
        .args(["worktree", "list", "--porcelain", "-z"])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!("cannot list worktrees for {}", common.display()));
    }
    let mut roots = Vec::new();
    for path in output
        .stdout
        .split(|byte| *byte == 0)
        .filter_map(|record| record.strip_prefix(b"worktree "))
    {
        let root = PathBuf::from(OsString::from_vec(path.to_vec()));
        let root = fs::canonicalize(&root)
            .map_err(|e| format!("worktree is inaccessible: {}: {e}", root.display()))?;
        roots.push(root);
    }
    if roots.is_empty() {
        return Err("Git returned no worktrees".into());
    }
    Ok(roots)
}

pub fn current_repo(cwd: &Path) -> Result<Repo> {
    let repo = unregistered_repo(cwd)?;
    register_repo(&repo.project, cwd)?;
    Ok(repo)
}

fn unregistered_repo(cwd: &Path) -> Result<Repo> {
    let root = repo_root(cwd)?;
    let name = String::from_utf8(git(cwd, &["config", "--local", "--get", "envrun.project"])?)
        .map_err(|_| "project name is not UTF-8")?;
    let project = project(name.trim_end())?;
    if !project.files.is_dir() {
        return Err(format!("project '{}' has no envrun store", project.name));
    }
    Ok(Repo { root, project })
}

pub fn inspect_repo(cwd: &Path) -> Result<Option<Repo>> {
    let inside = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !inside.status.success() {
        return Ok(None);
    }
    let configured = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", "--local", "--get", "envrun.project"])
        .output()
        .map_err(|e| e.to_string())?;
    match configured.status.code() {
        Some(0) => unregistered_repo(cwd).map(Some),
        Some(1) => Ok(None),
        _ => Err("cannot read envrun Git configuration".into()),
    }
}

pub fn discover_repo(cwd: &Path) -> Result<Option<Repo>> {
    let repo = inspect_repo(cwd)?;
    if let Some(repo) = &repo {
        register_repo(&repo.project, cwd)?;
    }
    Ok(repo)
}

fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    if !fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_dir()) {
        return Err(format!(
            "private directory is not a regular directory: {}",
            path.display()
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("cannot secure {}: {e}", path.display()))
}

fn private_store_parent(project: &Project, relative: &Path) -> Result<()> {
    let mut directory = project.files.clone();
    private_dir(&directory)?;
    if let Some(parent) = relative.parent() {
        for component in parent.components() {
            let Component::Normal(name) = component else {
                return Err("invalid store path".into());
            };
            directory.push(name);
            private_dir(&directory)?;
        }
    }
    Ok(())
}

pub fn init(cwd: &Path, name: &str) -> Result<Repo> {
    let root = repo_root(cwd)?;
    let project = project(name)?;
    if project.files.exists() {
        return Err(format!("project '{name}' already exists"));
    }
    if git(cwd, &["config", "--local", "--get", "envrun.project"]).is_ok() {
        return Err("this Git repository is already associated with an envrun project".into());
    }
    private_dir(&config_dir()?)?;
    private_dir(&config_dir()?.join("projects"))?;
    private_dir(project.files.parent().unwrap())?;
    private_dir(&project.files)?;
    if let Err(e) = git(cwd, &["config", "--local", "envrun.project", name]) {
        let _ = fs::remove_dir_all(project.files.parent().unwrap());
        return Err(e);
    }
    if let Err(error) = register_repo(&project, cwd) {
        let _ = git(cwd, &["config", "--local", "--unset", "envrun.project"]);
        let _ = fs::remove_dir_all(project.files.parent().unwrap());
        return Err(error);
    }
    Ok(Repo { root, project })
}

pub fn attach(cwd: &Path, name: &str) -> Result<Repo> {
    let root = repo_root(cwd)?;
    let project = project(name)?;
    if !project.files.is_dir() {
        return Err(format!("project '{name}' does not exist"));
    }
    if let Ok(existing) = git(cwd, &["config", "--local", "--get", "envrun.project"]) {
        if existing != format!("{name}\n").as_bytes() {
            return Err(
                "this Git repository is already associated with another envrun project".into(),
            );
        }
        register_repo(&project, cwd)?;
        return Ok(Repo { root, project });
    }
    git(cwd, &["config", "--local", "envrun.project", name])?;
    if let Err(error) = register_repo(&project, cwd) {
        let _ = git(cwd, &["config", "--local", "--unset", "envrun.project"]);
        return Err(error);
    }
    Ok(Repo { root, project })
}

pub fn relative_path(repo: &Repo, cwd: &Path, input: &Path) -> Result<PathBuf> {
    let path = if input.is_absolute() {
        input.to_path_buf()
    } else {
        cwd.join(input)
    };
    // Canonicalize the parent so paths through outside symlinks cannot enter the store.
    let parent = path.parent().ok_or("path has no parent")?;
    let parent = fs::canonicalize(parent).map_err(|e| format!("cannot resolve parent: {e}"))?;
    let name = path.file_name().ok_or("path has no file name")?;
    let path = parent.join(name);
    let relative = path
        .strip_prefix(&repo.root)
        .map_err(|_| "path is outside this repository")?;
    if !is_env_path(relative) {
        return Err("managed file must be named .env or .env.*".into());
    }
    Ok(relative.to_path_buf())
}

fn is_env_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(OsStr::to_str) else {
        return false;
    };
    (name == ".env" || name.starts_with(".env."))
        && !name.ends_with(".example")
        && !name.ends_with(".backup")
        && !name.contains(".envrun-")
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

fn store_file(project: &Project, relative: &Path) -> Result<PathBuf> {
    if !is_env_path(relative) {
        return Err("invalid managed env path".into());
    }
    Ok(project.files.join(relative))
}

fn git_is_tracked(repo: &Repo, relative: &Path) -> Result<bool> {
    let status = Command::new("git")
        .arg("-C")
        .arg(&repo.root)
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(relative)
        .output()
        .map_err(|e| e.to_string())?;
    match status.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err("cannot check Git tracking status".into()),
    }
}

fn git_is_ignored(repo: &Repo, relative: &Path) -> Result<bool> {
    let status = Command::new("git")
        .arg("-C")
        .arg(&repo.root)
        .args(["check-ignore", "-q", "--"])
        .arg(relative)
        .status()
        .map_err(|e| e.to_string())?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err("cannot check Git ignore status".into()),
    }
}

pub fn scan(repo: &Repo) -> Result<Vec<PathBuf>> {
    let output = git(
        &repo.root,
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "-z",
        ],
    )?;
    let mut result = Vec::new();
    for raw in output.split(|byte| *byte == 0).filter(|p| !p.is_empty()) {
        let rel = PathBuf::from(String::from_utf8_lossy(raw).as_ref());
        if is_env_path(&rel) && repo.root.join(&rel).is_file() && !repo.root.join(&rel).is_symlink()
        {
            result.push(rel);
        }
    }
    result.sort();
    Ok(result)
}

pub fn local_files(repo: &Repo) -> Result<Vec<PathBuf>> {
    let mut paths = BTreeSet::new();
    for args in [
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ][..],
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "-z",
        ][..],
    ] {
        for raw in git(&repo.root, args)?
            .split(|byte| *byte == 0)
            .filter(|p| !p.is_empty())
        {
            let rel = PathBuf::from(String::from_utf8_lossy(raw).as_ref());
            if is_env_path(&rel)
                && fs::symlink_metadata(repo.root.join(&rel))
                    .is_ok_and(|metadata| metadata.file_type().is_file())
            {
                paths.insert(rel);
            }
        }
    }
    Ok(paths.into_iter().collect())
}

fn temp_path(path: &Path) -> PathBuf {
    let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let suffix = format!(".envrun-{}-{id}", std::process::id());
    let mut name = path.file_name().unwrap().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

pub fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if !path.parent().ok_or("file has no parent")?.is_dir() {
        return Err("destination directory does not exist".into());
    }
    let tmp = temp_path(path);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| format!("cannot create temporary file: {e}"))?;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
        file.write_all(bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&tmp, path).map_err(|e| format!("cannot replace {}: {e}", path.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn is_exact_link(path: &Path, target: &Path) -> bool {
    fs::read_link(path).is_ok_and(|actual| actual == target)
}

pub fn add(repo: &Repo, cwd: &Path, input: &Path) -> Result<PathBuf> {
    let _lock = project_lock(&repo.project)?;
    let relative = relative_path(repo, cwd, input)?;
    let source = repo.root.join(&relative);
    let target = store_file(&repo.project, &relative)?;
    if fs::symlink_metadata(&target).is_ok() {
        return Err(format!("{} is already managed", relative.display()));
    }
    if git_is_tracked(repo, &relative)? {
        return Err(format!("{} is tracked by Git", relative.display()));
    }
    if !git_is_ignored(repo, &relative)? {
        return Err(format!("{} is not ignored by Git", relative.display()));
    }
    let meta = fs::symlink_metadata(&source)
        .map_err(|e| format!("cannot inspect {}: {e}", source.display()))?;
    if !meta.file_type().is_file() {
        return Err("source must be a regular file".into());
    }
    let bytes = fs::read(&source).map_err(|e| format!("cannot read source: {e}"))?;
    private_store_parent(&repo.project, &relative)?;
    write_private_atomic(&target, &bytes)?;
    if fs::read(&target).map_err(|e| e.to_string())? != bytes {
        let _ = fs::remove_file(&target);
        return Err("import copy verification failed".into());
    }
    let backup = temp_path(&source);
    let staged_link = temp_path(&source);
    if let Err(e) = symlink(&target, &staged_link) {
        let _ = fs::remove_file(&target);
        return Err(format!("cannot stage symlink: {e}"));
    }
    if let Err(e) = fs::rename(&source, &backup) {
        let _ = fs::remove_file(staged_link);
        let _ = fs::remove_file(&target);
        return Err(format!("cannot move source: {e}"));
    }
    if !fs::read(&backup).is_ok_and(|current| current == bytes) {
        if let Err(error) = fs::rename(&backup, &source) {
            return Err(format!(
                "source changed during import; recovery copy at {}: {error}",
                backup.display()
            ));
        }
        let _ = fs::remove_file(&staged_link);
        let _ = fs::remove_file(&target);
        return Err("source changed during import; original file restored".into());
    }
    if let Err(e) = fs::rename(&staged_link, &source) {
        let _ = fs::rename(&backup, &source);
        let _ = fs::remove_file(staged_link);
        let _ = fs::remove_file(&target);
        return Err(format!("cannot install symlink: {e}"));
    }
    fs::remove_file(backup)
        .map_err(|e| format!("imported, but cannot remove temporary source: {e}"))?;
    Ok(relative)
}

fn collect_files(base: &Path, dir: &Path, output: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_dir() {
            collect_files(base, &entry.path(), output)?;
        } else if kind.is_file()
            && let Ok(rel) = entry.path().strip_prefix(base)
            && is_env_path(rel)
        {
            output.push(rel.to_path_buf());
        }
    }
    Ok(())
}

pub fn files(project: &Project) -> Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    collect_files(&project.files, &project.files, &mut result)?;
    result.sort();
    Ok(result)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkStatus {
    Linked,
    Missing,
    Conflict,
    AbsentDirectory,
}

pub fn status(repo: &Repo, relative: &Path) -> Result<LinkStatus> {
    let target = store_file(&repo.project, relative)?;
    if !target.is_file() {
        return Err("managed file is missing".into());
    }
    if git_is_tracked(repo, relative)? {
        return Ok(LinkStatus::Conflict);
    }
    let path = destination(&repo.root, relative)?;
    if !path.parent().unwrap().is_dir() {
        return Ok(LinkStatus::AbsentDirectory);
    }
    if is_exact_link(&path, &target) {
        return Ok(LinkStatus::Linked);
    }
    if fs::symlink_metadata(&path).is_ok() {
        return Ok(LinkStatus::Conflict);
    }
    Ok(LinkStatus::Missing)
}

pub fn link(repo: &Repo) -> Result<(usize, usize)> {
    let _lock = project_lock(&repo.project)?;
    let listed = files(&repo.project)?;
    let mut missing = Vec::new();
    let mut skipped = 0;
    for relative in listed {
        match status(repo, &relative)? {
            LinkStatus::Linked => {}
            LinkStatus::Missing => missing.push(relative),
            LinkStatus::AbsentDirectory => skipped += 1,
            LinkStatus::Conflict => {
                return Err(format!(
                    "conflict at {}; no links created",
                    relative.display()
                ));
            }
        }
    }
    let mut linked = Vec::new();
    for relative in &missing {
        let target = store_file(&repo.project, relative)?;
        let destination = destination(&repo.root, relative)?;
        if let Err(error) = symlink(target, &destination) {
            for path in linked {
                let _ = fs::remove_file(path);
            }
            return Err(format!("cannot link {}: {error}", relative.display()));
        }
        linked.push(destination);
    }
    Ok((missing.len(), skipped))
}

pub fn unlink(repo: &Repo, relative: &Path) -> Result<()> {
    let _lock = project_lock(&repo.project)?;
    if status(repo, relative)? != LinkStatus::Linked {
        return Err(format!("{} is not linked here", relative.display()));
    }
    let source = store_file(&repo.project, relative)?;
    let destination = destination(&repo.root, relative)?;
    let tmp = temp_path(&destination);
    let bytes = fs::read(source).map_err(|e| e.to_string())?;
    write_private_atomic(&tmp, &bytes)?;
    fs::remove_file(&destination).map_err(|e| e.to_string())?;
    if let Err(e) = fs::rename(&tmp, &destination) {
        let _ = symlink(store_file(&repo.project, relative)?, &destination);
        return Err(format!("cannot restore ordinary file: {e}"));
    }
    Ok(())
}

fn relink_inputs(repo: &Repo, relative: &Path) -> Result<(PathBuf, PathBuf, Vec<u8>, Vec<u8>)> {
    let local = destination(&repo.root, relative)?;
    let shared = managed_file(&repo.project, relative)?;
    if git_is_tracked(repo, relative)? || !git_is_ignored(repo, relative)? {
        return Err("relink requires an ignored, untracked env file".into());
    }
    if !fs::symlink_metadata(&local).is_ok_and(|meta| meta.file_type().is_file()) {
        return Err(format!(
            "{} is not an independent local file",
            relative.display()
        ));
    }
    let local_bytes = fs::read(&local).map_err(|e| format!("cannot read local file: {e}"))?;
    let shared_bytes = fs::read(&shared).map_err(|e| format!("cannot read shared file: {e}"))?;
    Ok((local, shared, local_bytes, shared_bytes))
}

pub fn relink_preview(repo: &Repo, relative: &Path) -> Result<bool> {
    let (_, _, local, shared) = relink_inputs(repo, relative)?;
    Ok(local != shared)
}

/// Replace an independent local file with a link. Divergent bytes are kept in a
/// private backup and its path is returned to the caller.
pub fn relink(repo: &Repo, relative: &Path) -> Result<Option<PathBuf>> {
    let _lock = project_lock(&repo.project)?;
    let (local, shared, local_bytes, shared_bytes) = relink_inputs(repo, relative)?;
    let backup = if local_bytes != shared_bytes {
        let base = repo
            .project
            .files
            .parent()
            .ok_or("project store has no parent")?
            .join("relink-backups");
        private_dir(&base)?;
        let directory = loop {
            let candidate = base.join(format!(
                "{}-{}",
                std::process::id(),
                TEMP_ID.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("cannot create relink backup: {error}")),
            }
        };
        private_dir(&directory)?;
        let path = directory.join("local.env");
        write_private_atomic(&path, &local_bytes)?;
        Some(path)
    } else {
        None
    };
    let staged = temp_path(&local);
    symlink(&shared, &staged).map_err(|e| format!("cannot stage env link: {e}"))?;
    let unchanged = fs::symlink_metadata(&local).is_ok_and(|meta| meta.file_type().is_file())
        && fs::read(&local).is_ok_and(|bytes| bytes == local_bytes);
    if !unchanged {
        let _ = fs::remove_file(&staged);
        return Err("local env file changed during relink; retry".into());
    }
    if let Err(error) = fs::rename(&staged, &local) {
        let _ = fs::remove_file(&staged);
        return Err(format!("cannot install env link: {error}"));
    }
    Ok(backup)
}

pub fn removal_blockers(repo: &Repo, relative: &Path) -> Result<Vec<PathBuf>> {
    let target = managed_file(&repo.project, relative)?;
    let registrations = repositories(&repo.project)?;
    if registrations.is_empty() {
        return Err("project has no registered repositories".into());
    }
    let mut blockers = BTreeSet::new();
    for (id, common) in registrations {
        if !common.is_dir() {
            return Err(format!(
                "registered Git directory is inaccessible: {} (ID {id}); restore it, or use envrun forget-repo {id} --yes only after permanently retiring this clone and its worktrees",
                common.display()
            ));
        }
        for root in worktree_roots(&common)? {
            let path = destination(&root, relative)?;
            if is_exact_link(&path, &target) {
                blockers.insert(root);
            }
        }
    }
    Ok(blockers.into_iter().collect())
}

pub fn remove(repo: &Repo, relative: &Path) -> Result<()> {
    let _lock = project_lock(&repo.project)?;
    let target = managed_file(&repo.project, relative)?;
    let blockers = removal_blockers(repo, relative)?;
    if !blockers.is_empty() {
        let paths = blockers
            .iter()
            .map(|root| format!("  {}", root.display()))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(format!(
            "{} is still linked in:\n{paths}\nRun envrun unlink {} in each worktree first. Unlink preserves a regular local copy; remove deletes only the shared copy.",
            relative.display(),
            relative.display()
        ));
    }
    fs::remove_file(target).map_err(|e| format!("cannot remove managed file: {e}"))
}

pub fn managed_file(project: &Project, relative: &Path) -> Result<PathBuf> {
    let path = store_file(project, relative)?;
    if !fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file()) {
        return Err(format!("{} is not managed", relative.display()));
    }
    Ok(path)
}

pub fn selected_files(
    repo: &Repo,
    cwd: &Path,
    explicit: &[PathBuf],
    source: Source,
) -> Result<Vec<PathBuf>> {
    let cwd = fs::canonicalize(cwd).map_err(|e| e.to_string())?;
    let within = cwd
        .strip_prefix(&repo.root)
        .map_err(|_| "working directory is outside repository")?;
    let mut selected = Vec::new();
    let mut ancestor = PathBuf::new();
    for component in std::iter::once(None).chain(within.components().map(Some)) {
        if let Some(c) = component {
            ancestor.push(c.as_os_str());
        }
        let rel = ancestor.join(".env");
        let local_exists = match fs::symlink_metadata(destination(&repo.root, &rel)?) {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(format!("cannot inspect {}: {error}", rel.display())),
        };
        let shared_exists = managed_file(&repo.project, &rel).is_ok();
        let include = match source {
            Source::Auto => local_exists || shared_exists,
            Source::Local => local_exists,
            Source::Shared => shared_exists,
        };
        if include {
            resolve_file(repo, &rel, source)?;
            selected.push(rel);
        }
    }
    for input in explicit {
        let rel = relative_path(repo, &cwd, input)?;
        resolve_file(repo, &rel, source)?;
        selected.push(rel);
    }
    if selected.is_empty() {
        return Err("no env files selected for this source".into());
    }
    Ok(selected)
}

pub fn load_values(
    repo: &Repo,
    selected: &[PathBuf],
    source: Source,
) -> Result<BTreeMap<String, (String, PathBuf)>> {
    let mut values = BTreeMap::new();
    let mut context = envfile::inherited_vars();
    for rel in selected {
        let file = resolve_file(repo, rel, source)?;
        let bytes = fs::read(&file.path).map_err(|e| e.to_string())?;
        for (key, value) in envfile::read_vars_with_context(&bytes, &context)? {
            context.insert(key.clone(), value.clone());
            values.insert(key, (value, rel.clone()));
        }
    }
    Ok(values)
}

pub fn read_bytes(project: &Project, relative: &Path) -> Result<Vec<u8>> {
    fs::read(managed_file(project, relative)?).map_err(|e| e.to_string())
}

pub fn update_bytes(project: &Project, relative: &Path, before: &[u8], after: &[u8]) -> Result<()> {
    let _lock = project_lock(project)?;
    let path = managed_file(project, relative)?;
    let current = fs::read(&path).map_err(|e| e.to_string())?;
    if current != before {
        return Err("env file changed; reload before saving".into());
    }
    envfile::read_vars_bytes(after)?;
    write_private_atomic(&path, after)
}

pub fn get_key(project: &Project, relative: &Path, key: &str) -> Result<String> {
    let path = managed_file(project, relative)?;
    envfile::get(&path, key)
}

pub fn keys(project: &Project, relative: &Path) -> Result<Vec<String>> {
    let path = managed_file(project, relative)?;
    envfile::keys(&path)
}

pub fn set_key(project: &Project, relative: &Path, key: &str, value: &str) -> Result<()> {
    let before = read_bytes(project, relative)?;
    let after = envfile::edit(&before, key, Some(value))?;
    update_bytes(project, relative, &before, &after)
}

pub fn unset_key(project: &Project, relative: &Path, key: &str) -> Result<()> {
    let before = read_bytes(project, relative)?;
    let after = envfile::edit(&before, key, None)?;
    update_bytes(project, relative, &before, &after)
}
