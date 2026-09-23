use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    base: PathBuf,
    main: PathBuf,
    tree: PathBuf,
    config: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

impl Fixture {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!(
            "envrun-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let main = base.join("main");
        let tree = base.join("feature");
        let config = base.join("config");
        fs::create_dir_all(main.join("api")).unwrap();
        git(&main, &["init", "-q"]);
        git(&main, &["config", "user.name", "Test"]);
        git(&main, &["config", "user.email", "test@example.invalid"]);
        fs::write(main.join(".gitignore"), ".env*\n").unwrap();
        fs::write(main.join(".env.example"), "EXAMPLE=tracked\n").unwrap();
        fs::write(main.join("api/readme"), "api\n").unwrap();
        git(&main, &["add", ".gitignore", "api/readme"]);
        git(&main, &["add", "-f", ".env.example"]);
        git(&main, &["commit", "-qm", "fixture"]);
        fs::write(main.join(".env"), "ROOT=from-root\nSHARED=root\n").unwrap();
        fs::write(
            main.join("api/.env"),
            "SHARED=service\nMULTI=\"first\nsecond\"\n",
        )
        .unwrap();
        fs::write(main.join("api/.env.local"), "SHARED=local\n").unwrap();
        fs::write(main.join("api/.env.backup"), "BACKUP=skip\n").unwrap();
        Self {
            base,
            main,
            tree,
            config,
        }
    }

    fn cli(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_envrun"))
            .current_dir(cwd)
            .env("XDG_CONFIG_HOME", &self.config)
            .args(args)
            .output()
            .unwrap()
    }

    fn checked(&self, cwd: &Path, args: &[&str]) -> String {
        let output = self.cli(cwd, args);
        assert!(
            output.status.success(),
            "envrun {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn complete(&self, cwd: &Path, words: &[&str]) -> String {
        self.complete_shell(cwd, "bash", words)
    }

    fn complete_shell(&self, cwd: &Path, shell: &str, words: &[&str]) -> String {
        let output = Command::new(env!("CARGO_BIN_EXE_envrun"))
            .current_dir(cwd)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("ENVRUN_COMPLETE", shell)
            .env("_CLAP_COMPLETE_INDEX", (words.len() - 1).to_string())
            .arg("--")
            .args(words)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "completion for {words:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn set(&self, cwd: &Path, args: &[&str], value: &[u8]) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_envrun"))
            .current_dir(cwd)
            .env("XDG_CONFIG_HOME", &self.config)
            .arg("set")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(value).unwrap();
        child.wait_with_output().unwrap()
    }

    fn import(&self) {
        self.checked(&self.main, &["init", "demo"]);
        self.checked(&self.main, &["add", ".env", "api/.env", "api/.env.local"]);
    }

    fn worktree(&self) {
        git(
            &self.main,
            &[
                "worktree",
                "add",
                "-qb",
                "feature",
                self.tree.to_str().unwrap(),
            ],
        );
    }
}

#[test]
fn imports_links_and_runs_with_scoped_precedence() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.checked(&fixture.main, &["keys", ".env"]),
        "ROOT\nSHARED\n"
    );
    fixture.checked(&fixture.main, &["init", "demo"]);
    let scan = fixture.checked(&fixture.main, &["scan"]);
    assert!(scan.contains("api/.env.local"));
    assert!(!scan.contains(".env.example"));
    assert!(!scan.contains(".env.backup"));
    fixture.checked(
        &fixture.main,
        &["add", ".env", "api/.env", "api/.env.local"],
    );
    assert!(fixture.main.join("api/.env").is_symlink());
    let stored = fixture.config.join("envrun/projects/demo/files/api/.env");
    assert_eq!(
        fs::metadata(&stored).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(stored.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );

    fixture.worktree();
    let before_mode = fs::metadata(fixture.tree.join("api"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    let unlinked_api = fixture.tree.join("api");
    assert_eq!(
        fixture.checked(
            &unlinked_api,
            &["run", "--", "sh", "-c", "printf %s \"$SHARED\""]
        ),
        "service"
    );
    fixture.checked(&fixture.tree, &["link"]);
    assert!(fixture.tree.join("api/.env").is_symlink());
    assert_eq!(
        fs::metadata(fixture.tree.join("api"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        before_mode
    );

    let api = fixture.tree.join("api");
    let output = fixture.checked(
        &api,
        &[
            "run",
            "--",
            "sh",
            "-c",
            "printf '%s|%s|%s' \"$ROOT\" \"$SHARED\" \"$MULTI\"",
        ],
    );
    assert_eq!(output, "from-root|service|first\nsecond");
    let overridden = Command::new(env!("CARGO_BIN_EXE_envrun"))
        .current_dir(&api)
        .env("XDG_CONFIG_HOME", &fixture.config)
        .env("SHARED", "shell")
        .args(["run", "--", "sh", "-c", "printf %s \"$SHARED\""])
        .output()
        .unwrap();
    assert!(overridden.status.success());
    assert_eq!(overridden.stdout, b"service");
    let output = fixture.checked(
        &api,
        &[
            "run",
            "--file",
            ".env.local",
            "--",
            "sh",
            "-c",
            "printf '%s' \"$SHARED\"",
        ],
    );
    assert_eq!(output, "local");
    let explained = fixture.checked(&api, &["run", "--file", ".env.local", "--explain"]);
    assert!(explained.contains("SHARED <- api/.env.local"));
    assert!(!explained.contains("from-root"));
    let exit = fixture.cli(&api, &["run", "--", "sh", "-c", "exit 17"]);
    assert_eq!(exit.status.code(), Some(17));

    let mut set = Command::new(env!("CARGO_BIN_EXE_envrun"))
        .current_dir(&api)
        .env("XDG_CONFIG_HOME", &fixture.config)
        .args(["set", ".env", "SHARED"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    set.stdin.take().unwrap().write_all(b"changed").unwrap();
    assert!(set.wait_with_output().unwrap().status.success());
    assert_eq!(
        fixture.checked(&fixture.main, &["get", "api/.env", "SHARED"]),
        "changed"
    );
    assert_eq!(
        fixture
            .checked(&api, &["keys", ".env"])
            .lines()
            .collect::<Vec<_>>(),
        vec!["MULTI", "SHARED"]
    );
    assert!(
        fixture
            .checked(&fixture.main, &["status"])
            .contains("api/.env")
    );
}

#[test]
fn sibling_services_use_their_own_env_files_in_a_worktree() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.main.join("worker")).unwrap();
    fs::write(fixture.main.join("worker/readme"), "worker\n").unwrap();
    git(&fixture.main, &["add", "worker/readme"]);
    git(&fixture.main, &["commit", "-qm", "add worker service"]);
    fs::write(
        fixture.main.join("worker/.env"),
        "SHARED=worker\nWORKER_ONLY=yes\n",
    )
    .unwrap();
    fs::write(fixture.main.join("api/.env"), "SHARED=api\nAPI_ONLY=yes\n").unwrap();

    fixture.checked(&fixture.main, &["init", "demo"]);
    fixture.checked(&fixture.main, &["add", ".env", "api/.env", "worker/.env"]);
    fixture.worktree();
    fixture.checked(&fixture.tree, &["link"]);
    assert!(fixture.tree.join(".env").is_symlink());
    assert!(fixture.tree.join("api/.env").is_symlink());
    assert!(fixture.tree.join("worker/.env").is_symlink());

    let api = fixture.tree.join("api");
    let worker = fixture.tree.join("worker");
    let api_explain = fixture.checked(&api, &["run", "--explain"]);
    assert!(api_explain.contains("file: .env"));
    assert!(api_explain.contains("file: api/.env"));
    assert!(!api_explain.contains("worker/.env"));
    assert!(api_explain.contains("SHARED <- api/.env"));
    assert_eq!(
        fixture.checked(
            &api,
            &[
                "run",
                "--",
                "sh",
                "-c",
                "printf '%s|%s|%s' \"$ROOT\" \"$SHARED\" \"$API_ONLY\""
            ],
        ),
        "from-root|api|yes"
    );

    let worker_explain = fixture.checked(&worker, &["run", "--explain"]);
    assert!(worker_explain.contains("file: .env"));
    assert!(worker_explain.contains("file: worker/.env"));
    assert!(!worker_explain.contains("api/.env"));
    assert!(!worker_explain.contains("API_ONLY"));
    assert_eq!(
        fixture.checked(
            &worker,
            &[
                "run",
                "--",
                "sh",
                "-c",
                "printf '%s|%s|%s' \"$ROOT\" \"$SHARED\" \"$WORKER_ONLY\""
            ],
        ),
        "from-root|worker|yes"
    );

    let mut set = Command::new(env!("CARGO_BIN_EXE_envrun"))
        .current_dir(&worker)
        .env("XDG_CONFIG_HOME", &fixture.config)
        .args(["set", ".env", "SHARED"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    set.stdin.take().unwrap().write_all(b"updated").unwrap();
    assert!(set.wait_with_output().unwrap().status.success());
    assert_eq!(
        fixture.checked(&fixture.main, &["get", "worker/.env", "SHARED"]),
        "updated"
    );
    assert_eq!(
        fixture.checked(&fixture.main, &["get", "api/.env", "SHARED"]),
        "api"
    );
}

#[test]
fn conflicts_block_linking_and_remove_requires_unlinking() {
    let fixture = Fixture::new();
    fixture.import();
    fixture.worktree();
    fs::write(fixture.tree.join("api/.env"), "local work\n").unwrap();
    let failed = fixture.cli(&fixture.tree, &["link"]);
    assert!(!failed.status.success());
    assert!(!fixture.tree.join(".env").exists());
    assert_eq!(
        fs::read(fixture.tree.join("api/.env")).unwrap(),
        b"local work\n"
    );
    fs::remove_file(fixture.tree.join("api/.env")).unwrap();
    fixture.checked(&fixture.tree, &["link"]);
    assert!(
        !fixture
            .cli(&fixture.main, &["remove", "api/.env", "--yes"])
            .status
            .success()
    );
    fixture.checked(&fixture.tree, &["unlink", "api/.env"]);
    fixture.checked(&fixture.main, &["unlink", "api/.env"]);
    assert!(!fixture.main.join("api/.env").is_symlink());
    fixture.checked(&fixture.main, &["remove", "api/.env", "--yes"]);
    assert_eq!(
        fs::read(fixture.tree.join("api/.env")).unwrap(),
        b"SHARED=service\nMULTI=\"first\nsecond\"\n"
    );
}

#[test]
fn remove_checks_separately_attached_clones() {
    let fixture = Fixture::new();
    fixture.import();
    let clone = fixture.base.join("clone");
    let output = Command::new("git")
        .args(["clone", "-q"])
        .arg(&fixture.main)
        .arg(&clone)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fixture.checked(&clone, &["attach", "demo"]);
    fixture.checked(&clone, &["link"]);
    let moved = fixture.base.join("moved-clone");
    fs::rename(&clone, &moved).unwrap();
    fixture.checked(&moved, &["status"]);
    fixture.checked(&fixture.main, &["unlink", "api/.env"]);
    let blocked = fixture.cli(&fixture.main, &["remove", "api/.env", "--yes"]);
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("still linked"));
    fixture.checked(&moved, &["unlink", "api/.env"]);
    fixture.checked(&fixture.main, &["remove", "api/.env", "--yes"]);
}

#[test]
fn conflicting_copies_require_the_same_explicit_source_for_reads_edits_and_runs() {
    let fixture = Fixture::new();
    fixture.import();
    fixture.worktree();
    fixture.checked(&fixture.tree, &["link"]);
    let api = fixture.tree.join("api");
    fixture.checked(&api, &["unlink", ".env"]);
    fs::write(api.join(".env"), "SHARED=independent\nLOCAL_ONLY=yes\n").unwrap();
    for args in [
        vec!["keys", ".env"],
        vec!["get", ".env", "SHARED"],
        vec!["unset", ".env", "SHARED"],
        vec!["run", "--explain"],
    ] {
        let output = fixture.cli(&api, &args);
        assert!(
            !output.status.success(),
            "ambiguous source accepted by {args:?}"
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("--source local"));
    }
    assert!(
        !fixture
            .set(&api, &[".env", "SHARED"], b"wrong")
            .status
            .success()
    );
    assert_eq!(
        fixture.checked(&api, &["get", ".env", "SHARED", "--source", "local"]),
        "independent"
    );
    assert!(
        fixture
            .set(
                &api,
                &[".env", "SHARED", "--source", "local"],
                b"local-edited"
            )
            .status
            .success()
    );
    assert_eq!(
        fixture.checked(&api, &["get", ".env", "SHARED", "--source", "local"]),
        "local-edited"
    );
    assert_eq!(
        fixture.checked(
            &api,
            &[
                "run",
                "--source",
                "local",
                "--",
                "sh",
                "-c",
                "printf %s \"$SHARED\""
            ]
        ),
        "local-edited"
    );
    assert_eq!(
        fixture.checked(&fixture.main, &["get", "api/.env", "SHARED"]),
        "service"
    );
    assert!(
        fixture
            .set(
                &api,
                &[".env", "SHARED", "--source", "shared"],
                b"shared-edited"
            )
            .status
            .success()
    );
    assert_eq!(
        fixture.checked(
            &api,
            &[
                "run",
                "--source",
                "shared",
                "--",
                "sh",
                "-c",
                "printf %s \"$SHARED\""
            ]
        ),
        "shared-edited"
    );
    assert_eq!(
        fixture.checked(&api, &["get", ".env", "SHARED", "--source", "local"]),
        "local-edited"
    );
    fixture.checked(&api, &["unset", ".env", "LOCAL_ONLY", "--source", "local"]);
    assert_eq!(
        fixture.checked(&api, &["keys", ".env", "--source", "local"]),
        "SHARED\n"
    );
}

#[test]
fn completion_suggests_env_paths_and_keys_without_values() {
    let fixture = Fixture::new();
    fixture.checked(&fixture.main, &["init", "demo"]);
    let imports = fixture.complete(&fixture.main, &["envrun", "add", "api/"]);
    assert!(imports.contains("api/.env"));
    assert!(imports.contains("api/.env.local"));
    assert!(!imports.contains("api/.env.example"));
    assert!(!imports.contains("api/.env.backup"));

    fixture.checked(&fixture.main, &["add", "api/.env"]);
    let paths = fixture.complete(&fixture.main, &["envrun", "get", "api/"]);
    assert!(paths.contains("api/.env"));
    let keys = fixture.complete(&fixture.main, &["envrun", "get", "api/.env", "SH"]);
    assert!(keys.contains("SHARED"));
    assert!(!keys.contains("service"));
    let keys_without_prefix = fixture.complete(&fixture.main, &["envrun", "get", "api/.env", ""]);
    assert!(keys_without_prefix.contains("SHARED"));
    assert!(!keys_without_prefix.contains("--help"));
    assert!(!keys_without_prefix.contains("--source"));
    let zsh_keys = fixture.complete_shell(&fixture.main, "zsh", &["envrun", "get", "api/.env", ""]);
    assert!(zsh_keys.contains("SHARED"));
    assert!(!zsh_keys.contains("--help"));
    assert!(!zsh_keys.contains("--source"));
    let prefixed_path =
        fixture.complete_shell(&fixture.main, "zsh", &["envrun", "get", "./api/.env", ""]);
    assert!(prefixed_path.contains("SHARED"));
    let flags = fixture.complete(&fixture.main, &["envrun", "get", "api/.env", "--"]);
    assert!(flags.contains("--source"));
    let sources = fixture.complete(
        &fixture.main,
        &["envrun", "get", "api/.env", "SHARED", "--source", "l"],
    );
    assert!(sources.contains("local"));

    fixture.worktree();
    fixture.checked(&fixture.tree, &["link"]);
    let nested = fixture.complete(&fixture.tree.join("api"), &["envrun", "get", ".e"]);
    assert!(nested.contains(".env"));

    fixture.checked(&fixture.main, &["unlink", "api/.env"]);
    fs::write(fixture.main.join("api/.env"), "LOCAL_ONLY=private-value\n").unwrap();
    let ambiguous = fixture.complete(&fixture.main, &["envrun", "get", "api/.env", ""]);
    assert!(!ambiguous.contains("SHARED"));
    assert!(!ambiguous.contains("LOCAL_ONLY"));
    let local = fixture.complete(
        &fixture.main,
        &["envrun", "get", "--source", "local", "api/.env", "LOCAL"],
    );
    assert!(local.contains("LOCAL_ONLY"));
    assert!(!local.contains("private-value"));
    let shared = fixture.complete(
        &fixture.main,
        &["envrun", "get", "--source", "shared", "api/.env", "SH"],
    );
    assert!(shared.contains("SHARED"));
    assert!(!shared.contains("LOCAL_ONLY"));
}

#[test]
fn cli_suggestions_and_shell_setup_are_secret_free() {
    let fixture = Fixture::new();
    let before = fixture.checked(&fixture.main, &["suggest"]);
    assert!(before.contains("envrun init <project-name>"));
    fixture.import();
    let after = fixture.checked(&fixture.main, &["suggest"]);
    assert!(after.contains("envrun keys api/.env"));
    assert!(!after.contains("from-root"));
    assert!(!after.contains("service"));

    for shell in ["bash", "zsh", "fish"] {
        let setup = fixture.checked(&fixture.main, &["completions", shell]);
        assert!(setup.contains(&format!("ENVRUN_COMPLETE={shell} envrun")));
        let registration = Command::new(env!("CARGO_BIN_EXE_envrun"))
            .current_dir(&fixture.main)
            .env("XDG_CONFIG_HOME", &fixture.config)
            .env("ENVRUN_COMPLETE", shell)
            .output()
            .unwrap();
        assert!(registration.status.success());
        let script = String::from_utf8(registration.stdout).unwrap();
        assert!(!script.contains("from-root"));
        let registered = script.lines().any(|line| match shell {
            "bash" => line.trim_start().starts_with("complete ") && line.ends_with(" envrun"),
            "zsh" => line.starts_with("compdef ") && line.ends_with(" envrun"),
            "fish" => {
                line.starts_with("complete ") && line.contains("--command envrun --arguments")
            }
            _ => unreachable!(),
        });
        assert!(registered, "{shell} registered the wrong command");
    }

    let typo = fixture.cli(&fixture.main, &["satus"]);
    assert!(!typo.status.success());
    assert!(String::from_utf8_lossy(&typo.stderr).contains("status"));
}

#[test]
fn relink_restores_links_and_backs_up_divergent_local_copy() {
    let fixture = Fixture::new();
    fixture.import();
    fixture.worktree();
    fixture.checked(&fixture.tree, &["link"]);
    let local = fixture.tree.join("api/.env");
    let shared = fixture.config.join("envrun/projects/demo/files/api/.env");
    let original = fs::read(&shared).unwrap();

    fixture.checked(&fixture.tree, &["unlink", "api/.env"]);
    assert!(!local.is_symlink());
    let denied = fixture.cli(&fixture.tree, &["relink", "api/.env"]);
    assert!(!denied.status.success());
    assert!(!local.is_symlink());
    assert!(
        fixture
            .checked(&fixture.tree, &["relink", "api/.env", "--yes"])
            .contains("copies matched")
    );
    assert!(local.is_symlink());
    assert_eq!(fs::read(&shared).unwrap(), original);

    fixture.checked(&fixture.tree, &["unlink", "api/.env"]);
    let changed = b"SHARED=independent\nONLY_LOCAL=keep\n";
    fs::write(&local, changed).unwrap();
    let output = fixture.checked(&fixture.tree, &["relink", "api/.env", "--yes"]);
    let backup = PathBuf::from(output.trim().split_once("previous local copy: ").unwrap().1);
    assert!(local.is_symlink());
    assert_eq!(fs::read(&shared).unwrap(), original);
    assert_eq!(fs::read(&backup).unwrap(), changed);
    assert_eq!(
        fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(backup.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert!(
        fixture
            .checked(&fixture.tree, &["status"])
            .contains("linked           api/.env")
    );
}

#[test]
fn relink_refuses_unmanaged_and_tracked_local_files() {
    let fixture = Fixture::new();
    fixture.import();
    fixture.checked(&fixture.main, &["unlink", "api/.env"]);
    let local = fixture.main.join("api/.env");
    git(&fixture.main, &["add", "-f", "api/.env"]);
    let denied = fixture.cli(&fixture.main, &["relink", "api/.env", "--yes"]);
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("ignored, untracked"));
    assert!(!local.is_symlink());
    assert!(
        fixture
            .config
            .join("envrun/projects/demo/files/api/.env")
            .exists()
    );
}

#[test]
fn link_and_unlink_reject_symlinked_parent_directories() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    fixture.import();
    fixture.worktree();
    let outside = fixture.base.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::remove_dir_all(fixture.tree.join("api")).unwrap();
    symlink(&outside, fixture.tree.join("api")).unwrap();
    let linked = fixture.cli(&fixture.tree, &["link"]);
    assert!(!linked.status.success());
    assert!(String::from_utf8_lossy(&linked.stderr).contains("unsafe parent"));
    assert!(
        !fixture.tree.join(".env").exists(),
        "preflight must block all writes"
    );
    assert!(!outside.join(".env").exists());
    let stored = fixture.config.join("envrun/projects/demo/files/api/.env");
    symlink(&stored, outside.join(".env")).unwrap();
    let repo = envrun::project::Repo {
        root: fs::canonicalize(&fixture.tree).unwrap(),
        project: envrun::project::Project {
            name: "demo".into(),
            files: fixture.config.join("envrun/projects/demo/files"),
        },
    };
    assert!(envrun::project::unlink(&repo, Path::new("api/.env")).is_err());
    assert!(
        outside.join(".env").is_symlink(),
        "must not replace an external link with secret bytes"
    );
}

#[test]
fn layered_expansion_uses_prior_files_before_the_inherited_shell() {
    let fixture = Fixture::new();
    fs::write(fixture.main.join(".env"), "ENVRUN_TEST_HOST=root\n").unwrap();
    fs::write(
        fixture.main.join("api/.env"),
        "URL=https://${ENVRUN_TEST_HOST}/api\n",
    )
    .unwrap();
    fs::write(
        fixture.main.join("api/.env.local"),
        "ENVRUN_TEST_HOST=override\nURL=https://$ENVRUN_TEST_HOST/local\n",
    )
    .unwrap();
    fixture.import();
    fixture.worktree();
    let api = fixture.tree.join("api");
    for (extra, expected) in [
        (vec![], "root|https://root/api"),
        (
            vec!["--file", ".env.local"],
            "override|https://override/local",
        ),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_envrun"))
            .current_dir(&api)
            .env("XDG_CONFIG_HOME", &fixture.config)
            .env("ENVRUN_TEST_HOST", "shell")
            .arg("run")
            .args(extra)
            .args([
                "--",
                "sh",
                "-c",
                "printf '%s|%s' \"$ENVRUN_TEST_HOST\" \"$URL\"",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
    }
}

#[test]
fn auto_run_includes_local_only_service_files_and_explains_the_source() {
    let fixture = Fixture::new();
    fixture.checked(&fixture.main, &["init", "demo"]);
    fixture.checked(&fixture.main, &["add", ".env"]);
    let api = fixture.main.join("api");
    assert_eq!(fixture.checked(&api, &["get", ".env", "SHARED"]), "service");
    assert!(
        fixture
            .set(&api, &[".env", "SHARED"], b"local-edited")
            .status
            .success()
    );
    assert_eq!(
        fixture.checked(&api, &["run", "--", "sh", "-c", "printf %s \"$SHARED\""]),
        "local-edited"
    );
    let explained = fixture.checked(&api, &["run", "--explain"]);
    assert!(explained.contains("file: .env (shared)"));
    assert!(explained.contains("file: api/.env (local)"));
    assert!(explained.contains("SHARED <- api/.env (local)"));
    assert_eq!(
        fixture.checked(
            &api,
            &[
                "run",
                "--source",
                "shared",
                "--",
                "sh",
                "-c",
                "printf %s \"$SHARED\""
            ]
        ),
        "root"
    );
}

#[test]
fn missing_clone_registration_requires_explicit_cleanup_without_deleting_envs() {
    let fixture = Fixture::new();
    fixture.import();
    let clone = fixture.base.join("deleted clone");
    git(
        &fixture.main,
        &[
            "clone",
            "-q",
            fixture.main.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    fixture.checked(&clone, &["attach", "demo"]);
    let registrations = fixture.checked(&fixture.main, &["repositories"]);
    let id = registrations
        .lines()
        .find(|line| line.contains("deleted clone"))
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    assert!(
        !fixture
            .cli(&fixture.main, &["forget-repo", id, "--yes"])
            .status
            .success()
    );
    fs::remove_dir_all(&clone).unwrap();
    fixture.checked(&fixture.main, &["unlink", "api/.env"]);
    let blocked = fixture.cli(&fixture.main, &["remove", "api/.env", "--yes"]);
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains(id));
    assert!(
        !fixture
            .cli(&fixture.main, &["forget-repo", id])
            .status
            .success()
    );
    fixture.checked(&fixture.main, &["forget-repo", id, "--yes"]);
    assert!(
        fixture
            .config
            .join("envrun/projects/demo/files/api/.env")
            .is_file()
    );
    fixture.checked(&fixture.main, &["remove", "api/.env", "--yes"]);
    assert!(fixture.main.join("api/.env").is_file());
    assert!(fixture.main.join(".env").is_symlink());
}

#[test]
fn removal_reports_every_linked_worktree_including_paths_with_newlines() {
    let fixture = Fixture::new();
    fixture.import();
    fixture.worktree();
    fixture.checked(&fixture.tree, &["link"]);
    let extra = fixture.base.join("worktree\nextra");
    git(
        &fixture.main,
        &["worktree", "add", "-qb", "extra", extra.to_str().unwrap()],
    );
    fixture.checked(&extra, &["link"]);
    let blocked = fixture.cli(&fixture.main, &["remove", "api/.env", "--yes"]);
    assert!(!blocked.status.success());
    let message = String::from_utf8_lossy(&blocked.stderr);
    for root in [&fixture.main, &fixture.tree, &extra] {
        assert!(message.contains(fs::canonicalize(root).unwrap().to_str().unwrap()));
    }
    for root in [&fixture.main, &fixture.tree, &extra] {
        fixture.checked(root, &["unlink", "api/.env"]);
    }
    fixture.checked(&fixture.main, &["remove", "api/.env", "--yes"]);
}
