# envrun TUI design

Status: implemented in `src/tui.rs` and `src/tui/view.rs`.

## Goal

Make it easy to answer three questions before editing a secret: **which file**, **which copy** (local or shared), and **which key**. Keep the full file and key lists useful in a monorepo. Preserve keyboard and mouse access, and keep values masked until a deliberate reveal.

The existing CLI, file selection rules, and safety checks remain the source of truth. The TUI continues to inspect one file at a time. `envrun run --explain` remains the way to inspect the ancestor-file load chain.

## Main screen

The wide layout has three quiet columns. Only the active column gets an accent marker. One muted divider separates each column; individual lists do not get full-height boxes. The detail column has a small value card, leaving room for source and file status. The workspace is capped at roughly 132 columns by 34 rows and centered in larger terminals.

Illustrative data and masked value:

```text
envrun  /  myapp                     worktree: feat-search                    ? help
services/api/.env                  LINKED  ·  AUTO → SHARED               F5 refresh
────────────────────────────────────────────────────────────────────────────────
FILES  8                      KEYS  22                       DETAILS
› services/api/.env  LINKED    › API_TOKEN                   API_TOKEN
  services/gateway/.env STORED   BASE_URL                    services/api/.env
  services/worker/.env LOCAL     LOG_LEVEL                   Source  [Auto]  Local  Shared
  services/worker/.env.dev BOTH  MODEL                       Using shared copy
  rust/accounting/.env LINKED   TIMEOUT                     Edits affect linked worktrees.
  ...                           ...
  a Add  i Import  l Link/Relink
  u Unlink  d Remove
                                                            Value
                                                            ••••••••  [r Reveal]

                                                            [s Set]  [x Unset]  [e Edit file]
────────────────────────────────────────────────────────────────────────────────
Keys  ↑↓ move  Tab pane  / filter  v source  ? help                   q quit
```

The path and source line stays visible while moving through keys. `AUTO → SHARED` states both the selected policy and the actual resolved copy. If `Local` is selected on an envrun symlink, it still reads the shared copy and displays `LOCAL → SHARED`.

File labels use words plus restrained color: `LINKED` (link to store), `STORED` (available in store, no link here), `LOCAL` (only ordinary local file), `BOTH` (independent local and shared copies), `CONFLICT` (tracked or otherwise unsafe local path), and `NO DIR` (the managed file's parent directory is absent). A status or path error uses `ERROR` and a specific recovery message. Long paths retain the filename and truncate the middle, with the complete path on the selected-file line. No state depends on color alone.

### Conflict state

`AUTO` does not choose between independent copies. The key list is replaced by an action card; no value or edit action is available until a source is chosen.

```text
services/worker/.env.dev          BOTH  ·  choose a source
────────────────────────────────────────────────────────────
Two independent copies exist.

[Local]  This worktree only
[Shared] Every linked worktree

Choose with v or click a source. Nothing is edited automatically.
```

When a source is chosen, keys and values come from that copy only. Explicit choices persist per file and worktree across navigation, refresh, and restart. Choosing `Auto` clears the preference. Changing files hides any revealed value. The chosen source stays visible even when a success or error message appears.

### Narrow layouts

At 72–109 columns and at least 22 rows, use two columns: files on the left (about one third), keys and detail stacked on the right. The detail area gets at least eight rows and grows when the key list is short. At 50–71 columns, or any width with only 16–21 rows, show one pane at a time: Files → Keys → Detail. `Enter` advances, `Esc` returns, and `Tab` cycles panes. Below 50 columns or 16 rows, show `Terminal too small; need 50×16` and keep `q` working. Resize preserves the selected file, key, source, filter, and focus; revealed values are hidden on layout change.

Illustrative narrow view:

```text
envrun / myapp           [Keys]       ?
services/api/.env  AUTO → SHARED
──────────────────────────────────────
› API_TOKEN
  BASE_URL
  LOG_LEVEL
  MODEL

Selected: API_TOKEN
Value: ••••••••  r Reveal
──────────────────────────────────────
Esc files  Tab pane  s set  ? help
```

No horizontal scroll is required for commands. Long key names truncate in lists; the complete selected key appears in the detail view when space permits.

## Interaction

| Action | Keyboard | Mouse |
| --- | --- | --- |
| Move through files or keys | `↑`/`↓`, `j`/`k` | Click row; wheel scrolls the list under the pointer |
| Change pane | `Tab`, `Shift+Tab`, `←`/`→`; `Enter`/`Esc` in single-pane layout | Click pane |
| Choose source | `v` cycles Auto → Local → Shared | Click source choice |
| Filter paths | `/`; `Esc` clears an active filter | Click filter control |
| Reveal selected value | `r` toggles | Click Reveal/Hide |
| Edit selected key | `s` set, `x` unset, `n` new | Click visible action |
| Edit whole file | `e` | Click Edit file |
| Manage files | `i` import, `a` add, `l` link or relink, `u` unlink, `d` remove | Click corresponding action in file context or `?` help |
| Refresh/help/quit | `F5`, `?`, `q` | Click visible control where shown |

The footer shows only actions relevant to the focused pane and always shows `? help` and `q quit`. The help overlay lists every key and mouse action, defines file status labels, and explains local/shared effects. `Esc` closes an overlay; in the ordinary wide view it clears an active filter, then returns focus to Files. `q` is the explicit quit key.

Unavailable source choices remain visible but show the reason they cannot be used (`No local copy` or `Not managed`). They do not expose keys or enable edits. Removal uses the same checked preflight as the CLI. If links block deletion, the confirmation is replaced with a scrollable worktree list; if none block it, the confirmation names the shared file and states that independent local copies remain.

Inline value input remains single-line and masked. The external editor remains the path for multiline values. It should also be available to repair a file whose dotenv syntax cannot be parsed, provided the file source resolves safely; saving still requires valid syntax and the existing unchanged-file check. Prompt and confirmation dialogs appear near the center of the workspace with a visible cursor and explicit `Enter`/`Esc` or `Confirm`/`Cancel` actions. Mouse selection never reveals a value by itself.

## Empty, error, and safety states

| State | On-screen response |
| --- | --- |
| No project | `No project opened` with `n New project` and, when available, `a Attach` |
| Project with no files | `No env files yet` with `i Import ignored files` and `a Add path` |
| File with no keys | `No keys in this file` with `n Add key` |
| Filter matches nothing | `No files match …` with `Esc Clear filter` |
| Both copies present | Source choice card above; no automatic read or edit |
| File parse error | Name the file and offer `e Edit file` and `F5 Refresh`; never print file contents |
| Remove blocked | Scrollable list of every linked worktree, with `Unlink in each worktree first` and `Esc Close` |
| Successful action | One concise status line; persistent source and file status remain visible |

Values are masked with a constant-length marker; the UI never reveals length from the mask. Reveal applies only to the selected key. Changing key, file, source, pane, filter, or terminal layout hides the value. Errors, help, tooltips, and status never include values. The reveal area scrolls for long or multiline values and makes line breaks visible without leaking text outside its bounds.

Use the terminal's default foreground/background where possible. Reserve cyan for focus, yellow for warnings, and red for errors; words and selection markers carry the same meaning without color. Respect `NO_COLOR`. Do not rely on emoji. Use the same rendering checks at 50×16, 60×20, 80×24, 100×30, and 140×40, including a light terminal theme.

## Implementation notes

The TUI uses `project.rs` and `envfile.rs` for file selection, editing, and safety checks. Rendering and mouse hit testing share layout calculations. The test suite covers terminal sizes, source selection, masking, and removal checks.
