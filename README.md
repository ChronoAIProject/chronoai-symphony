# chronoai-symphony

A multi-agent coding orchestrator built on the [Symphony Service Specification](https://github.com/openai/symphony/blob/main/SPEC.md), extended with multi-agent pipelines, native Claude Code support, and a live operations dashboard.

Symphony turns GitHub Issues into autonomous coding sessions. It polls your repository, dispatches coding agents (OpenAI Codex or Claude Code) to work on issues in isolated workspaces, manages the full lifecycle from implementation through code review to human approval, and provides real-time observability through a web dashboard.

**Key features beyond the Symphony spec:**

- **Multi-agent pipelines** - Different agents for different workflow phases (e.g., Codex implements, Claude reviews)
- **Native Claude Code CLI** - Direct integration with `claude -p`, no third-party wrappers
- **Custom pipeline stages** - Define any workflow state with its own agent, role, and prompt
- **Per-stage prompts** - Each pipeline stage can have its own prompt template
- **Live dashboard** - Real-time activity feed, token usage, rate limits, approval queue
- **GitHub App auth** - Bot identity for commits/PRs with auto-refreshing tokens
- **PR review cycle** - Automated code review → human review → rework loop

**Core capabilities:**

- Polls GitHub Issues and dispatches agents based on labels and state
- Creates isolated per-issue workspaces with feature branches
- Runs multiple agents in parallel on different issues
- Manages retries with exponential backoff and stall detection
- Streams agent activity to a web dashboard with approve/deny controls
- Tracks token usage and rate limits across both Codex and Claude
- Hot-reloads WORKFLOW.md changes without restart

## Agent-Assisted Setup

The fastest way to set up Symphony for your project is to ask your coding agent to do it. Paste this prompt into Claude Code, Codex, or any coding agent:

```
Set up Symphony for my repository based on
https://github.com/ChronoAIProject/chronoai-symphony/blob/main/README.md

My repository: <owner>/<repo>
Tech stack: <your stack, e.g., "Rust + React", "Python + FastAPI", "Node.js + Next.js">
```

The agent will read this README and create a tailored `WORKFLOW.md` with the right hooks, prompt template, and architecture rules for your project.

## Quick Start

### Prerequisites

- [Rust 1.94+](https://rustup.rs/) (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`)
- [Codex CLI](https://github.com/openai/codex) with `app-server` support (`codex app-server` must work)
- A GitHub repository with issues to process
- A GitHub personal access token (see [Token Permissions](#token-permissions) below)

### 1. Set environment variables

```bash
export GITHUB_TOKEN=ghp_your_token_here
```

### 2. Create a WORKFLOW.md in your project

Copy the included [WORKFLOW.md](WORKFLOW.md) to your project root and edit it:

```bash
# From the chronoai-symphony repo
cp WORKFLOW.md /path/to/your/project/WORKFLOW.md
```

Then update these fields:
- `tracker.project_slug` - your `owner/repo`
- `hooks.after_create` - your git clone URL and build steps
- `hooks.before_run` - your dependency install commands
- The prompt body - your project's tech stack, architecture rules, and instructions

See the [full config reference](#full-workflowmd-reference) below for all available settings.

### 3. Install and run Symphony

Symphony requires `codex` CLI to be installed and configured on the host machine (it launches `codex app-server` as a subprocess). Docker is available for deployment but requires codex to be available inside the container.

**Install from source (recommended):**

```bash
# Clone the repository
git clone https://github.com/ChronoAIProject/chronoai-symphony.git
cd chronoai-symphony

# Install the symphony binary
cargo install --path crates/symphony-cli

# Run (from your project directory where WORKFLOW.md is)
cd /path/to/your/project
symphony ./WORKFLOW.md --port 8080 --pretty-logs

# Dashboard at http://localhost:8080
```

**Run without installing:**

```bash
# From the chronoai-symphony repo directory
cargo run -- /path/to/your/project/WORKFLOW.md --port 8080 --pretty-logs
```

**With Docker (requires codex installed inside the container):**

```bash
# Create a .env file
echo "GITHUB_TOKEN=ghp_your_token_here" > .env

# Start with Docker Compose
docker compose up -d

# View logs
docker compose logs -f
```

> **Note:** The Docker image does not include codex. You need to either mount the codex binary into the container or build a custom image with codex pre-installed. For most setups, running directly on the host with `cargo install` is simpler.

## Setup Guide for Your Project

This section explains how to integrate Symphony into an existing repository so a coding agent can autonomously work on your GitHub issues.

### Step 1: Label your GitHub Issues

Symphony maps issue states using GitHub labels. Create these labels in your repository:

| Label | Purpose | Symphony State |
|-------|---------|---------------|
| `todo` | Issue ready for agent to pick up | Active (dispatched) |
| `in-progress` | Agent is working on it | Active (tracked) |
| `code-review` | PR created, automated review in progress | Active (review agent) |
| `human-review` | Automated review passed, needs human approval | Active (handoff) |
| `rework` | Reviewer requested changes on the PR | Active (agent addresses feedback) |
| `done` | Work is complete | Terminal (workspace cleaned) |
| `cancelled` | Issue abandoned | Terminal (workspace cleaned) |

An open issue with **no workflow label** defaults to state `Todo`.
A **closed** issue defaults to state `Done`.

**Review lifecycle:**
```
Todo → In Progress (Codex) → Code Review (Claude) → Human Review → Done
                                    ↑                       |
                                    └── Rework (Codex) ←────┘
```

1. Implementation agent finishes and adds `code-review`
2. Review agent (e.g., Claude) reviews the PR, either approves (`human-review`) or requests changes (`rework`)
3. If rework: implementation agent addresses feedback, moves back to `code-review`
4. If approved: human reviews, then adds `done`

### Step 2: Write your WORKFLOW.md

Place a `WORKFLOW.md` file in your project root. It has two parts:

**YAML front matter** (between `---` delimiters) configures the runtime.
**Markdown body** is the prompt template sent to the coding agent for each issue.

#### Minimal WORKFLOW.md

```markdown
---
tracker:
  kind: github
  api_key: $GITHUB_TOKEN
  project_slug: your-org/your-repo
---

Fix issue {{ issue.identifier }}: {{ issue.title }}.

{{ issue.description }}
```

#### Full WORKFLOW.md reference

```yaml
tracker:
  kind: github                          # Required. Only "github" supported.
  project_slug: owner/repo             # Required. GitHub owner/repo.
  endpoint: https://api.github.com     # Optional. Default shown.

  # Auth option 1: Personal access token
  api_key: $GITHUB_TOKEN               # Supports $VAR env references.

  # Auth option 2: GitHub App (commits/PRs show as "app-name[bot]")
  # app_id: $GITHUB_APP_ID             # Supports $VAR env references.
  # installation_id: $GITHUB_APP_INSTALLATION_ID
  # private_key_path: $GITHUB_APP_PRIVATE_KEY_PATH
  active_states:                        # Optional. Default: Todo, In Progress.
    - Todo
    - In Progress
    - Human Review                       # Agent waits; re-dispatched on Rework.
    - Rework                             # Agent reads PR feedback and fixes.
  terminal_states:                      # Optional. Default shown.
    - Done
    - Closed
    - Cancelled
    - Canceled
    - Duplicate

polling:
  interval_ms: 30000                    # Optional. Poll every 30s (default).

workspace:
  root: /tmp/symphony_workspaces       # Optional. Supports ~ and $VAR.

git:
  user_name: symphony-bot              # Git author name for agent commits.
  email: symphony@your-org.com         # Optional. Git author email.

hooks:
  after_create: |                       # Runs once when workspace is first created.
    git clone --depth 1 https://github.com/owner/repo.git .
  before_run: |                         # Runs before each agent attempt.
    git fetch origin
    BRANCH="symphony/issue-${SYMPHONY_ISSUE_NUMBER}"
    if git show-ref --verify --quiet "refs/remotes/origin/$BRANCH"; then
      git checkout "$BRANCH" && git pull origin "$BRANCH"
    elif git show-ref --verify --quiet "refs/heads/$BRANCH"; then
      git checkout "$BRANCH"
    else
      git checkout main && git pull
      git checkout -b "$BRANCH" origin/main
    fi
  after_run: |                          # Runs after each attempt (failures ignored).
    echo "done"
  before_remove: |                      # Runs before workspace deletion (failures ignored).
    echo "cleaning up"
  timeout_ms: 300000                    # Hook timeout. Default: 60s.

agent:
  default: codex                       # Which agent profile to use by default.
  max_concurrent_agents: 10            # Global concurrency limit. Default: 10.
  max_turns: 20                         # Max turns per agent session. Default: 20.
  max_retry_backoff_ms: 300000         # Max retry delay. Default: 5 minutes.
  auto_merge: false                    # Auto-merge after approval. Default: false.
  require_label: symphony              # Only dispatch issues with this label.
                                        # Prevents public users from triggering runs.
  max_concurrent_agents_by_state:      # Optional per-state concurrency limits.
    in progress: 5
    todo: 3

# Named agent profiles. Add `agent:<name>` label to an issue to override.
agents:
  codex:
    command: codex app-server          # Launch command.
    approval_policy: never             # never, on-request, granular, etc.
    thread_sandbox: workspace-write
    model: gpt-5.3-codex              # Passed as --model flag + env var.
    reasoning_effort: xhigh            # Passed as --config flag + env var.
    network_access: true               # Sandbox network access. Default: true.
    turn_timeout_ms: 3600000           # Turn timeout. Default: 1 hour.
    read_timeout_ms: 5000              # Handshake timeout. Default: 5s.
    stall_timeout_ms: 300000           # Inactivity timeout. Default: 5 min.
  claude:
    agent_type: claude-cli             # Native Claude Code CLI integration.
    command: claude                    # Official CLI, no wrapper needed.
    model: claude-sonnet-4-6           # Passed as --model flag.
    reasoning_effort: high             # Passed as --effort flag. low/medium/high/max.
    approval_policy: never             # "never" → --dangerously-skip-permissions.
                                        # Omit or set other value to let Claude prompt.
    max_turns: 20                      # Passed as --max-turns flag.
    network_access: true
    turn_timeout_ms: 7200000           # 2 hours for full Claude session.

# Optional: custom pipeline stages (replaces agent.by_state when set)
# pipeline:
#   stages:
#     - state: in-progress               # Stage per state.
#       agent: codex                     # Agent profile name, or "none".
#       role: implementer               # {{ stage.role }} in prompts.
#       transition_to: code-review      # {{ stage.transition_to }}.
#     - state: code-review
#       agent: claude
#       role: reviewer
#       prompt: "Custom prompt..."       # Replaces WORKFLOW.md body.
#       transition_to: human-review
#       reject_to: rework               # {{ stage.reject_to }}.
#     - state: human-review
#       agent: none                      # No agent dispatched.

server:
  port: 8080                            # Enable HTTP dashboard on this port.
```

### Hook environment variables

Hooks receive these environment variables for the current issue:

| Variable | Example | Description |
|----------|---------|-------------|
| `SYMPHONY_ISSUE_ID` | `#68` | Issue ID |
| `SYMPHONY_ISSUE_IDENTIFIER` | `#68` | Human-readable identifier |
| `SYMPHONY_ISSUE_NUMBER` | `68` | Issue number (without `#`) |

### Step 3: Template variables

The prompt body uses [Liquid](https://shopify.github.io/liquid/) template syntax. These variables are available:

**`issue` object:**

| Variable | Type | Description |
|----------|------|-------------|
| `issue.id` | string | Issue ID (`#123` format) |
| `issue.identifier` | string | `#123` format |
| `issue.title` | string | Issue title |
| `issue.description` | string or nil | Issue body |
| `issue.priority` | integer or nil | From `priority:N` labels |
| `issue.state` | string | Current state (from labels) |
| `issue.url` | string | GitHub issue URL |
| `issue.labels` | array of strings | All labels, lowercase |
| `issue.blocked_by` | array of objects | Each has `.id`, `.identifier`, `.state` |
| `issue.branch_name` | string or nil | Associated branch |
| `issue.created_at` | string | ISO-8601 timestamp |
| `issue.updated_at` | string | ISO-8601 timestamp |

**`attempt`:** `nil` on first run, integer on retry/continuation.

**Example prompt using conditionals:**

```liquid
{% if attempt %}
This is retry attempt {{ attempt }}. Check what was already done and continue.
{% endif %}

{% if issue.labels contains "bug" %}
This is a bug fix. Write a regression test first.
{% endif %}

{% for blocker in issue.blocked_by %}
Blocked by {{ blocker.identifier }} ({{ blocker.state }}).
{% endfor %}
```

### Step 4: Blocker detection

Symphony detects blockers from issue body text. Use these patterns:

```
blocked by #45
depends on #102
Blocked by #12
```

Issues in `Todo` state with non-terminal blockers are held until the blockers resolve.

### Step 5: Configure your CI/hooks

A typical `after_create` hook clones your repo. A `before_run` hook ensures the workspace is up to date:

```yaml
hooks:
  after_create: |
    git clone https://github.com/owner/repo.git .
    npm install  # or pip install, cargo build, etc.
  before_run: |
    git fetch origin
    git checkout main
    git pull
    npm install
```

## Running in Production

### Docker Compose

Create a `.env` file:

```bash
GITHUB_TOKEN=ghp_your_token_here
SYMPHONY_PORT=8080
RUST_LOG=info
```

```bash
docker compose up -d
```

The dashboard is available at `http://localhost:8080`.

### Kubernetes

Apply the manifests in `k8s/`:

```bash
# Update k8s/secret.yaml with your GitHub token
# Update k8s/configmap.yaml with your WORKFLOW.md

kubectl create namespace symphony
kubectl apply -k k8s/
```

See `k8s/` directory for the full set of manifests (Deployment, Service, ConfigMap, Secret, PVC, ServiceAccount).

## HTTP API

When started with `--port` or `server.port` in WORKFLOW.md:

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/` | GET | HTML dashboard with live updates, activity feed, approval queue |
| `/api/v1/state` | GET | Full system state JSON (running, retrying, tokens, approvals) |
| `/api/v1/{identifier}` | GET | Single issue runtime details |
| `/api/v1/refresh` | POST | Trigger immediate poll cycle |
| `/api/v1/approve/{id}` | POST | Approve or deny a pending agent request |

**Example:**

```bash
# System state
curl http://localhost:8080/api/v1/state | jq .

# Specific issue
curl http://localhost:8080/api/v1/%23123 | jq .

# Force immediate poll
curl -X POST http://localhost:8080/api/v1/refresh

# Approve a pending request
curl -X POST http://localhost:8080/api/v1/approve/abc123 \
  -H 'Content-Type: application/json' \
  -d '{"decision": "approve"}'
```

## Architecture

```
WORKFLOW.md
    |
    v
+-------------------+     +------------------+     +-----------------+
|  Workflow Loader   |---->|   Config Layer   |---->|   Validation    |
|  (YAML + prompt)   |     | (typed, defaults)|     | (preflight)     |
+-------------------+     +------------------+     +-----------------+
                                    |
                                    v
+-------------------+     +------------------+     +-----------------+
|  GitHub Tracker   |<----|  Orchestrator    |---->| Workspace Mgr   |
|  (issue polling)   |     | (state machine)  |     | (per-issue dirs) |
+-------------------+     +------------------+     +-----------------+
                                    |
                                    v
                          +------------------+     +-----------------+
                          |  Agent Runner    |---->| Codex App-Server|
                          | (protocol client)|     | (subprocess)    |
                          +------------------+     +-----------------+
                                    |
                                    v
                          +------------------+
                          |  HTTP Server     |
                          | (dashboard + API)|
                          +------------------+
```

**Crate structure:**

| Crate | Purpose |
|-------|---------|
| `symphony-core` | Domain types, errors, identifiers |
| `symphony-workflow` | WORKFLOW.md parsing, config, Liquid templates, file watching |
| `symphony-tracker` | `IssueTracker` trait + GitHub Issues adapter |
| `symphony-workspace` | Workspace lifecycle, hooks, path safety |
| `symphony-agent` | Codex app-server JSON-RPC protocol client |
| `symphony-orchestrator` | Poll loop, dispatch, reconciliation, retry queue |
| `symphony-server` | Axum HTTP server with dashboard + JSON REST API |
| `symphony-logging` | Structured tracing setup |
| `symphony-cli` | CLI entry point |

## Development

### Prerequisites

- Rust 1.94+ (`rustup update stable`)
- Bash (for workspace hooks)
- Git

### Build and test

```bash
cargo build
cargo test
```

### Install

```bash
cargo install --path crates/symphony-cli
```

### Run locally

```bash
export GITHUB_TOKEN=ghp_...

# From source
cargo run -- ./WORKFLOW.md --port 8080 --pretty-logs

# Or after install
symphony ./WORKFLOW.md --port 8080 --pretty-logs
```

### CLI usage

```
symphony [OPTIONS] [WORKFLOW_PATH]

Arguments:
  [WORKFLOW_PATH]  Path to WORKFLOW.md file [default: ./WORKFLOW.md]

Options:
      --port <PORT>  Enable HTTP server on specified port
      --pretty-logs  Use human-readable (non-JSON) log output
  -h, --help         Print help
```

## How It Works

1. **Poll**: Every `polling.interval_ms`, Symphony fetches open GitHub issues matching `active_states` labels.
2. **Dispatch**: Eligible issues are sorted by priority and age, then dispatched up to `max_concurrent_agents`.
3. **Workspace**: Each issue gets an isolated directory under `workspace.root` with its own git clone, enabling parallel agents on different issues.
4. **Branching**: The `before_run` hook creates a feature branch (`symphony/issue-N`) from `main` for each issue, so agents never conflict on the same branch.
5. **Agent**: A Codex app-server subprocess is launched in the workspace. Symphony sends the rendered prompt (including the full issue description) and streams turn events in real-time.
6. **Turns**: The agent can run up to `max_turns` consecutive turns per session. Between turns, Symphony checks if the issue is still active.
7. **Dashboard**: The web UI shows running sessions with live activity feed, token usage, pending approvals with approve/deny buttons, and retry queue status.
8. **Retry**: On failure, exponential backoff retries are scheduled. On normal exit, a 1-second continuation retry re-checks issue state.
9. **Reconciliation**: Every tick, running issues are checked against GitHub. Terminal issues trigger workspace cleanup. Non-active issues stop the agent.
10. **Reload**: Changes to WORKFLOW.md are detected and applied without restart. Config, prompt, hooks, and concurrency limits update live.

## Supported Agents

Symphony supports two integration modes:

| Agent | Type | Command | Install | Notes |
|-------|------|---------|---------|-------|
| [OpenAI Codex](https://github.com/openai/codex) | `codex` (default) | `codex app-server` | `npm i -g @openai/codex` | JSON-RPC protocol over stdio. Multi-turn sessions managed by Symphony. |
| [Claude Code](https://docs.anthropic.com/en/docs/claude-code) | `claude-cli` | `claude` | [Install guide](https://docs.anthropic.com/en/docs/claude-code/getting-started) | Native CLI integration. Uses `claude -p` with `--output-format stream-json`. Single invocation, Claude manages its own turns. |

### Multi-agent setup

Define multiple agents in `WORKFLOW.md` and assign them per-issue via GitHub labels:

```yaml
agents:
  codex:
    command: codex app-server
    model: gpt-5.3-codex
    reasoning_effort: xhigh           # Codex: -c model_reasoning_effort=xhigh
  claude:
    agent_type: claude-cli
    command: claude
    model: claude-sonnet-4-6
    reasoning_effort: high             # Claude: --effort high (low/medium/high/max)
    max_turns: 20

agent:
  default: codex    # Issues without a label use Codex
```

`reasoning_effort` maps to the right flag for each agent:

| Agent | Flag passed |
|-------|-------------|
| Codex | `-c model_reasoning_effort=<value>` |
| Claude Code | `--effort <value>` (low, medium, high, max) |

To use Claude for a specific issue, add the label `agent:claude` to the GitHub issue. Both agents can run in parallel on different issues simultaneously.

### Implement + Review pipeline

Use different agents for different workflow phases. Codex implements, Claude reviews:

```yaml
agents:
  codex:
    command: codex app-server
    model: gpt-5.3-codex
  claude:
    agent_type: claude-cli
    command: claude
    model: claude-sonnet-4-6

agent:
  default: codex
  by_state:
    code-review: claude       # Claude reviews after Codex implements
    rework: codex             # Codex fixes after review feedback
```

Flow:
```
Todo → In Progress (Codex) → Code Review (Claude) → Human Review → Done
                                    ↑                       |
                                    └── Rework (Codex) ←────┘
```

The implementation agent moves the issue to `code-review` when done. Symphony automatically switches to the Claude agent for review. Claude reviews the PR and either approves (→ `human-review`) or requests changes (→ `rework`), where Codex picks it up again.

### Custom pipeline stages (advanced)

For full control, use the `pipeline:` section. Each stage defines an agent, role, optional prompt override, and transitions. This replaces `agent.by_state`:

```yaml
pipeline:
  stages:
    - state: architect                    # Custom state - any name
      agent: claude
      role: architect
      prompt: |                           # Custom prompt REPLACES the WORKFLOW.md body
        You are a software architect. Analyze {{ issue.identifier }}.
        Create an implementation plan. Do NOT write code.
        {{ issue.description }}
      transition_to: in-progress

    - state: in-progress
      agent: codex
      role: implementer                   # Available as {{ stage.role }} in the prompt
      transition_to: code-review

    - state: code-review
      agent: claude
      role: reviewer
      prompt: |                           # Different prompt for review phase
        Review PR for {{ issue.identifier }}: `gh pr diff`
        If good: add label `human-review`. If not: add label `rework`.
      transition_to: human-review
      reject_to: rework

    - state: rework
      agent: codex
      role: implementer
      transition_to: code-review

    - state: human-review
      agent: none                         # No agent - handoff to human
```

**Prompt behavior:**

| Stage config | What happens |
|---|---|
| No `prompt` field | Uses the WORKFLOW.md body with `{{ stage.role }}`, `{{ stage.transition_to }}`, `{{ stage.reject_to }}` injected |
| Has `prompt` field | Stage prompt **replaces** the WORKFLOW.md body. Use `{{ default_prompt }}` to include the original body |

**Template variables available in all prompts:**

| Variable | Description |
|---|---|
| `{{ stage.role }}` | Role label (e.g., "implementer", "reviewer", "architect") |
| `{{ stage.transition_to }}` | Next state on success |
| `{{ stage.reject_to }}` | Next state on rejection |
| `{{ default_prompt }}` | The rendered WORKFLOW.md body (only in stage prompt overrides) |

**Key differences between agent types:**

| | Codex (`codex`) | Claude Code (`claude-cli`) |
|---|---|---|
| Protocol | JSON-RPC over stdio | `claude -p` with stream-json output |
| Handshake | initialize -> thread/start -> turn/start | None (single CLI invocation) |
| Turn management | Symphony manages multi-turn loop | Claude CLI manages internally via `--max-turns` |
| Approval policy | Sent in JSON-RPC handshake params | `never` → `--dangerously-skip-permissions` |
| Prompt delivery | JSON-RPC `turn/start` message | `$SYMPHONY_PROMPT` env var |
| Model flag | `-c model=<value>` | `--model <value>` |
| Reasoning effort | `-c model_reasoning_effort=<value>` | `--effort <value>` (low/medium/high/max) |

## Authentication

Symphony supports two authentication methods. Both are used by Symphony (for polling) and by the coding agent (for pushing code, creating PRs, updating labels).

### Option 1: Personal Access Token (simple)

Use a fine-grained PAT with these permissions on the target repo:

| Permission | Access | Why |
|------------|--------|-----|
| **Metadata** | Read | Always required |
| **Issues** | Read & Write | Poll issues, update labels, post comments |
| **Contents** | Read & Write | Clone repo, push branches |
| **Pull requests** | Read & Write | Create and update PRs |

```bash
export GITHUB_TOKEN=github_pat_...
```

```yaml
# WORKFLOW.md
tracker:
  api_key: $GITHUB_TOKEN
```

All actions appear under your personal GitHub account.

### Option 2: GitHub App (recommended)

Actions appear as `your-app-name[bot]` with a bot badge. No spare email needed.

**Setup:**

1. Go to **Settings > Developer settings > GitHub Apps > New GitHub App**
2. Name it (e.g., `my-symphony-bot`)
3. Set permissions: Issues (R/W), Contents (R/W), Pull Requests (R/W), Metadata (R)
4. Generate a private key (downloads a `.pem` file)
5. Install the app on your repository
6. Note the **installation ID** from the URL: `github.com/settings/installations/{id}`

```bash
export GITHUB_APP_ID=123456
export GITHUB_APP_INSTALLATION_ID=789012
export GITHUB_APP_PRIVATE_KEY_PATH=/path/to/app.pem
```

```yaml
# WORKFLOW.md (safe to commit - no secrets)
tracker:
  app_id: $GITHUB_APP_ID
  installation_id: $GITHUB_APP_INSTALLATION_ID
  private_key_path: $GITHUB_APP_PRIVATE_KEY_PATH
```

Symphony automatically:
- Generates JWT from the private key
- Exchanges it for a 1-hour installation token
- Refreshes the token every 30 minutes
- Sets `GH_TOKEN` and `GITHUB_TOKEN` for the agent subprocess

## Security

- **Public repo protection:** Set `agent.require_label: symphony` so only issues with that label are dispatched. Public users can create issues but cannot add labels (only collaborators can).
- Workspace paths are sanitized and validated to stay within the configured root
- API tokens are resolved from environment variables, never stored in config files
- Secrets are not logged
- Hooks run inside workspace directories only
- The HTTP server binds to `0.0.0.0` (use firewall rules to restrict access)
- GitHub App tokens auto-refresh and are short-lived (1 hour)

## Acknowledgments

This project is an independent Rust implementation built from the
[Symphony Service Specification](https://github.com/openai/symphony/blob/main/SPEC.md)
created by [OpenAI](https://github.com/openai/symphony). No source code was
copied from the original Elixir reference implementation. The dashboard UI
design was inspired by their Phoenix LiveView dashboard.

The original OpenAI Symphony project is licensed under the
[Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0).

## License

MIT
