# Project Instructions for AI Agents

This file provides instructions and context for AI coding agents working on this project.

## General

- **Never** use `npm`, `npx`, `yarn`, `pnpm` - use `bun`, `bunx` for all package operations
- **NEVER** use internal task tools (TaskCreate, TaskUpdate, TaskList, TaskGet, TodoWrite) — they are forbidden. Use `tk` instead.

## Git Rules

- **NEVER** run `git commit` or `git push` unless the user explicitly says to commit or push
- Do not auto-commit, do not auto-push after fixes — wait for explicit instruction
- Never include "Co-Authored-By" lines in commit messages
- Never mention amount of lines changed, only functional changes
- Keep commit messages concise and descriptive

## Tech Stack

- Tauri (Rust backend + Vanilla JS frontend)
- bun for package management (not npm)
- No bundler - static files served directly

## Audio

- OGG Vorbis encoding via vorbis_rs
- Real-time mixing via shared buffers
- In-app playback via rodio
- System audio capture via Core Audio Process Taps (cidre)
- Mic capture via cpal

## UI/UX Design

- All UI/UX, styling, color, theme, and design tasks must use `ui-ux-pro-max` skill

## Documentation

- Always use `Context7` MCP when library/API documentation, code generation, setup or configuration steps are needed without having to explicitly ask.

## File Operations

- NEVER use Bash redirects (`>`, `>>`, heredoc, `cat > file`, `echo > file`)
- Do NOT write files via shell commands
- Use only Write/Edit tools for file creation or modification

## Build

- Run all commands without prompting for user input unless interaction is **absolutely** required
- **NEVER** run `cargo tauri dev` unless the user explicitly asks — use `cargo check` for compilation verification
- Running the app opens a window and interferes with the user's workflow

```bash
cargo check          # verify compilation (default)
cargo tauri dev      # development (only when user asks)
cargo tauri build    # production
```

<!-- BEGIN NTK INTEGRATION -->

### Tickets

Use only `ntk` CLI to manage tickets (tasks). Key commands:

- `ntk ls` — list tickets
  - `-s <status>` — filter by status (`open`, `in_progress`, `blocked`, `to_test`, `done`)
  - `-a <initials>` — filter by assignee
  - `-t <tags>` — filter by tags (comma-separated)
- `ntk show <id>` — view ticket details
- `ntk start <id>` — mark ticket as in_progress
- `ntk close <id>` — mark ticket as done
- `ntk next` — pick next ticket to work on (`-a`, `-P` to filter)
- `ntk users` — list assignees
- `ntk create <title>` — create a new ticket:
  - `-p <priority>` — `high`, `med`, `low`
  - `-a <initials>` — assignee (default: me)
  - `-s <status>` — default: `open`
  - `-T <type>` — `feature`, `task`, `bug`, `epic`, `constraint`, `scaffold`, `infra`, `chore`, `core` (default: `task`)
  - `-t <tags>` — comma-separated tags
  - `-d <text>` — description
  - `--deps <tid,tid>` — dependency ticket IDs (blocks `ntk next` until all done)
  - `--due <date>` — due date (`YYYY-MM-DD`)
- `ntk update <id>` — update ticket fields:
  - `-s <status>` — change status (`open`, `in_progress`, `blocked`, `to_test`, `done`)
  - `-p <priority>` — change priority (`high`, `med`, `low`)
  - `-a <initials>` — reassign
  - `-T <type>` — change type (`feature`, `task`, `bug`, `epic`, `constraint`, `scaffold`, `infra`, `chore`, `core`)
  - `-t +tag,-tag` — add/remove tags
  - `-P <project>` — change project
  - `-A <text>` — append text to page body
  - `--deps <tid,tid>` — set dependency ticket IDs
  - `--due <date>` — set due date (`YYYY-MM-DD`)

<!-- NTK -->

### Tickets

**CRITICAL:** ALL task management MUST use `ntk` CLI. NO other tools. NO Notion API. NO exceptions.

Drafting depth depends on the task. For trivial one-liners ("just add XXX", "update README", "bump version") — DO NOT scour the codebase, take the task as-is. For substantive tickets — draft the description independently using codebase context. NEVER ask the user to fill in details that can be inferred from the codebase or the internet. Only ask if the info genuinely cannot be found.

**Description Format (`-d`):**
```
## Summary
Business-level what/why. Max 2 sentences.

## Expected Outcome
The concrete result/value delivered when this is done.

## Details
- Implementation specifics, affected files/modules, technical approach
- Edge cases, constraints, dependencies

## Acceptance Criteria
- [ ] Independently verifiable checklist item
- [ ] Independently verifiable checklist item. NO vague "works correctly". Define "correct".
```

**Always pass `-d` (and `-A`) via a heredoc:**
```
ntk create "title" -p med -d "$(cat <<'EOF'
## Summary
...
EOF
)"
```

**Ticket Rules:**
- **Initiative:** Expand user one-liners into full tickets using codebase context — but only when the task warrants it (see above).
- **Clarity:** NEVER create vague tickets. Ask questions FIRST if ACs cannot be written.
- **Closing:** Before running `ntk close` (or moving to `done`) you MUST append a comment via `ntk update <id> -A "..."` describing WHAT WAS DONE — how it was fixed, key files/decisions. No comment, no close.

Project is auto-set via `.ntkrc`. Outside a repo pass `-P <name>` (list via `ntk projects`).

No `.ntkrc` and user named a project? Match it against `ntk projects` (case-insensitive, fuzzy), then pass `-P <matched-name>`.

**Commands:**
- `ntk ls [-s status,status] [-a initials,initials] [-t tags] [--since YYYY-MM-DD]` — List (comma-separated for multiple statuses/assignees; `--since` filters by creation date)
- `ntk show <id> [id...]` — View (pass multiple ids space- or comma-separated)
- `ntk start <id>` — Mark `in_progress`
- `ntk close <id>` — Mark `done`
- `ntk next [-a initials] [-P project]` — Pick next
- `ntk deps <id> | -t <tag>[,tag] [-P proj]` — Show dependency tree for one ticket (with `N/M done`, `[ready]`/`(waiting on N)`) or a forest for all tickets carrying the tag(s); external blockers shown as `↗`
- `ntk users` — List assignees
- `ntk projects` — List projects from Notion (refreshes global config)
- `ntk create <title> [-p high|med|low] [-a initials] [-s open] [-T feature|task|bug|epic|constraint|scaffold|infra|chore|core] [-t tags] [-d text] [-P project] [--deps tid,tid] [--due YYYY-MM-DD]`
- `ntk update <id> [id...]` — Modify (same flags as create + `-d text` to REPLACE body + `-A text` to append + `--title text`). Multiple ids: same flags applied to each; if any id doesn't resolve, nothing is updated. `--deps` accepts `tid,tid` (replace), `+tid,-tid` (add/remove), or `""` (clear); `--due` accepts a date or `""` to clear.

### Reviewing Agent Work

Trigger: user asks "what's done?" / "let's check it" / similar.

Process **one ticket at a time** — never batch. Start with the first ticket in `ntk ls -s to_test -t agent-done`, finish it (GO or NO-GO), then move on.

Queue >1 ticket on separate branches? Review **ONLY in a worktree**: `git worktree add ../<repo>-review <base>`. Drop it when done.

Agent branches are stale — base could change during run. Reconcile overlaps yourself. Escalate only if blocked.

1. `ntk show <id>` — read request + agent log.
2. `git fetch origin && git diff --stat origin/<base>..origin/ntk/<id>` (base = project's main branch).
3. Summarize: what changed, flag anything off-topic or junk (files unrelated to the ticket).
4. Give your own short, concise verdict — one sentence — then ask the user: **GO / NO-GO?**
   - **GO:** sync base (`git switch <base> && git pull --rebase`), `git checkout origin/ntk/<id> -- <task files only>` (skip junk). If file already modified — edit by hand, no `checkout`. Commit + push, `ntk update <id> -s done -A "merged: ..."`, `git push origin --delete ntk/<id>` (only after merge push confirmed).
   - **NO-GO:** show the issues, discuss. Nothing else.

<!-- /NTK -->
