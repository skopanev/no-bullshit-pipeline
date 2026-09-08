# Pipelines — model

**What a pipeline is.** A named, ordered chain of self-contained steps that
runs against a recording's transcript (or a dictation's text). There is no
separate "Connection" registry and no networked delivery connectors — that
complexity was removed after talking to real users. The only built-in
destination is **Save to folder**; for anything networked (Notion, Slack, an
HTTP endpoint) write a **Shell** step that does it (`curl`, the vendor CLI, …).

```
Transcript ─▶ Step 1 ─▶ Step 2 ─▶ … ─▶ Step N
              (cli|shell|save_local)
```

## Step

Each step is fully self-contained — it carries its own type, inline config,
and template. No shared/reusable objects, no credential indirection.

```rust
PipelineStep {
  name,                  // unique within the pipeline; filesystem-safe
  step_type: StepType,   // CliAgent | Shell   (serde alias: connection_type)
  template: String,      // CLI: the prompt · Shell: the bash script body
  config: serde_json::Value,  // inline, non-secret, per-type (see below)
  description: Option<String>,
}
```

### `cli_agent`

Runs a locally-installed coding CLI (Claude Code / Codex / OpenCode / agy).

- `config`: `{ cli, model?, timeout_secs?, working_directory? }`
- `template`: the prompt. The engine substitutes placeholders (below) before
  the agent sees it, then injects the result as `config.prompt`.

### `shell`

Script-mode: the step IS a bash script. The script is **not** placeholder-
substituted (avoids shell-injection from transcript content) — raw values
arrive via env vars instead.

- `config`: `{ cwd (required), shell?=/bin/bash, env?={K:V}, timeout_secs?=120 }`
- `template`: the script body. Stdout becomes the step's output.
- env: `NBP_TRANSCRIPT`, `NBP_PROCESSING_RESULT` (previous step output, empty
  on step 1), `NBP_APP` (friendly app name).

### `save_local`

Write the step's content to a local folder — the one built-in destination.

- `config`: `{ folder_path (required) }` (`~` expanded).
- `template`: encodes WHAT to save — `{processing_result}` (default) or
  `{transcript}`, picked via a radio in the editor. The engine renders it, so
  the connector just writes the resulting string.
- File lands at `<folder>/<app> <date> <start-time>.md` (e.g.
  `Zoom 2026-06-01 14-30.md`), named after the recording's app + local start
  time (collision-suffixed). The step's chained output is `Saved to <path>`
  (save is normally terminal).

## I/O contract

Placeholders for CLI templates (missing keys render as empty string):

| Placeholder | Value |
|---|---|
| `{transcript}` | full recording transcript |
| `{processing_result}` | previous step's output (empty on step 1) |
| `{app}` | friendly app name (Zoom / FaceTime / NBP / …) |

The chain is **strictly linear**: step _i_ eats the transcript (step 1) or
step _i-1_'s output. Every step writes its output as a `<step>.md` artifact
into the recording's `pipelines/<pipeline>/` folder — so **save-to-folder is
the base behaviour**, not a separate step.

## Failure semantics

A failed step halts everything downstream (later steps can't run without the
prior step's output) and marks the run `Partial`. A run with no failures is
`Done`. New pipelines must contain at least one step; pipeline definitions are
not used as recording tags.

## Storage ownership

- `~/.nbp/pipelines.json` is the only source of truth for pipeline definitions.
- `~/.nbp/settings.json` stores only name references (default, last used, and
  dictation shortcut selection).
- A recording's `metadata.json` stores only actual pipeline run history.
- Frontend arrays are reloadable snapshots, not an independent cache or store.

Definition reads are side-effect free. Create, edit, rename, and delete are
serialized backend operations and replace `pipelines.json` atomically. Renames
also update settings references in the same command.

## Dictation

Quick Dictate runs the **same** pipeline as recordings, through the same
`pipeline_engine::run_one_step` (no separate dispatch). Whatever pipeline the
user picks runs in full:
- `cli_agent` / `shell` — transforms; their output becomes the running text.
- `save_local` — side-effect; writes to its folder and leaves the text alone.

Differences from a recording, all in a thin wrapper: there's no recording, so
connector artifacts go to a throwaway temp dir, `{app}` = "Dictation", and the
**pasted** value is the last transform output (a trailing save doesn't change
it). A save-step failure is non-fatal (the paste shouldn't be lost); a
transform failure falls back to pasting the raw transcript.

## Legacy migration

Storage schema v1 runs once at startup. It moves a legacy `pipelines.json` out
of the recordings directory, removes obsolete recording `tags`, removes only
the exact zero-step pipeline definitions/states previously synthesized from
those tags, and clears settings references that point to missing definitions.
The version is recorded only after all writes succeed, so an interrupted pass
is safe to retry. Normal recording reads never perform this migration.

Pre-simplification pipelines referenced removed networked-delivery types
(notion/slack/telegram/webhook) and a `connection_id` field. `load_pipelines`
parses loosely and drops any step whose type isn't `cli_agent`/`shell`/
`save_local`; `connection_id` is ignored. Surviving steps are kept (a legacy
`save_local` step loses its folder — it lived on the Connection — so the user
re-picks it). Invalid JSON or an invalid remaining pipeline is reported and is
never overwritten by a later save. There is no Connections tab or Keychain for
connectors.
