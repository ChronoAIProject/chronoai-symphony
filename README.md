# chronoai-symphony

A Rust implementation of the [Symphony Service Specification](https://github.com/openai/symphony/blob/main/SPEC.md) that orchestrates coding agents against **GitHub Issues**.

Symphony is a long-running automation service that:

- Polls GitHub Issues on a fixed cadence
- Creates isolated per-issue workspaces
- Runs coding agent sessions (Codex app-server compatible) for each issue
- Manages retries with exponential backoff
- Provides an HTTP dashboard and JSON API for observability

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

```markdown
---
tracker:
  kind: github
  api_key: $GITHUB_TOKEN
  project_slug: your-org/your-repo
  active_states:
    - Todo
    - In Progress
    - Human Review
    - Rework
  terminal_states:
    - Done
    - Closed
    - Cancelled

polling:
  interval_ms: 30000

workspace:
  root: /tmp/symphony_workspaces

hooks:
  after_create: |
    git clone --depth 1 https://github.com/your-org/your-repo.git .
  before_run: |
    git fetch origin
    git checkout main && git pull
    BRANCH="symphony/issue-${SYMPHONY_ISSUE_NUMBER}"
    git checkout -B "$BRANCH" origin/main
  timeout_ms: 300000

agent:
  max_concurrent_agents: 5
  max_turns: 20

codex:
  command: codex app-server
  approval_policy: never
  thread_sandbox: workspace-write
  turn_timeout_ms: 3600000
  stall_timeout_ms: 300000

server:
  port: 8080
---

You are a coding agent working on issue {{ issue.identifier }}: {{ issue.title }}.

{{ issue.description }}

{% if attempt %}
Continuation attempt {{ attempt }}. Resume from current workspace state.
{% endif %}

## Status Map

- **Todo** -> Move to `in-progress`, start work.
- **In Progress** -> Continue implementation.
- **Human Review** -> Do not code. Wait for review.
- **Rework** -> Read PR review comments, address feedback, push fixes, return to `human-review`.
- **Done** -> Shut down.

## Git Workflow

1. You are on branch `symphony/issue-{{ issue.identifier | remove: "#" }}`.
2. Commit with conventional messages.
3. Push and create a PR with `Closes {{ issue.identifier }}`.

## PR Feedback Sweep (before moving to human-review)

1. Read all PR comments: `gh pr view --comments`
2. Read inline reviews: `gh api repos/{owner}/{repo}/pulls/{number}/comments`
3. Address each comment: fix code or reply with justification.
4. Re-run tests, push, repeat until no outstanding comments.

## Instructions

1. Read the issue carefully.
2. Implement changes and write tests.
3. Push and create a pull request.
4. Complete PR feedback sweep.
5. Add label `human-review` to the issue.
```

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
| `human-review` | Agent finished, PR attached, needs human review | Active (handoff) |
| `rework` | Reviewer requested changes on the PR | Active (agent addresses feedback) |
| `done` | Work is complete | Terminal (workspace cleaned) |
| `cancelled` | Issue abandoned | Terminal (workspace cleaned) |

An open issue with **no workflow label** defaults to state `Todo`.
A **closed** issue defaults to state `Done`.

**Review lifecycle:** When the agent finishes, it adds `human-review`. A human reviews the PR. If changes are needed, the human swaps the label to `rework`. Symphony re-dispatches the agent, which reads PR review comments, addresses feedback, pushes fixes, and moves back to `human-review`. This cycle repeats until the human approves and adds `done`.

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
  api_key: $GITHUB_TOKEN               # Required. Supports $VAR env references.
  project_slug: owner/repo             # Required. GitHub owner/repo.
  endpoint: https://api.github.com     # Optional. Default shown.
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

hooks:
  after_create: |                       # Runs once when workspace is first created.
    git clone --depth 1 https://github.com/owner/repo.git .
  before_run: |                         # Runs before each agent attempt.
    git fetch origin
    git checkout main && git pull
    BRANCH="symphony/issue-${SYMPHONY_ISSUE_NUMBER}"
    git checkout -B "$BRANCH" origin/main
  after_run: |                          # Runs after each attempt (failures ignored).
    echo "done"
  before_remove: |                      # Runs before workspace deletion (failures ignored).
    echo "cleaning up"
  timeout_ms: 300000                    # Hook timeout. Default: 60s.

agent:
  max_concurrent_agents: 10            # Global concurrency limit. Default: 10.
  max_turns: 20                         # Max turns per agent session. Default: 20.
  max_retry_backoff_ms: 300000         # Max retry delay. Default: 5 minutes.
  max_concurrent_agents_by_state:      # Optional per-state concurrency limits.
    in progress: 5
    todo: 3

codex:
  command: codex app-server            # Agent launch command. Default shown.
  approval_policy: never               # Auto-approve all actions. Valid: untrusted,
                                        # on-failure, on-request, granular, never.
  thread_sandbox: workspace-write      # Sandbox mode. Default: workspace-write.
  turn_timeout_ms: 3600000             # Turn timeout. Default: 1 hour.
  read_timeout_ms: 5000                # Startup handshake timeout. Default: 5s.
  stall_timeout_ms: 300000             # Inactivity timeout. Default: 5 min. <=0 disables.

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

## Token Permissions

Symphony itself only **reads** issues for polling and reconciliation. However, the coding agent subprocess (Codex) inherits `GITHUB_TOKEN` and needs **write** access to push code, create PRs, update labels, and post comments.

### Fine-grained personal access token (recommended)

Go to **Settings > Developer settings > Personal access tokens > Fine-grained tokens**, select the target repository, and grant:

| Permission | Access | Used by | Why |
|------------|--------|---------|-----|
| **Metadata** | Read | Symphony + Agent | Always required by GitHub |
| **Issues** | Read & Write | Symphony (read), Agent (write) | Poll issues, update labels/state, post comments |
| **Contents** | Read & Write | Agent | Clone repo, push branches, read/write files |
| **Pull requests** | Read & Write | Agent | Create and update pull requests |
| **Workflows** | Read & Write | Agent (optional) | Only if agent needs to modify GitHub Actions |

### Classic personal access token

If using a classic token, grant the `repo` scope (covers all of the above for private repos). For public repos, `public_repo` is sufficient.

### Token setup

```bash
# Fine-grained token (recommended)
export GITHUB_TOKEN=github_pat_...

# Or classic token
export GITHUB_TOKEN=ghp_...
```

### Who uses the token

```
GITHUB_TOKEN
    |
    +---> Symphony (read-only)
    |       - Poll open/closed issues
    |       - Fetch issue states for reconciliation
    |       - Check labels for state mapping
    |
    +---> Coding Agent (read + write)
            - git clone / git push (via hooks or agent tools)
            - Create pull requests
            - Add/remove labels (state transitions)
            - Post comments on issues
            - Update issue state
```

## Security

- Workspace paths are sanitized and validated to stay within the configured root
- API tokens are resolved from environment variables, never stored in config files
- Secrets are not logged
- Hooks run inside workspace directories only
- The HTTP server binds to `127.0.0.1` by default
- This implementation targets trusted environments with auto-approved agent actions

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
