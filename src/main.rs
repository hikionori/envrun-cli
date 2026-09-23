mod completion;
mod suggest;

use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::CompleteEnv;
use clap_complete::engine::ArgValueCompleter;
use envrun::project::{self, LinkStatus, Source};
use envrun::{Result, envfile, tui};
use std::env;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Parser)]
#[command(version, about = "Manage env files across Git worktrees")]
struct Cli {
    #[command(subcommand)]
    command: Option<Action>,
}

fn completion_command() -> clap::Command {
    let mut command = Cli::command();
    if completion::completing_key() {
        for name in ["get", "set", "unset"] {
            command = command.mut_subcommand(name, |subcommand| {
                subcommand
                    .disable_help_flag(true)
                    .mut_arg("source", |source| source.hide(true))
            });
        }
    }
    command
}

#[derive(Clone, Copy, ValueEnum)]
enum CompletionShell {
    Bash,
    Zsh,
    Fish,
}

#[derive(Subcommand)]
enum Action {
    /// Open the terminal UI.
    Tui,
    /// Suggest useful next commands for this checkout.
    Suggest,
    /// Print the shell setup line for dynamic tab completion.
    Completions { shell: CompletionShell },
    /// Register this Git repository as a new project.
    Init { name: String },
    /// Associate this clone with an existing project.
    Attach { name: String },
    /// List registered clones and their IDs.
    Repositories,
    /// Forget a permanently deleted clone after retiring all of its worktrees.
    ForgetRepo {
        id: String,
        #[arg(long)]
        yes: bool,
    },
    /// List ignored env files available to import.
    Scan,
    /// Import files and replace them with links.
    Add {
        #[arg(required = true, add = ArgValueCompleter::new(completion::import_paths))]
        paths: Vec<PathBuf>,
    },
    /// Link managed files into this worktree.
    Link,
    /// Show worktree link status.
    Status,
    /// Restore an ordinary copy in this worktree.
    Unlink {
        #[arg(add = ArgValueCompleter::new(completion::managed_paths))]
        path: PathBuf,
    },
    /// Restore a shared link, backing up a different local copy privately.
    Relink {
        #[arg(add = ArgValueCompleter::new(completion::managed_paths))]
        path: PathBuf,
        #[arg(long)]
        yes: bool,
    },
    /// Delete a managed file after all worktrees unlink it.
    Remove {
        #[arg(add = ArgValueCompleter::new(completion::managed_paths))]
        path: PathBuf,
        #[arg(long)]
        yes: bool,
    },
    /// List variable names in one file.
    Keys {
        #[arg(add = ArgValueCompleter::new(completion::readable_paths))]
        path: PathBuf,
        #[arg(long, value_enum, default_value = "auto")]
        source: Source,
    },
    /// Write one value to stdout without a newline.
    Get {
        #[arg(add = ArgValueCompleter::new(completion::readable_paths))]
        path: PathBuf,
        #[arg(add = ArgValueCompleter::new(completion::key_names))]
        key: String,
        #[arg(long, value_enum, default_value = "auto")]
        source: Source,
    },
    /// Set one value from stdin.
    Set {
        #[arg(add = ArgValueCompleter::new(completion::readable_paths))]
        path: PathBuf,
        #[arg(add = ArgValueCompleter::new(completion::key_names))]
        key: String,
        #[arg(long, value_enum, default_value = "auto")]
        source: Source,
    },
    /// Remove one variable from a file.
    Unset {
        #[arg(add = ArgValueCompleter::new(completion::readable_paths))]
        path: PathBuf,
        #[arg(add = ArgValueCompleter::new(completion::key_names))]
        key: String,
        #[arg(long, value_enum, default_value = "auto")]
        source: Source,
    },
    /// Run a command with selected env files.
    Run {
        /// Choose local files or the shared store; auto refuses conflicting copies.
        #[arg(long, value_enum, default_value = "auto")]
        source: Source,
        #[arg(short, long = "file", add = ArgValueCompleter::new(completion::readable_paths))]
        files: Vec<PathBuf>,
        #[arg(long)]
        explain: bool,
        #[arg(last = true)]
        command: Vec<OsString>,
    },
}

fn relative(repo: &project::Repo, cwd: &Path, path: &Path) -> Result<PathBuf> {
    project::relative_path(repo, cwd, path)
}

fn run_command(
    repo: &project::Repo,
    cwd: &Path,
    files: &[PathBuf],
    source: Source,
    explain: bool,
    command: &[OsString],
) -> Result<()> {
    if !explain && command.is_empty() {
        return Err("expected a command after --".into());
    }
    let selected = project::selected_files(repo, cwd, files, source)?;
    let values = project::load_values(repo, &selected, source)?;
    if explain {
        for file in &selected {
            println!(
                "file: {} ({})",
                file.display(),
                project::resolve_file(repo, file, source)?.label()
            );
        }
        for (key, (_, path)) in &values {
            println!(
                "{key} <- {} ({})",
                path.display(),
                project::resolve_file(repo, path, source)?.label()
            );
        }
        return Ok(());
    }
    let mut child = Command::new(&command[0]);
    child.args(&command[1..]);
    for (key, (value, _)) in values {
        child.env(key, value);
    }
    Err(format!("cannot run command: {}", child.exec()))
}

fn inspection_path(cwd: &Path, path: &Path, source: Source) -> Result<PathBuf> {
    if let Some(repo) = project::discover_repo(cwd)? {
        let rel = relative(&repo, cwd, path)?;
        return Ok(project::resolve_file(&repo, &rel, source)?.path);
    }
    if source == Source::Shared {
        return Err("shared source requires an initialized repository".into());
    }
    Ok(path.to_path_buf())
}

fn execute(action: Option<Action>) -> Result<()> {
    let cwd = env::current_dir().map_err(|e| e.to_string())?;
    match action {
        None | Some(Action::Tui) => return tui::run(&cwd),
        Some(Action::Suggest) => return suggest::run(&cwd),
        Some(Action::Completions { shell }) => {
            println!(
                "{}",
                match shell {
                    CompletionShell::Bash => "source <(ENVRUN_COMPLETE=bash envrun)",
                    CompletionShell::Zsh => "source <(ENVRUN_COMPLETE=zsh envrun)",
                    CompletionShell::Fish => "ENVRUN_COMPLETE=fish envrun | source",
                }
            );
            return Ok(());
        }
        Some(Action::Init { name }) => {
            let repo = project::init(&cwd, &name)?;
            println!("{}: {}", repo.project.name, repo.root.display());
            return Ok(());
        }
        Some(Action::Attach { name }) => {
            let repo = project::attach(&cwd, &name)?;
            println!("{}: {}", repo.project.name, repo.root.display());
            return Ok(());
        }
        _ => {}
    }

    match &action {
        Some(Action::Keys { path, source }) => {
            for key in envfile::keys(&inspection_path(&cwd, path, *source)?)? {
                println!("{key}");
            }
            return Ok(());
        }
        Some(Action::Get { path, key, source }) => {
            io::stdout()
                .write_all(envfile::get(&inspection_path(&cwd, path, *source)?, key)?.as_bytes())
                .map_err(|e| e.to_string())?;
            return Ok(());
        }
        _ => {}
    }

    let repo = project::current_repo(&cwd)?;
    match action.unwrap() {
        Action::Repositories => {
            for (id, common) in project::repositories(&repo.project)? {
                println!(
                    "{id}  {}  {}",
                    if common.is_dir() {
                        "present"
                    } else {
                        "unavailable"
                    },
                    common.display()
                );
            }
        }
        Action::ForgetRepo { id, yes } => {
            if !yes {
                return Err("forget-repo requires --yes; retire the missing clone and ALL its worktrees first".into());
            }
            project::forget_repository(&repo.project, &id)?;
            println!("forgot repository {id}; shared env files were preserved");
        }
        Action::Scan => {
            for path in project::scan(&repo)? {
                println!("{}", path.display());
            }
        }
        Action::Add { paths } => {
            for path in paths {
                println!("added {}", project::add(&repo, &cwd, &path)?.display());
            }
        }
        Action::Link => {
            let (linked, skipped) = project::link(&repo)?;
            println!("linked {linked}, skipped {skipped} absent directories");
        }
        Action::Status => {
            for path in project::files(&repo.project)? {
                let state = match project::status(&repo, &path)? {
                    LinkStatus::Linked => "linked",
                    LinkStatus::Missing => "missing",
                    LinkStatus::Conflict => "conflict",
                    LinkStatus::AbsentDirectory => "absent directory",
                };
                println!("{state:16} {}", path.display());
            }
        }
        Action::Unlink { path } => {
            let rel = relative(&repo, &cwd, &path)?;
            project::unlink(&repo, &rel)?;
            println!("unlinked {}", rel.display());
        }
        Action::Relink { path, yes } => {
            if !yes {
                return Err(
                    "relink requires --yes; a different local copy will be backed up privately"
                        .into(),
                );
            }
            let rel = relative(&repo, &cwd, &path)?;
            match project::relink(&repo, &rel)? {
                Some(backup) => println!(
                    "relinked {}; previous local copy: {}",
                    rel.display(),
                    backup.display()
                ),
                None => println!("relinked {}; copies matched", rel.display()),
            }
        }
        Action::Remove { path, yes } => {
            if !yes {
                return Err("remove requires --yes".into());
            }
            let rel = relative(&repo, &cwd, &path)?;
            project::remove(&repo, &rel)?;
            println!("removed {}", rel.display());
        }
        Action::Set { path, key, source } => {
            let rel = relative(&repo, &cwd, &path)?;
            let mut bytes = Vec::new();
            io::stdin()
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            let value = String::from_utf8(bytes).map_err(|_| "stdin value must be UTF-8")?;
            project::edit_file(&repo, &rel, source, &key, Some(&value))?;
        }
        Action::Unset { path, key, source } => {
            let rel = relative(&repo, &cwd, &path)?;
            project::edit_file(&repo, &rel, source, &key, None)?;
        }
        Action::Run {
            source,
            files,
            explain,
            command,
        } => run_command(&repo, &cwd, &files, source, explain, &command)?,
        Action::Keys { .. }
        | Action::Get { .. }
        | Action::Tui
        | Action::Suggest
        | Action::Completions { .. }
        | Action::Init { .. }
        | Action::Attach { .. } => unreachable!(),
    }
    Ok(())
}

fn main() {
    CompleteEnv::with_factory(completion_command)
        .var("ENVRUN_COMPLETE")
        .bin("envrun")
        .complete();
    let cli = Cli::parse();
    if let Err(error) = execute(cli.command) {
        eprintln!("envrun: {error}");
        std::process::exit(1);
    }
}
