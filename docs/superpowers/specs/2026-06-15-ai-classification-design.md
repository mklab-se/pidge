# AI E-mail Classification & Native Categories — Design

**Date:** 2026-06-15
**Status:** Approved (brainstorm), pending implementation plan

## 1. Goal

Let a pidge user (or, primarily, an AI agent driving pidge) ask "what is this
e-mail?" and get back one or more labels computed by the user's configured AI
provider (via ailloy), against a user-defined prompt. The label(s) are a
**string set returned on stdout** so the caller can act on them — most often an
agent that then moves the message to a folder. Optionally, the computed labels
can be written to the message's **native Outlook categories**, turning the
feature into a simple AI rule-engine for labelling mail.

### Primary use case

An agent runs `pidge ai classify <hash> --prompt "…"`, reads the label from
stdout, and decides what to do (e.g. `pidge mail move <hash> --to Kvitton/MKLab`).
Persisting the classification is **optional** — the agent holds it in context.

## 2. Non-goals (this iteration)

- **Sorting/moving by category** (e.g. `mail move --by-category`) — deferred to
  its own follow-up spec.
- **Named classifier profiles** (multiple saved prompts) — a single default
  classifier config is enough for v1; profiles can be added later as new config
  keys without new commands.
- Training, embeddings, or any non-chat AI use.

## 3. Core concepts

- **Classification is multi-label.** A message can be both a `receipt` and a
  `ticket`. `classify` returns an ordered set of labels, never a single scalar.
  Outlook categories are natively an array, so this maps directly.
- **Classification (compute) is separate from categorization (store).**
  `pidge ai classify` computes labels with AI. `pidge categorize` manages the
  native Outlook `categories` field with no AI. `--set-category` bridges them.
- **Label precedence/dedup is a *sorting* concern, not a classification one.**
  classify reports everything that applies; the deferred sort-by-category spec
  (or the agent) decides precedence (e.g. "ticket beats receipt").

## 4. Command surface

### 4.1 `pidge ai classify` — compute label(s)

```
pidge ai classify [<fragment>] [--text <s>] [FILTERS] [--prompt <s>|--prompt-file <p>]
                   [--labels a,b,c] [--parallel <n>] [--no-cache]
                   [--set-category] [--account <e>] [--json]
```

Three input modes (mutually exclusive selection of *what* to classify):

1. **Single message:** `pidge ai classify 234ab` — classify one message by
   short-hash fragment. Prints the labels (one per line), or `--json`.
2. **Arbitrary text (prompt test):** `pidge ai classify --text "Your invoice #1…"`
   — classify a literal string, no mailbox needed. This is the "does my prompt
   return something useful?" path requested for `pidge ai`.
3. **Batch:** filter flags select a set of messages; each is classified
   (concurrently). `--json` recommended for agents.

**Filters (batch mode, mirror existing bulk ops):**
- `--from <addr>` (repeatable)
- `--older-than <spec>` (`30d`, `6m`, `2026-01-01`, …)
- `--folder <path>` (classify within a folder, incl. nested like `Kvitton/MKLab`)
- `-n, --limit <n>` (cap the number classified)
- `--account <email>` (repeatable; default all signed-in)

**Behaviour flags:**
- `--prompt <s>` / `--prompt-file <p>` (`-` = stdin) — overrides the configured
  default prompt for this run. Required only if no default prompt is configured.
- `--labels a,b,c` — allowed set; each returned label is validated against it.
  In-set labels are kept; if *none* are in-set, the result is `["unknown"]`.
  Without `--labels`, the model's raw labels are returned as-is.
- `--parallel <n>` — max concurrent AI calls in batch mode (overrides config).
- `--no-cache` — bypass the classification cache for this run.
- `--set-category` — after computing, write the label set to the message's
  native Outlook categories (replace). No-op in `--text` mode.
- `--json` — structured output.

**Output:**
- Single / text mode (human): one label per line.
- Single / text mode (`--json`): `{"hash": "234ab", "classification": ["receipt","ticket"]}`
  (hash omitted / null for `--text`).
- Batch (human): `<hash>  <from>  <labels joined by ", ">` per line.
- Batch (`--json`): `[{"hash": "…", "from": "…", "classification": [...]}, …]`.

### 4.2 `pidge categorize` — native Outlook categories (no AI)

```
pidge categorize <fragment>                 # show current categories (default)
pidge categorize set   <fragment> <label>…  # replace categories with these
pidge categorize add   <fragment> <label>…  # add label(s), keeping existing
pidge categorize clear <fragment>           # remove all categories
```

Backed by Graph: `GET …/messages/{id}?$select=categories` and
`PATCH …/messages/{id}` with `{ "categories": [...] }`.

### 4.3 `pidge config` — read/write pidge's own settings (git-style)

```
pidge config show                                   # effective config
pidge config get <key>                              # one value
pidge config set <key> <value>                      # scalar / comma-list
pidge config set classify.prompt --file <path>      # multi-line value from file
pidge config set classify.prompt -                  # …from stdin
pidge config unset <key>                            # revert key to built-in default
```

Keys (v1): `classify.prompt`, `classify.parallel`, `classify.cache`,
`classify.labels`. Generic dotted-key store so future settings need no new
commands. Distinct from `pidge ai config` (which configures the AI *provider*
via ailloy); `--help` cross-references the two.

## 5. Config schema & precedence

Extends the existing `~/.config/pidge/config.yaml` (`Config` already has a
`defaults` block; add a sibling `classify` block, `#[serde(default)]`):

```yaml
classify:
  prompt: |
    Reply with a comma-separated list of every label that applies:
    invoice, receipt, ticket, outgoing-invoice. Reply "other" if none apply.
  parallel: 8
  cache: true
  labels: [invoice, receipt, ticket, outgoing-invoice, other]
```

**Precedence for every setting:** CLI flag → `classify.*` in config →
built-in default. Built-in defaults: `parallel = 4`, `cache = true`,
`prompt = none` (error if unset and not passed), `labels = none` (no validation).

## 6. Architecture

New CLI command modules under `crates/pidge/src/commands/`:
- `mail_categorize.rs` — `get` / `set` / `add` / `clear`.
- `ai_classify.rs` — input-mode dispatch, batch concurrency, output rendering.
- `config.rs` — `show` / `get` / `set` / `unset` over the config struct.

New `pidge-client` (Graph) methods:
- `get_categories(account, id) -> Vec<String>`
- `set_categories(account, id, &[String])` (PATCH).

New `pidge-core` config:
- `ClassifyConfig { prompt: Option<String>, parallel: Option<usize>, cache:
  Option<bool>, labels: Vec<String> }` on `Config`, with a typed key-path
  get/set/unset used by `pidge config`.

**AI seam (testability):** a small `Classifier` abstraction wraps the ailloy
call:
```
trait LabelModel { async fn classify(&self, prompt: &str, body: &str) -> Result<String>; }
```
- Production impl: `ailloy::Client::for_capability("chat")` →
  `client.chat(&[Message::user(<prompt>\n\n<email text>)])` → `response.content`.
- The **response parser** (`content` → `Vec<String>`: split on comma/newline,
  trim, lowercase-compare for validation, JSON-array tolerant, dedup
  preserving order) is a pure function, unit-tested without a model.

**Email text fed to the model:** subject + sender + plain-text body
(`body_as_plain_text`, already used for `--json`), truncated to a sane cap
(e.g. 4000 chars) to bound token use.

**Batch concurrency:** reuse the `futures::stream::buffer_unordered(parallel)`
pattern already used by bulk move/archive. Each task: resolve message → cache
lookup → (miss) call model → parse → optional `--set-category`.

**Cache:** JSON map in the cache dir (`…/pidge/classify-cache.json`) keyed by
`"{graph_id}:{sha256(prompt)[..16]}"` → `Vec<String>`. Read before calling the
model; write after. `--no-cache` / `classify.cache = false` skips both sides.
Caching is best-effort: a corrupt/missing cache is ignored, never fatal.

## 7. Error handling

- AI not configured / disabled → actionable error pointing to `pidge ai config`
  / `pidge ai enable` (reuse ailloy's `is_ai_active`).
- No prompt (no flag, no config default) → clear error naming
  `pidge config set classify.prompt`.
- Model returns empty / unparseable → `["unknown"]` (with a stderr note), so a
  batch never aborts on one bad row.
- Per-message failures in batch are reported per row and don't abort the run
  (mirrors bulk move/archive); exit status reflects whether *any* succeeded.
- Graph throttling (429) on `--set-category` reuses the existing
  exponential-backoff retry helper.

## 8. Testing

- **Pure parser:** comma/newline/JSON parsing, trimming, dedup-order, empty →
  `unknown`.
- **Allowed-set validation:** in-set kept, none-in-set → `unknown`, case-insensitive.
- **Config precedence:** flag > config > built-in for each key; `set`/`get`/
  `unset` round-trips; multi-line prompt via file/stdin.
- **Graph categories:** wiremock for `get`/`set` (PATCH body shape).
- **Classifier seam:** a fake `LabelModel` drives `ai_classify` batch logic
  (concurrency, cache hit/miss, `--set-category`) with no live model.
- Existing CI gates: `cargo fmt`, `clippy -D warnings`, `cargo test --workspace`.

## 9. Future work (separate specs)

- **Sort/move by category:** `pidge mail move --by-category <c> --to <folder>`,
  and/or `mail list/search --category <c>`.
- **Named classifier profiles:** `classifiers.<name>.{prompt,labels}` + a
  `--classifier <name>` selector.
