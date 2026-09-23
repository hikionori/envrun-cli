# envrun

`envrun` manages ignored `.env` files across Git worktrees. It keeps shared files in a private local store, links them into each worktree, and loads their variables for any command. A terminal UI lets you browse files and keys without showing values by default. It supports macOS and Linux.

This is a public beta. The store is local plaintext, with no encryption or team secret synchronization. Review the paths and sources shown by `envrun status` and `envrun run --explain` before using it with important credentials.

## License

The source is available under the [PolyForm Noncommercial License 1.0.0](LICENSE). Noncommercial use, modification, and sharing are permitted under its terms. Commercial use by others is not licensed; Danila Verbinskiy retains the option to use and sell the software commercially. This is a source-available project, not an open-source license.

Install from GitHub with `cargo install --git https://github.com/hikionori/envrun-cli --locked`, or from this checkout with `cargo install --path . --locked`. Run commands through `cargo run --` while developing. The package is named `envrun-cli`; both installation methods provide the `envrun` command.

## Suggestions and completion

Clap suggests close command and option names when you mistype one. Run `envrun suggest` for next commands based on the current checkout. In a terminal, it also lets you pick a suggestion to print its command; it does not run that command.

To enable tab completion, add the line generated for your shell to its startup file:

```sh
envrun completions bash >> ~/.bashrc
envrun completions zsh >> ~/.zshrc
mkdir -p ~/.config/fish
envrun completions fish >> ~/.config/fish/config.fish
```

Use the line for your shell, then start a new shell. Completion suggests commands, options, env file paths, `--source` choices, and existing key names for `get`, `set`, and `unset`. It never suggests values. Key names come from the selected local or shared file; when both copies exist, add `--source local` or `--source shared` to choose one. The shell setup line reloads envrun's completion code on startup so it stays in sync after upgrades.

## Quick start

In a Git repository with ignored env files, choose the paths you want to share:

```sh
envrun init myapp
envrun scan
envrun add .env services/api/.env
envrun status
```

The paths passed to `add` must exist and be ignored by Git. Use the output of `scan` to choose them. `init` creates an empty named project and records its name and a local repository ID in Git config. `scan` lists ignored, regular `.env` and `.env.*` files, excluding `*.example`, `*.backup`, and files already linked to the store. `add` copies selected files to `$XDG_CONFIG_HOME/envrun/projects/<name>/files/` (or `~/.config/envrun/...`), verifies the copies, and replaces the originals with symlinks. It refuses tracked files and existing managed paths.

From the repository root, create another worktree and link the managed files there:

```sh
git worktree add -b feature-x ../myapp-feature
cd ../myapp-feature
envrun link
envrun status
```

`link` creates links for managed paths whose parent directories exist. It stops before creating any links if it finds a conflicting file or a symlinked parent directory. Editing a linked file changes the shared copy immediately. To make a worktree's file independent, run `envrun unlink path/to/.env`; this restores a regular copy there. Both copies then exist: use `--source local` to work with the independent file, or `--source shared` to work with the store.

To restore a link after `unlink`, run `envrun relink path/to/.env --yes` in that worktree. If the local and shared copies differ, `relink` saves the local copy in a private backup and prints its path before linking to the shared file. If they match, it relinks without a backup. The TUI offers the same action with `l` when the file shows `BOTH`.

`envrun remove path/to/.env --yes` deletes only the stored copy, preserving local copies. It lists every blocking worktree if links still exist. Run `envrun unlink path/to/.env` in each listed worktree before retrying. The TUI shows this list before offering a removal confirmation.

To associate a separate clone with an existing named project, run `envrun attach myapp` in that clone. This is explicit because separate clones may need different secrets. The store records attached clones so removal checks their links too. If you move a clone, run `envrun status` there to refresh its recorded path before removing a managed file from another clone.

Use `envrun repositories` to list registered clone IDs and Git directories. After permanently deleting a clone **and retiring all of its worktrees**, `envrun forget-repo <id> --yes` removes its stale registration. This command refuses existing Git directories and preserves all stored env files. Do not forget a temporarily disconnected or moved clone; restore it or refresh its location instead.

## Run commands

```sh
cd services/api
envrun run -- npm run dev
envrun run --file .env.local -- npm run dev
envrun run --file .env.local --explain
envrun run --source local -- npm run dev
envrun run --source shared -- npm run dev
```

Replace `npm run dev` with the command your project uses, and pass `--file .env.local` only if that file exists. `run` loads `.env` files from the repository root through the current directory. Each `--file` adds a file relative to the current directory. Later files override earlier files and inherited shell variables. `--explain` prints paths, key names, and the selected local/shared source without values or command execution. Shell syntax such as pipelines needs an explicit shell, for example `envrun run -- sh -c 'first | second'`.

The same `--source` policy applies to `keys`, `get`, `set`, `unset`, and `run`:

| Source | Selection |
| --- | --- |
| `auto` (default) | Use an envrun link, a shared file missing locally, or an ordinary local file with no shared counterpart. Refuse when independent local and shared copies both exist, even if their bytes match. |
| `local` | Use files in this worktree. An existing envrun symlink still points to shared data and is reported as shared. |
| `shared` | Use the store, ignoring independent local copies. Links are not required. |

Automatic loading includes local-only ancestor `.env` files. Inspect `envrun run --explain` to review the selection. `${NAME}` and `$NAME` expand using prior assignments, earlier files, then inherited shell variables. Expansion is performed once, in file order; undefined references become empty. Single quotes and `\$` keep dollar references literal. Shell expressions such as `${NAME:-default}` are rejected.

## Manage keys and use the TUI

```sh
envrun keys services/api/.env
envrun get services/api/.env DATABASE_URL
envrun get services/api/.env DATABASE_URL --source local
printf %s 'development' | envrun set services/api/.env LOG_LEVEL
envrun unset services/api/.env OLD_KEY
envrun
```

`keys` and `get` can inspect an existing local env file before initialization. Editing requires an initialized repository, but a local file does not have to be imported. `get` writes the parsed value without a trailing newline. `set` reads stdin verbatim, including any newline supplied by the caller. Key edits preserve unrelated assignments and comments, and refuse ambiguous assignment boundaries or duplicate assignments for that key.

`get` and the TUI inspect one file, expanding references against its preceding assignments and the inherited shell. Use `run` when you need expansion across the full ancestor-file chain.

The TUI shows files, keys, and details in separate columns on wide terminals, and adapts to two columns or one pane on smaller terminals. File rows say `LINKED`, `STORED`, `LOCAL`, `BOTH`, `CONFLICT`, or `NO DIR`. The header always shows the selected path and source policy. Press `v` to cycle `Auto → Local → Shared`; for example, `AUTO -> SHARED` means Auto resolved to the shared copy. A linked file also reads the shared copy when Local is chosen. If independent local and shared copies both exist, Auto asks you to choose. An explicit choice is saved per file and worktree, so it survives navigation, refresh, and TUI restart. `Auto` clears that choice. CLI commands continue to use their own `--source` flag or the Auto default.

Use `Tab` or left/right arrows to change panes; use `j`/`k`, up/down arrows, mouse clicks, or the wheel to select rows. On a narrow terminal, `Enter` advances and `Esc` returns. Use `/` to filter paths, `r` to reveal only the selected value, and `F5` to refresh after external changes. Use `i` to select ignored files for import, `a` to add a path, `l` to link or relink a `BOTH` file, `u` to unlink, and `d` to remove a shared copy. Use `n` to add a key, `s` to set its single-line value, `x` to unset it, and `e` to edit the whole file with `$VISUAL`, `$EDITOR`, or `vi`. The editor command may contain whitespace-separated arguments, such as `code --wait`. Press `?` for full help and `q` to quit.

The store contains local plaintext secrets. Directories and stored files are private to the current Unix user. Values stay masked in the TUI until explicitly revealed; errors, status, and `--explain` do not print values.
