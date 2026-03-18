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

hooks:
  after_create: |
    git clone --depth 1 https://github.com/your-org/your-repo.git .
  before_run: |
    git fetch origin
    git checkout main && git pull
    BRANCH="symphony/issue-${SYMPHONY_ISSUE_NUMBER}"
    git checkout -B "$BRANCH" origin/main
  after_run: |
    echo "Agent session completed for ${SYMPHONY_ISSUE_IDENTIFIER}"
  timeout_ms: 300000

agent:
  max_concurrent_agents: 5
  max_turns: 20
  max_retry_backoff_ms: 300000

codex:
  command: codex app-server
  approval_policy: never
  thread_sandbox: workspace-write
  turn_timeout_ms: 3600000
  read_timeout_ms: 5000
  stall_timeout_ms: 600000

server:
  port: 8080
---

You are a coding agent working on issue {{ issue.identifier }}: {{ issue.title }}.

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
- Do not end the turn while the issue remains active unless you are blocked.
{% endif %}

## Status Map

| Label | Meaning |
|-------|---------|
| `todo` | Queued. Move to `in-progress` before starting. |
| `in-progress` | Implementation underway. |
| `human-review` | PR attached. Waiting on human approval. |
| `rework` | Reviewer requested changes. Address feedback. |
| `done` | Terminal. No further action. |

## Step 0: Route by Current State

- **Todo** -> Add label `in-progress`, remove `todo`, then start execution.
- **In Progress** -> Continue execution from current workspace state.
- **Human Review** -> Do not code. Poll for review updates.
- **Rework** -> Read all PR review comments, address feedback, push fixes, then move back to `human-review`.
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

## Execution Flow

1. Find or create the Symphony Workpad comment (see above).
2. Write your plan as a checklist in the workpad.
3. Implement the changes. Update the SAME comment as tasks complete.
4. Run tests and validation.
5. Push branch and create PR.
6. Run the PR feedback sweep before marking as `human-review`.

## PR Feedback Sweep

Before moving to `human-review`, check all PR feedback:

1. Read PR comments: `gh pr view --comments`
2. Read inline review comments: `gh api repos/{owner}/{repo}/pulls/{pr_number}/comments`
3. For each actionable comment: fix the code OR post a justified reply.
4. Re-run tests after changes. Push updates.
5. Repeat until no outstanding comments remain.

## Rework Flow

When state is `rework`:

1. Read ALL review comments on the existing PR.
2. Address each comment: fix code or reply with justification.
3. Run full test suite.
4. Push fixes to the same branch.
5. Complete the PR feedback sweep.
6. Change label from `rework` to `human-review`.

{% if issue.labels.size > 0 %}
## Labels

{% for label in issue.labels %}- {{ label }}
{% endfor %}
{% endif %}

{% for blocker in issue.blocked_by %}
**Blocked by {{ blocker.identifier }} ({{ blocker.state }}).** Focus on independent parts if possible.
{% endfor %}

## Quality Checklist

Before moving to `human-review`:
- [ ] All tests pass
- [ ] No linting warnings
- [ ] No hardcoded secrets
- [ ] Conventional commit messages
- [ ] PR created with `Closes {{ issue.identifier }}`
- [ ] PR feedback sweep completed
- [ ] Progress comment updated with final status
