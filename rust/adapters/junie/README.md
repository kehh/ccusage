# ccusage-adapter-junie

The Junie adapter: it turns `events.jsonl` session streams under `~/.junie`
into the usage entries the reports render.

## Owns

- `paths.rs` — root resolution and `sessions/<session>/events.jsonl` discovery
- `parser.rs` — `LlmResponseMetadataEvent` admission, `modelUsage` expansion,
  timestamp resolution
- `loader.rs` — parallel file reads, global dedupe, `load_entries`
- `report.rs` — daily / weekly / monthly / session summary shapes

Anything that is not specific to this source belongs in `ccusage-core` or
`ccusage-adapter-common` instead.

## Data source

```text
$JUNIE_HOME/            # or ~/.junie
└── sessions/
    ├── index.jsonl     # one line per session: sessionId, createdAt, projectDir
    └── session-YYMMDD-HHMMSS-XXXX/
        ├── events.jsonl        # PRIMARY (LlmResponseMetadataEvent modelUsage)
        ├── stats-aggregate.json # NOT used (totals without timestamps)
        └── state.json           # NOT used
```

Runtime root priority: a non-empty `JUNIE_HOME` → `~/.junie` (Junie's bundled
`junie-cli-user-disk-storage.md` defines `JUNIE_HOME` and the default home).
Path discovery stays inside the adapter, matching Junie's official environment
variable and default home.

`stats-aggregate.json` is deliberately not a source: it carries totals without
timestamps, so it cannot bucket by date; `events.jsonl` is the only file with
per-call time, model, tokens, and cost.

## Event format

Usage lives in `events.jsonl` lines whose `agentEvent.kind` is
`LlmResponseMetadataEvent`. Current Junie (1668+) nests usage under
`event.agentEvent.modelUsage` with a top-level `timestampMs`; legacy Junie
(1588.x, May–June 2026) lacks `timestampMs`, `agent`, and `taskId`, so those
lines bucket under the session's creation time instead (index `createdAt`, else
the `session-YYMMDD-HHMMSS-*` directory name, else file mtime). One line can
carry several `modelUsage` entries (one per model hit during the response
round); each becomes its own usage record.

## Token mapping

| Junie field         | ccusage field                 |
| ------------------- | ----------------------------- |
| `inputTokens`       | `input_tokens`                |
| `outputTokens`      | `output_tokens`               |
| `cacheInputTokens`  | `cache_read_input_tokens`     |
| `cacheCreateTokens` | `cache_creation_input_tokens` |

Each `modelUsage.cost` is Junie's recorded USD. `display` and the default
`auto` report what Junie recorded; `calculate` (and `auto` for entries without
a recorded cost) recompute from token × LiteLLM pricing. Custom model names
may surface missing-pricing warnings — accepted, standard behavior.

## Public surface

- `loader::load_entries`
- `report::summarize_entries`
- `run`

## Depends on

- `ccusage-adapter-common`
- `ccusage-core`
- `ccusage-report`
- `jiff`
- `serde`
- `serde_json`

## Testing

Unit tests synthesize `events.jsonl` fixtures (via `ccusage-test-support`)
rather than committing real session trees, which would embed absolute paths
and user prompts. Coverage spans path discovery, current and legacy event
formats, multi-model expansion, timestamp resolution, cost-mode semantics,
dedupe, and the four report shapes.

```sh
# Requires CCUSAGE_PRICING_JSON_PATH (or Nix) for the embedded LiteLLM snapshot.
cargo test -p ccusage-adapter-junie
```

## Live smoke

```sh
cargo test -p ccusage --test junie_cli -- --ignored
```
