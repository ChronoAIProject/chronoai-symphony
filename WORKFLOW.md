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
#     # Triage: Claude assesses the issue, plans if complex, adds routing labels.
#     - state: todo
#       agent: claude
#       role: triage
#       prompt: |
#         You are a senior technical lead triaging {{ issue.identifier }}.
#         {{ issue.description }}
#         1. Assess what needs to change and which parts are affected.
#         2. Add labels: `backend`, `frontend`, or both (for parallel agents).
#         3. If complex: create a workpad comment with an implementation plan.
#         4. Move to in-progress: `gh issue edit {{ issue.identifier }} --remove-label todo --add-label in-progress`
#       transition_to: in-progress
#
#     # Parallel: backend + frontend agents when triage adds both labels
#     - state: in-progress
#       agent: codex
#       role: backend-implementer
#       when_labels: [backend]
#       scope: backend/
#       transition_to: code-review
#     - state: in-progress
#       agent: claude
#       role: frontend-implementer
#       when_labels: [frontend]
#       scope: frontend/
#       transition_to: code-review
#
#     # Fullstack fallback: triage didn't add backend/frontend labels
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
# Flow: Todo (triage) → In Progress (implement) → Code Review → Human Review → Done
# The triage agent decides if architecture planning is needed and which
# implementation agents to dispatch. No manual label management required.

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
- **Code Review** -> You are the **review agent**. Review the PR:
  1. Read all changed files: `gh pr diff`
  2. Check code quality, tests, security, and architecture
  3. If approved: `gh issue edit {{ issue.identifier }} --remove-label code-review --add-label human-review`
  4. If needs work: post review comments, then `gh issue edit {{ issue.identifier }} --remove-label code-review --add-label rework`
  *(Review stages manage labels manually because they have two outcomes: approve OR reject)*
- **Human Review** -> Do not code. Poll for review updates.
- **Rework** -> Read PR review comments, address feedback, push fixes, then `gh issue edit {{ issue.identifier }} --remove-label rework --add-label code-review`.
- **Done / Closed** -> Do nothing, shut down.

## Git Workflow

1. You are on branch `symphony/issue-{{ issue.identifier | remove: "#" }}` (created from `main`).
2. Commit with conventional messages (`feat:`, `fix:`, `refactor:`).
3. Push your commits to the branch.
4. Check if a PR already exists for this branch: `gh pr list --head symphony/issue-{{ issue.identifier | remove: "#" }} --json number --jq '.[0].number'`
5. If no PR exists, create one: `gh pr create --title "{{ issue.identifier }}: {{ issue.title }}" --body "Closes {{ issue.identifier }}" --head symphony/issue-{{ issue.identifier | remove: "#" }}`
6. If a PR already exists (e.g., another parallel agent created it), just push to the same branch - the PR updates automatically.

**IMPORTANT:** All agents working on the same issue share the same branch and PR. Do NOT create separate branches or PRs.

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
5. Commit and push to the branch. Create a PR if one doesn't exist (see Git Workflow).
6. **STOP implementing.** {% if stage.transition_to %}Symphony will automatically move the issue to `{{ stage.transition_to }}` when all parallel agents finish.{% else %}Update the **issue** label: `gh issue edit {{ issue.identifier }} --remove-label in-progress --add-label code-review`{% endif %}

## Rework Flow

When state is `rework`:

1. Read ALL review comments on the existing PR (both human and automated).
2. Address **only** the comments raised. Do not fix unrelated things.
3. Run tests relevant to your fixes.
4. Push fixes to the same branch.
5. **STOP.** {% if stage.transition_to %}Symphony will automatically move the issue to `{{ stage.transition_to }}`.{% else %}Update the **issue** label: `gh issue edit {{ issue.identifier }} --remove-label rework --add-label code-review`{% endif %}

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
