---
tracker:
  kind: github
  api_key: $GITHUB_TOKEN               # Option 1: Personal access token
  # app_id: $GITHUB_APP_ID             # Option 2: GitHub App (shows as bot)
  # installation_id: $GITHUB_APP_INSTALLATION_ID
  # private_key_path: $GITHUB_APP_PRIVATE_KEY_PATH
  project_slug: your-org/your-repo
  active_states:
    - Todo
    - In Progress
    - Code Review
    - Human Review
    - Rework
  terminal_states:
    - Done
    - Closed
    - Cancelled
    - Canceled
    - Duplicate

polling:
  interval_ms: 30000

workspace:
  root: /tmp/symphony_workspaces

git:
  user_name: symphony-bot                # Git author for agent commits.
  # email: symphony@your-org.com         # Optional. Defaults to git's default.

hooks:
  after_create: |
    git clone --depth 1 https://github.com/your-org/your-repo.git .
  before_run: |
    git fetch origin
    BRANCH="symphony/issue-${SYMPHONY_ISSUE_NUMBER}"
    if git show-ref --verify --quiet "refs/remotes/origin/$BRANCH"; then
      git checkout "$BRANCH"
      git pull origin "$BRANCH"
    elif git show-ref --verify --quiet "refs/heads/$BRANCH"; then
      git checkout "$BRANCH"
    else
      git checkout main && git pull
      git checkout -b "$BRANCH" origin/main
    fi
  after_run: |
    echo "Agent session completed for ${SYMPHONY_ISSUE_IDENTIFIER}"
  timeout_ms: 300000

agent:
  default: codex                        # Which agent to use by default.
  max_concurrent_agents: 5
  max_turns: 20
  max_retry_backoff_ms: 300000
  auto_merge: false                     # Auto-merge PR after approval (default: false).
  # require_label: symphony             # Only dispatch issues with this label.
  # by_state:                           # Override agent per state (implement + review pipeline).
  #   code-review: claude               # Claude reviews after Codex implements.
  #   rework: codex                     # Codex fixes after review feedback.

# Multiple named agents. Add `agent:claude` label to an issue to use Claude.
agents:
  codex:
    command: codex app-server
    approval_policy: never
    thread_sandbox: workspace-write
    # model: gpt-5.3-codex
    # reasoning_effort: xhigh
    network_access: true
    turn_timeout_ms: 3600000
    read_timeout_ms: 5000
    stall_timeout_ms: 600000
  # claude:                              # Uncomment to enable. Add `agent:claude` label to issues.
  #   agent_type: claude-cli             # Uses official Claude Code CLI directly.
  #   command: claude                    # Official CLI, no third-party wrapper needed.
  #   model: claude-sonnet-4-6
  #   reasoning_effort: high             # --effort flag. low, medium, high, max.
  #   approval_policy: never             # "never" → --dangerously-skip-permissions.
  #   max_turns: 20
  #   network_access: true
  #   turn_timeout_ms: 7200000           # 2 hours for full session.

# Custom pipeline stages (optional). Define per-state agent, role, prompt,
# and transitions. When set, these take priority over agent.by_state.
#
# Prompt behavior:
# - No `prompt` on stage → uses the WORKFLOW.md body below with stage vars added
# - `prompt` on stage → REPLACES the body. Use {{ default_prompt }} to include it.
# - Available vars: {{ stage.role }}, {{ stage.transition_to }}, {{ stage.reject_to }}
#
# pipeline:
#   stages:
#     # Architect stage: only for issues labeled "architect".
#     # A maintainer adds "architect" when the issue is complex.
#     # Issues without "architect" skip straight to implementation.
#     - state: in-progress
#       agent: claude
#       role: architect
#       when_labels: [architect]            # Only if maintainer adds this label
#       prompt: |
#         You are a software architect. Analyze {{ issue.identifier }}: {{ issue.title }}.
#         {{ issue.description }}
#         Create a detailed implementation plan. Do NOT write code.
#         Post the plan as a Symphony Workpad comment, then remove the
#         `architect` label so the implementer picks it up on the next cycle.
#       transition_to: in-progress          # Stays in-progress, but without
#                                           # "architect" label the fallback runs next
#
#     # Parallel: backend + frontend agents when issue has both labels
#     - state: in-progress
#       agent: codex
#       role: backend-implementer
#       when_labels: [backend]              # Only if issue has "backend" label
#       scope: backend/                     # Hint: focus on this directory
#       transition_to: code-review
#     - state: in-progress
#       agent: claude
#       role: frontend-implementer
#       when_labels: [frontend]             # Only if issue has "frontend" label
#       scope: frontend/
#       transition_to: code-review
#
#     # Fullstack fallback: no backend/frontend label = one agent does it all
#     - state: in-progress
#       agent: codex
#       role: implementer
#       transition_to: code-review
#
#     # Code review
#     - state: code-review
#       agent: claude
#       role: reviewer
#       prompt: |
#         Review PR for {{ issue.identifier }}: `gh pr diff`
#         If good: add label `human-review`, remove `code-review`.
#         If needs work: post review comments, add label `rework`, remove `code-review`.
#       transition_to: human-review
#       reject_to: rework
#     - state: rework
#       agent: codex
#       role: implementer
#       transition_to: code-review
#     - state: human-review
#       agent: none
#
# How the architect flow works:
# 1. Maintainer creates issue, adds labels: symphony + architect
# 2. Symphony dispatches Claude as architect (when_labels: [architect] matches)
# 3. Claude creates implementation plan, removes "architect" label
# 4. Next poll: "architect" label gone, fallback implementer stage runs
# 5. No extra state needed - the label presence/absence controls routing

server:
  port: 8080
---

You are a {% if stage.role %}{{ stage.role }}{% else %}coding agent{% endif %} working on issue {{ issue.identifier }}: {{ issue.title }}.

## Issue Details

- **Identifier**: {{ issue.identifier }}
- **State**: {{ issue.state }}
- **URL**: {{ issue.url }}

{% if issue.description %}
{{ issue.description }}
{% endif %}

{% if attempt %}
---

**Continuation attempt {{ attempt }}.** Resume from the current workspace state:
- Check what was already done (`git log`, `git status`).
- Do not redo completed work.
{% endif %}

## CRITICAL: Scope and Completion Rules

1. **Stay focused on the issue description.** Only implement what is explicitly requested. Do not fix unrelated bugs, refactor surrounding code, or add features not in the issue.
2. **Do not expand scope.** If you discover unrelated problems, create a NEW GitHub issue for them instead of fixing them now: `gh issue create --title "..." --body "Found while working on {{ issue.identifier }}"`.
3. **Finish and hand off.** Once the requested changes are implemented and tests pass, immediately push, create the PR, and move to `code-review`. Do not keep iterating to find more things to improve.
4. **Good enough is done.** The code review agent will catch quality issues. Your job is to implement the feature/fix, not to achieve perfection.
5. **If blocked, stop.** If you cannot complete the task (missing permissions, unclear requirements, dependencies), update the workpad with what's blocking you and move to `human-review`.

## Status Map

| Label | Meaning |
|-------|---------|
| `todo` | Queued. Move to `in-progress` before starting. |
| `in-progress` | Implementation underway. |
| `code-review` | PR created. Automated review in progress (by review agent). |
| `human-review` | Automated review passed. Waiting on human approval. |
| `rework` | Reviewer requested changes. Address feedback. |
| `done` | Terminal. No further action. |

## Step 0: Route by Current State

- **Todo** -> Add label `in-progress`, remove `todo`, then start execution.
- **In Progress** -> Implement the changes. When done, add label `code-review` (for automated review) or `human-review` (skip to human).
- **Code Review** -> You are the **review agent**. Review the PR for this issue:
  1. Read all changed files: `gh pr diff`
  2. Check code quality, tests, security, and architecture
  3. If changes look good: add label `human-review`, remove `code-review`
  4. If changes need work: post review comments on the PR, add label `rework`, remove `code-review`
- **Human Review** -> Do not code. Poll for review updates.
- **Rework** -> Read all PR review comments, address feedback, push fixes, then move back to `code-review` or `human-review`.
- **Done / Closed** -> Do nothing, shut down.

## Git Workflow

1. You are on branch `symphony/issue-{{ issue.identifier | remove: "#" }}` (created from `main`).
2. Commit with conventional messages (`feat:`, `fix:`, `refactor:`).
3. Push and create a pull request targeting `main`.
4. Include `Closes {{ issue.identifier }}` in the PR description.

## Symphony Workpad (Single Persistent Comment)

Use exactly ONE persistent comment on the issue as your workpad. NEVER create additional comments for updates.

**Finding or creating the workpad:**
1. Search existing comments for `## Symphony Workpad`: `gh api repos/{owner}/{repo}/issues/{number}/comments --jq '.[] | select(.body | contains("## Symphony Workpad")) | .id'`
2. If found, reuse that comment ID for ALL updates.
3. If not found, create it once: `gh issue comment {number} --body "## Symphony Workpad\n- [ ] Planning\n- [ ] Implementation\n- [ ] Tests\n- [ ] Validation"`
4. Save the comment ID and use it for every update.

**Updating the workpad (NEVER create a new comment):**
```bash
gh api repos/{owner}/{repo}/issues/comments/{comment_id} -X PATCH -f body="## Symphony Workpad
- [x] Task 1 - completed
- [x] Task 2 - completed
- [ ] Tests
- [ ] Validation"
```

## Execution Flow (In Progress)

1. Find or create the Symphony Workpad comment (see above).
2. Write a **focused plan** with only the tasks needed for THIS issue. No extras.
3. Implement the changes. Update the workpad as tasks complete.
4. Run tests relevant to your changes. Fix only test failures caused by your changes.
5. Commit, push, and create PR with `Closes {{ issue.identifier }}`.
6. **STOP implementing.** Add label `code-review`. Do not make more changes after creating the PR.

## Rework Flow

When state is `rework`:

1. Read ALL review comments on the existing PR (both human and automated).
2. Address **only** the comments raised. Do not fix unrelated things.
3. Run tests relevant to your fixes.
4. Push fixes to the same branch.
5. **STOP.** Add label `code-review`. Do not continue making more changes.

{% if issue.labels.size > 0 %}
## Labels

{% for label in issue.labels %}- {{ label }}
{% endfor %}
{% endif %}

{% for blocker in issue.blocked_by %}
**Blocked by {{ blocker.identifier }} ({{ blocker.state }}).** Focus on independent parts if possible.
{% endfor %}

## Quality Checklist

Before moving to `code-review`:
- [ ] All tests pass
- [ ] No linting warnings
- [ ] No hardcoded secrets
- [ ] Conventional commit messages
- [ ] PR created with `Closes {{ issue.identifier }}`
- [ ] Progress comment updated with final status
