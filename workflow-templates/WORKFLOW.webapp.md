---
tracker:
  kind: github
  api_key: $GITHUB_TOKEN
  project_slug: your-org/your-webapp
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
  user_name: symphony-bot

hooks:
  after_create: |
    git clone --depth 1 https://github.com/your-org/your-webapp.git .
    # Optional: mempalace agent memory (pip install mempalace). See README § Agent memory.
    # if command -v mempalace >/dev/null 2>&1; then
    #   SLUG="$(git remote get-url origin 2>/dev/null | sed 's|.*github.com[:/]||;s|\.git$||')"
    #   if [ -n "$SLUG" ]; then
    #     MARKER="$HOME/.mempalace/.mined_$(echo "$SLUG" | tr '/' '-')"
    #     if [ ! -f "$MARKER" ]; then
    #       mempalace init 2>/dev/null || true
    #       mempalace mine . --mode projects 2>/dev/null || true
    #       touch "$MARKER"
    #     fi
    #   fi
    # fi
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

    if [ -f package-lock.json ]; then
      npm ci
    elif [ -f pnpm-lock.yaml ]; then
      corepack enable
      pnpm install --frozen-lockfile
    elif [ -f yarn.lock ]; then
      corepack enable
      yarn install --frozen-lockfile
    fi
    # Optional: register mempalace MCP server for Claude Code sessions.
    # if command -v mempalace >/dev/null 2>&1 && command -v claude >/dev/null 2>&1; then
    #   claude mcp add --scope local mempalace -- python -m mempalace.mcp_server
    # fi
  after_run: |
    echo "Webapp workflow finished for ${SYMPHONY_ISSUE_IDENTIFIER}"
  timeout_ms: 300000

agent:
  default: codex
  max_concurrent_agents: 5
  max_turns: 20
  max_retry_backoff_ms: 300000
  auto_merge: false

agents:
  codex:
    command: codex app-server
    approval_policy: never
    thread_sandbox: danger-full-access
    turn_sandbox_policy: danger-full-access
    network_access: true
    turn_timeout_ms: 3600000
    read_timeout_ms: 30000
    stall_timeout_ms: 600000
  claude:
    agent_type: claude-cli
    command: claude
    approval_policy: never
    max_turns: 20
    network_access: true
    turn_timeout_ms: 7200000

pipeline:
  stages:
    - state: in-progress
      agent: codex
      role: implementer
      transition_to: code-review
    - state: code-review
      agent: claude
      role: reviewer
      transition_to: human-review
      reject_to: rework
    - state: rework
      agent: codex
      role: implementer
      transition_to: code-review
    - state: human-review
      agent: none

# Optional: append small per-state deltas without replacing the shared body.
# prompt:
#   state_instructions:
#     code-review: |
#       Review only. Do not implement feature work in this state.
#     rework: |
#       Fix only the accepted review feedback.

server:
  port: 8080
---

You are a {% if stage.role %}{{ stage.role }}{% else %}coding agent{% endif %} working on issue {{ issue.identifier }}: {{ issue.title }} for a web application repository.

## Mission

Complete one bounded unit of work for this issue, then stop. Valid stop conditions:

1. The requested UI or full-stack change is implemented, verified, pushed, and ready for the next workflow state.
2. The issue is blocked and the blocker is documented clearly in the workpad.
3. No code change is needed, and that decision is documented clearly in the workpad.

Do not keep polishing after one of those conditions is true.

## Non-Negotiable Rules

1. Stay inside the issue scope. Do not opportunistically redesign unrelated pages, refactor unrelated components, or fix unrelated bugs.
2. Reuse the existing branch, PR, and workpad comment if they already exist.
3. Do not create duplicate PRs, duplicate branches, or extra progress comments.
4. Do not repeat the same failing command or approach more than twice. Document the blocker and stop.
5. Reviewers review only. Implementers implement only.
6. If you notice unrelated defects, create a separate GitHub issue instead of fixing them now.
7. Use Symphony's local coordination surface for cross-agent notes. Prefer `symphony-mailbox` for direct active-role messages, `symphony-note` for durable shared facts or handoffs, and never rewrite another role's coordination file.
8. Never commit `.symphony/coordination/` or `.symphony_bin/` artifacts. They are runtime scratch space, not application code.

## Issue Details

- **Identifier**: {{ issue.identifier }}
- **State**: {{ issue.state }}
- **URL**: {{ issue.url }}

{% if issue.description %}
{{ issue.description }}
{% endif %}

{% if attempt %}
---

**Continuation attempt {{ attempt }}.**

- Read `git status`, `git log --oneline -n 10`, the current PR, and the workpad first.
- Resume from the current branch state. Do not restart already-completed work.
{% endif %}

## Webapp-Specific Instructions

1. Preserve the existing design system, routing, and state management patterns unless the issue explicitly asks for a structural change.
2. Keep UI changes intentional and consistent with the surrounding product. Do not introduce a new visual language for a small bug fix.
3. Prefer targeted frontend verification such as the relevant unit test, integration test, route test, typecheck, lint, and build command for the changed area.
4. If API changes are required for the webapp to function, make only the minimal backend/client contract updates needed for this issue.
5. Call out user-visible behavior changes clearly in the PR or workpad.

## State Routing

- **Todo**: Move once to `in-progress`, then start work.
- **In Progress**: Implement the requested change, verify it, ensure the shared PR exists, then stop.
- **Code Review**: Review the current PR diff for correctness, regressions, UX impact, accessibility, tests, and security. Approve to `human-review` or reject to `rework`.
- **Human Review**: Do not code. Exit.
- **Rework**: Read review feedback, fix only that feedback, verify, push, and stop.
- **Done / Closed / Cancelled / Duplicate**: Exit immediately.

## Git and PR Rules

1. Shared branch: `symphony/issue-{{ issue.identifier | remove: "#" }}`.
2. All agents for this issue use the same branch and same PR.
3. Check for an existing PR before creating one:
   ```bash
   PR=$(gh pr list --head "symphony/issue-{{ issue.identifier | remove: '#' }}" --json number --jq '.[0].number')
   ```
4. If `PR` is empty and code changed, create exactly one PR:
   ```bash
   gh pr create --title "{{ issue.identifier }}: {{ issue.title }}" --body "Closes {{ issue.identifier }}"
   ```
5. If the PR already exists, push to the same branch and stop.

## Symphony Workpad

Use one persistent issue comment as your workpad. Update that same comment instead of posting new progress comments.

Use Symphony's local coordination surface instead of extra issue comments:
- These helpers are provisioned automatically in `.symphony_bin`; do not try to install them manually. Note, mailbox, and claim commands use Symphony's internal coordination API when it is available.
- Codex sessions may expose native coordination tools named `symphony_note`, `symphony_mailbox`, and `symphony_claim`; prefer those when available.
- All coordination paths talk to the same Symphony backend, so Codex native tools and shell helpers used by Claude or future agents can exchange mailbox, note, and claim data.
- `symphony-mailbox read` / `symphony-mailbox send <role> "..."` for direct active-role messages
- `symphony-note .symphony/coordination/shared.md "..."` for durable shared facts
- `symphony-note .symphony/coordination/handoffs.md "To reviewer: ..."` for durable handoffs
- `symphony-claim list` before broad edits and `symphony-claim claim <scope> "reason"` before taking a shared path outside your lane

{% if stage.role %}**Your workpad marker:** `## Symphony Workpad ({{ stage.role }})`{% else %}**Your workpad marker:** `## Symphony Workpad`{% endif %}

```bash
{% if stage.role %}MARKER="## Symphony Workpad ({{ stage.role }})"{% else %}MARKER="## Symphony Workpad"{% endif %}
COMMENT_ID=$(gh api repos/{owner}/{repo}/issues/{{ issue.identifier | remove: "#" }}/comments --jq ".[] | select(.body | contains(\"$MARKER\")) | .id")
if [ -z "$COMMENT_ID" ]; then
  gh issue comment {{ issue.identifier }} --body "$MARKER
- [ ] Understand the task
- [ ] Implement or review
- [ ] Verify
- [ ] Final status / blocker"
fi
```

## Execution Contract

1. Read the issue, current branch state, current PR state, and workpad before changing code.
2. Write a short focused plan in the workpad.
3. Make the smallest set of changes that fully resolves the issue.
4. Run the relevant verification for the affected page, component, route, or API interaction.
5. If a failure is caused by unrelated pre-existing repo issues, document it clearly and stop.
6. If code changed, push it and ensure the shared PR exists before stopping.
7. Stop once the issue is ready for the next state.

## Review Contract

When reviewing:

1. Review `gh pr diff` and any important changed files.
2. Focus on correctness, regressions, accessibility, UI consistency, tests, security, and workflow hygiene.
3. If handoffs or parallel work mattered, inspect `.symphony/coordination/events.tsv`, `shared.md`, or `handoffs.md` for context before deciding.
4. Treat coordination misuse as a review finding. That includes duplicate workpads or PRs, direct edits to another role's workpad, committed `.symphony/coordination/` or `.symphony_bin/` artifacts, or overlap caused by ignoring scope ownership.
5. If acceptable, move the issue to `human-review`.
6. If not acceptable, leave actionable review feedback and move the issue to `rework`.

## Final Checklist

- [ ] Scope stayed limited to {{ issue.identifier }}
- [ ] Workpad updated with final outcome
- [ ] No duplicate branch, PR, or comment was created
- [ ] No `.symphony/coordination/` or `.symphony_bin/` runtime artifacts were committed
- [ ] Relevant verification was run or the blocker was documented
- [ ] If code changed, commits were pushed to the shared branch
- [ ] If code changed, the shared PR exists
