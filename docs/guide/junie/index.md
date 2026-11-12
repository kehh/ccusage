# Junie Data Source (Experimental)

> Junie support is experimental. Expect breaking changes while both ccusage and [Junie](https://www.jetbrains.com/help/junie/) continue to evolve.

ccusage can read Junie session event streams as one of its supported local data sources. Junie uses the same unified and focused report model as Claude Code, Codex, OpenCode, Amp, pi-agent, GitHub Copilot CLI, and Gemini CLI.

## Usage

```sh
# Daily Junie usage
ccusage junie daily

# Weekly and monthly Junie usage
ccusage junie weekly
ccusage junie monthly

# Junie sessions
ccusage junie session

# Include Junie in the default all-source report
ccusage daily
```

## Data Location

ccusage reads session event streams from `JUNIE_HOME` (defaults to `~/.junie`). `JUNIE_HOME` takes a single root directory; unlike most source variables it does not accept a comma-separated list.

```sh
JUNIE_HOME="/path/to/junie" ccusage junie daily
```

Expected files are discovered under:

```text
~/.junie/sessions/index.jsonl                        # session metadata, including projectDir
~/.junie/sessions/session-YYMMDD-HHMMSS-XXXX/
└── events.jsonl                                     # usage event stream
```

Only `events.jsonl` files in direct child directories of `sessions/` are read. `stats-aggregate.json` and `state.json` are ignored because their totals carry no per-call timestamps. A nested `events.jsonl` belongs to an inner Junie session started by a task and is not counted again for the outer root.

## Supported Reports

| Command                 | Description                  | Related Report                          |
| ----------------------- | ---------------------------- | --------------------------------------- |
| `ccusage junie daily`   | Group usage by day           | [Daily Usage](/guide/daily-reports)     |
| `ccusage junie weekly`  | Group usage by week          | [Weekly Usage](/guide/weekly-reports)   |
| `ccusage junie monthly` | Group usage by month         | [Monthly Usage](/guide/monthly-reports) |
| `ccusage junie session` | Group usage by Junie session | [Session Usage](/guide/session-reports) |

Weekly rows start on Monday. Session IDs are the Junie session directory names, such as `session-YYMMDD-HHMMSS-XXXX`.

## Project Filtering

Junie records a `projectDir` for each session in `sessions/index.jsonl`. Use `-p` or `--project` to keep only the sessions whose recorded `projectDir` exactly matches a path. The filter is accepted on every Junie report, and it selects whole sessions before the report aggregates them.

```sh
# Weekly usage for one project directory
ccusage junie weekly --project /Users/me/code/my-app

# Combine the project filter with JSON output
ccusage junie daily --project /Users/me/code/my-app --json
```

Sessions that are missing from `index.jsonl` still load and report under the `unknown` project, so they never match a `--project` path.

On the daily report, `-i` or `--instances` adds per-project rows instead of merging each day into one row. Use `--project-aliases` to replace long paths with readable labels in the table:

```sh
ccusage junie daily --instances
ccusage junie daily --instances --project-aliases "/Users/me/code/my-app=My App"
```

With `--instances --json`, the output groups rows by `projectDir` and adds combined totals:

```json
{
	"projects": {
		"/Users/me/code/my-app": [{ "date": "2026-05-20", "...": "..." }]
	},
	"totals": { "inputTokens": 0, "...": "..." }
}
```

`--instances` and `--project-aliases` shape only the daily report. Weekly and monthly reports merge projects into their date buckets by design, and `--project` remains the way to narrow them. Sessions that are missing from `index.jsonl` group under the `unknown` project key in both the table and `--instances --json`.

Unified reports such as `ccusage daily` include Junie usage, but the `--project` filter on a unified command applies only to Claude Code. Run a focused `ccusage junie` command when you need to filter or group Junie projects.

## Token Mapping

- **Input tokens** - `modelUsage[].inputTokens`
- **Output tokens** - `modelUsage[].outputTokens`
- **Cache read tokens** - `modelUsage[].cacheInputTokens`
- **Cache creation tokens** - `modelUsage[].cacheCreateTokens`

Only `LlmResponseMetadataEvent` records contribute usage. One event can carry several `modelUsage` entries, and each entry becomes its own usage record. A record is skipped only when all four token counts are `0` and its cost is missing or zero.

Junie's legacy format (May–June 2026) omits `timestampMs`. For those records, ccusage uses the session's `createdAt` from `index.jsonl`, then the timestamp encoded in the session directory name, then the file modification time.

## Cost Calculation

Each Junie `modelUsage` entry records its own USD cost. The default `auto` mode keeps that recorded cost exactly, and `display` shows it without recalculation. `calculate` instead re-prices the token counts with LiteLLM pricing for the record's timestamp.

Custom model names, such as self-hosted models, may not have a LiteLLM price. Those records remain visible but show a missing-pricing warning when calculation is required. See [Cost Modes](/guide/cost-modes) for the full mode semantics.

## Environment Variables

| Variable     | Description                                               |
| ------------ | --------------------------------------------------------- |
| `JUNIE_HOME` | Root directory containing Junie data (default `~/.junie`) |

## Troubleshooting

::: details No Junie usage data found
Ensure the data root exists at `~/.junie/sessions/` and that each session directory contains an `events.jsonl` file with `LlmResponseMetadataEvent` records. Set `JUNIE_HOME` if your Junie data lives elsewhere. If data exists but a focused project report is empty, check that `--project` matches the `projectDir` stored in `sessions/index.jsonl` exactly.
:::

For JSON shapes shared by every report, see [JSON Output](/guide/json-output).
