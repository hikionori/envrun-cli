use clap_complete::engine::{CompletionCandidate, PathCompleter, ValueCompleter};
use envrun::envfile;
use envrun::project::{self, Repo, Source};
use std::collections::BTreeSet;
use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy)]
enum Paths {
    Import,
    Managed,
    Readable,
}

fn env_path(path: &Path) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| {
            (name == ".env" || name.starts_with(".env."))
                && !name.ends_with(".example")
                && !name.ends_with(".backup")
                && !name.contains(".envrun-")
        })
}

pub(crate) fn from_cwd(repo: &Repo, cwd: &Path, relative: &Path) -> PathBuf {
    let within = cwd.strip_prefix(&repo.root).unwrap_or(Path::new(""));
    if let Ok(local) = relative.strip_prefix(within)
        && !local.as_os_str().is_empty()
    {
        return local.to_path_buf();
    }
    let mut result = PathBuf::new();
    for _ in within.components() {
        result.push("..");
    }
    result.join(relative)
}

fn path_candidates(current: &OsStr, kind: Paths) -> Vec<CompletionCandidate> {
    let Ok(cwd) = env::current_dir() else {
        return Vec::new();
    };
    let mut paths = BTreeSet::new();
    if let Ok(Some(repo)) = project::inspect_repo(&cwd) {
        let listed = match kind {
            Paths::Import => project::scan(&repo).unwrap_or_default(),
            Paths::Managed | Paths::Readable => {
                let mut listed = project::files(&repo.project).unwrap_or_default();
                if matches!(kind, Paths::Readable) {
                    listed.extend(project::local_files(&repo).unwrap_or_default());
                }
                listed
            }
        };
        for path in listed {
            paths.insert(from_cwd(&repo, &cwd, &path).into_os_string());
        }
    }
    if matches!(kind, Paths::Readable) {
        for candidate in PathCompleter::file().filter(env_path).complete(current) {
            paths.insert(candidate.get_value().to_os_string());
        }
    }
    paths
        .into_iter()
        .filter(|path| {
            path.to_string_lossy()
                .starts_with(current.to_string_lossy().as_ref())
        })
        .map(CompletionCandidate::new)
        .collect()
}

pub fn import_paths(current: &OsStr) -> Vec<CompletionCandidate> {
    path_candidates(current, Paths::Import)
}

pub fn managed_paths(current: &OsStr) -> Vec<CompletionCandidate> {
    path_candidates(current, Paths::Managed)
}

pub fn readable_paths(current: &OsStr) -> Vec<CompletionCandidate> {
    path_candidates(current, Paths::Readable)
}

fn completion_words() -> Vec<OsString> {
    let args: Vec<_> = env::args_os().collect();
    let mut words = args
        .iter()
        .position(|word| word == "--")
        .map(|at| args[at + 1..].to_vec())
        .unwrap_or_default();
    if let Some(index) = env::var("_CLAP_COMPLETE_INDEX")
        .ok()
        .and_then(|index| index.parse::<usize>().ok())
    {
        words.truncate(index.saturating_add(1));
    }
    words
}

pub fn completing_key() -> bool {
    completing_key_for(&completion_words())
}

fn completing_key_for(words: &[OsString]) -> bool {
    if words.len() < 4 {
        return false;
    }
    if !matches!(
        words.get(1).and_then(|word| word.to_str()),
        Some("get" | "set" | "unset")
    ) {
        return false;
    }
    let Some(current) = words.last() else {
        return false;
    };
    if current.to_string_lossy().starts_with('-') {
        return false;
    }
    let mut positionals = 0;
    let mut args = words[2..words.len() - 1].iter();
    while let Some(word) = args.next() {
        if word == "--source" {
            if args.next().is_none() {
                return false;
            }
        } else if !word.to_string_lossy().starts_with('-') {
            positionals += 1;
        }
    }
    positionals == 1
}

fn key_context(words: &[OsString]) -> Option<(PathBuf, Source)> {
    if !matches!(words.get(1)?.to_str()?, "get" | "set" | "unset") {
        return None;
    }
    let mut path = None;
    let mut source = Source::Auto;
    let mut args = words.iter().skip(2).peekable();
    while let Some(word) = args.next() {
        if word == "--source" {
            source = match args.next()?.to_str()? {
                "local" => Source::Local,
                "shared" => Source::Shared,
                _ => Source::Auto,
            };
        } else if let Some(value) = word
            .to_str()
            .and_then(|word| word.strip_prefix("--source="))
        {
            source = match value {
                "local" => Source::Local,
                "shared" => Source::Shared,
                _ => Source::Auto,
            };
        } else if !word.to_string_lossy().starts_with('-') && path.is_none() {
            path = Some(PathBuf::from(word));
        }
    }
    Some((path?, source))
}

pub fn key_names(current: &OsStr) -> Vec<CompletionCandidate> {
    key_names_for(&completion_words(), current)
}

fn key_names_for(words: &[OsString], current: &OsStr) -> Vec<CompletionCandidate> {
    let Some((path, source)) = key_context(words) else {
        return Vec::new();
    };
    let Ok(cwd) = env::current_dir() else {
        return Vec::new();
    };
    let resolved = match project::inspect_repo(&cwd) {
        Ok(Some(repo)) => project::relative_path(&repo, &cwd, &path)
            .and_then(|relative| project::resolve_file(&repo, &relative, source))
            .map(|file| file.path),
        Ok(None) if source != Source::Shared => Ok(cwd.join(path)),
        _ => return Vec::new(),
    };
    let Ok(path) = resolved else {
        return Vec::new();
    };
    envfile::keys(&path)
        .unwrap_or_default()
        .into_iter()
        .filter(|name| name.starts_with(current.to_string_lossy().as_ref()))
        .map(CompletionCandidate::new)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_context_handles_source_before_or_after_path() {
        let words = ["envrun", "get", "--source", "shared", "api/.env", "KE"].map(OsString::from);
        assert_eq!(
            key_context(&words),
            Some((PathBuf::from("api/.env"), Source::Shared))
        );
        let words = ["envrun", "set", "api/.env", "--source=local", "KE"].map(OsString::from);
        assert_eq!(
            key_context(&words),
            Some((PathBuf::from("api/.env"), Source::Local))
        );
    }

    #[test]
    fn key_position_excludes_flags_but_flag_prefix_and_source_value_keep_them() {
        let words = ["envrun", "get"].map(OsString::from);
        assert!(!completing_key_for(&words));
        let words = ["envrun", "get", ".env"].map(OsString::from);
        assert!(!completing_key_for(&words));
        let words = ["envrun", "get", ".env", ""].map(OsString::from);
        assert!(completing_key_for(&words));
        let words = ["envrun", "get", ".env", "--"].map(OsString::from);
        assert!(!completing_key_for(&words));
        let words = ["envrun", "get", ".env", "--source", "l"].map(OsString::from);
        assert!(!completing_key_for(&words));
        let words = ["envrun", "get", ".env", "--source", "local", ""].map(OsString::from);
        assert!(completing_key_for(&words));
    }
}
