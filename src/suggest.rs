use crate::completion;
use envrun::Result;
use envrun::project;
use std::io::{self, IsTerminal, Write};
use std::path::Path;

struct Suggestion {
    label: String,
    command: String,
}

fn shell_word(word: &str) -> String {
    if word
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b))
    {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

fn suggestions(cwd: &Path) -> Result<Vec<Suggestion>> {
    let mut result = Vec::new();
    if let Some(repo) = project::inspect_repo(cwd)? {
        result.push(Suggestion {
            label: "Review worktree links".into(),
            command: "envrun status".into(),
        });
        let managed = project::files(&repo.project)?;
        if managed.iter().any(|path| {
            matches!(
                project::status(&repo, path),
                Ok(project::LinkStatus::Missing)
            )
        }) {
            result.push(Suggestion {
                label: "Link managed files into this worktree".into(),
                command: "envrun link".into(),
            });
        }
        let importable = project::scan(&repo)?;
        if importable.is_empty() {
            result.push(Suggestion {
                label: "Look for ignored env files to import".into(),
                command: "envrun scan".into(),
            });
        }
        for path in importable.into_iter().take(5) {
            let path = completion::from_cwd(&repo, cwd, &path);
            let Some(path) = path.to_str() else { continue };
            result.push(Suggestion {
                label: format!("Import {path}"),
                command: format!("envrun add {}", shell_word(path)),
            });
        }
        for path in managed.into_iter().take(5) {
            let path = completion::from_cwd(&repo, cwd, &path);
            let Some(path) = path.to_str() else { continue };
            result.push(Suggestion {
                label: format!("List keys in {path}"),
                command: format!("envrun keys {}", shell_word(path)),
            });
        }
        result.push(Suggestion {
            label: "Preview env files and key names used by a command".into(),
            command: "envrun run --explain".into(),
        });
        result.push(Suggestion {
            label: "Run a command with env files".into(),
            command: "envrun run -- <command>".into(),
        });
    } else if project::repo_root(cwd).is_ok() {
        result.push(Suggestion {
            label: "Create a new envrun project here".into(),
            command: "envrun init <project-name>".into(),
        });
        for project in project::projects()?.into_iter().take(5) {
            result.push(Suggestion {
                label: format!("Attach this clone to {}", project.name),
                command: format!("envrun attach {}", shell_word(&project.name)),
            });
        }
    } else {
        result.push(Suggestion {
            label: "Open a Git repository before initializing envrun".into(),
            command: "cd <git-repository>".into(),
        });
    }
    result.push(Suggestion {
        label: "Open the terminal UI".into(),
        command: "envrun tui".into(),
    });
    Ok(result)
}

pub fn run(cwd: &Path) -> Result<()> {
    let suggestions = suggestions(cwd)?;
    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
    println!("Suggested next commands:");
    for (index, suggestion) in suggestions.iter().enumerate() {
        println!("  {}. {}", index + 1, suggestion.label);
        if !interactive {
            println!("     {}", suggestion.command);
        }
    }
    if interactive {
        print!("Choose a number to print its command (Enter to exit): ");
        io::stdout().flush().map_err(|e| e.to_string())?;
        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .map_err(|e| e.to_string())?;
        if !input.trim().is_empty() {
            let index = input
                .trim()
                .parse::<usize>()
                .ok()
                .filter(|index| (1..=suggestions.len()).contains(index))
                .ok_or("choose a listed number")?;
            println!("{}", suggestions[index - 1].command);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::shell_word;

    #[test]
    fn quotes_suggested_path_without_running_shell_syntax() {
        assert_eq!(shell_word("api/.env"), "api/.env");
        assert_eq!(shell_word("api/my env/.env"), "'api/my env/.env'");
        assert_eq!(shell_word("api/it's/.env"), "'api/it'\\''s/.env'");
    }
}
