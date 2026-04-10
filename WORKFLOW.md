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
    # Optional: mempalace agent memory (pip install mempalace). See README § Agent memory.
    # Mines the project once into a shared palace at ~/.mempalace/. The marker file
    # prevents re-mining when later issues create new workspaces for the same project.
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
    # Optional: mempalace shared context for all agents (Claude, Codex, any future agent).
    # Loads relevant memories into a workspace file every agent can read.
    # if command -v mempalace >/dev/null 2>&1; then
    #   mkdir -p .symphony
    #   mempalace search "issue ${SYMPHONY_ISSUE_NUMBER}" --limit 10 \
    #     > .symphony/mempalace_context.md 2>/dev/null || true
    # fi
    # Register MCP server so Claude Code gets interactive read/write on top.
    # if command -v claude >/dev/null 2>&1 && command -v mempalace >/dev/null 2>&1; then
    #   claude mcp add --scope local mempalace -- python -m mempalace.mcp_server
    # fi
  after_run: |
    echo "Agent session completed for ${SYMPHONY_ISSUE_IDENTIFIER}"
    # Optional: store coordination artifacts back into shared mempalace so the
    # next agent (any type) can find what this session decided or handed off.
    # if command -v mempalace >/dev/null 2>&1 && [ -d .symphony/coordination ]; then
    #   mempalace mine .symphony/coordination --mode general 2>/dev/null || true
    # fi
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
    thread_sandbox: danger-full-access # Trusted isolated runner default.
    turn_sandbox_policy: danger-full-access
    # model: gpt-5.3-codex
    # reasoning_effort: xhigh
    network_access: true
    turn_timeout_ms: 3600000
    read_timeout_ms: 30000
    stall_timeout_ms: 600000
  # claude:                              # Uncomment to enable. Add `agent:claude` label to issues.
  #   agent_type: claude-cli             # Uses official Claude Code CLI directly.
  #   command: claude                    # Official CLI, no third-party wrapper needed.
  #   model: claude-sonnet-4-6
  #   reasoning_effort: high             # --effort flag. low, medium, high, max.
  #   approval_policy: never             # Trusted isolated runner default.
  #   # allowed_tools / disallowed_tools are optional. Leave them unset for full access.
  #   max_turns: 20
  #   network_access: true
  #   turn_timeout_ms: 7200000           # 2 hours for full session.

# Custom pipeline stages (optional). Define per-state agent, role, prompt,
# and transitions. When set, these take priority over agent.by_state.
#
# Validation rules:
# - `role` must be unique per state and must not contain `:`
# - `transition_to` / `reject_to` should point to known workflow states
# - `reject_to` requires `transition_to`
# - `agent: none` stages are pure handoff states and cannot define transitions
# - If multiple runnable stages share a state, each must define a unique non-root `scope`
# - If multiple runnable stages share a state, `transition_to` and `reject_to` must match across that group
#
# Prompt behavior:
# - `prompt.state_instructions.<state>` → APPENDS extra instructions after the shared body for that state.
# - `prompt.role_instructions.<role>` → APPENDS extra instructions after the shared body and state instructions for that role.
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
#
# prompt:
#   state_instructions:
#     code-review: |
#       Review only. Do not implement feature work in this state.
#     rework: |
#       Read open review feedback first and fix only the accepted review items.
#   role_instructions:
#     reviewer: |
#       Act only on review findings and verification. Do not author fixes.

server:
  port: 8080
---

You are a {% if stage.role %}{{ stage.role }}{% else %}coding agent{% endif %} working on issue {{ issue.identifier }}: {{ issue.title }}.

## Mission

Complete exactly one bounded unit of work for this issue, then stop. The valid stop conditions are:

1. The requested work is done, verified, pushed, and ready for the next workflow state.
2. The issue is blocked and the blocker is documented clearly in the workpad.
3. No code change is needed, and the reason is documented clearly in the workpad.

Do not keep iterating after one of those conditions is reached. Do not drift into unrelated cleanup or speculative improvements.

## Non-Negotiable Rules

1. Stay inside the issue scope. Only change what is required for {{ issue.identifier }}.
2. Do not create duplicate work. Reuse the existing branch, existing PR, and existing workpad comment if they already exist.
3. Do not open a second branch, second PR, or extra "status update" issue comments for the same role.
4. Do not repeat the same failing command or strategy more than twice. If you are still blocked, document the blocker and stop.
5. Do not wait idly, poll forever, or keep talking to yourself in a loop. If human input or an external dependency is required, hand off.
6. If you are acting as a reviewer, review only. If you are acting as an implementer, implement only.
7. If you notice unrelated problems, open a new issue instead of fixing them now: `gh issue create --title "..." --body "Found while working on {{ issue.identifier }}"`.
8. Use Symphony's local coordination surface for cross-agent notes. Prefer `symphony-mailbox` for direct active-role messages, `symphony-note` for durable shared facts or handoffs, and never rewrite another role's coordination file.
9. Never commit `.symphony/coordination/` or `.symphony_bin/` artifacts. They are runtime scratch space, not product code.

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

- Read the current repo state first: `git status`, `git log --oneline -n 10`, and the existing PR/workpad.
- Resume from the current state. Do not restart completed work.
- If the previous attempt was already blocked on the same issue, do not retry blindly. Document the blocker and finish with a handoff.
{% endif %}

## Status Map

| Label | Meaning |
|-------|---------|
| `todo` | Queued. Claim it once, then move to `in-progress`. |
| `in-progress` | Active implementation. |
| `code-review` | PR exists and needs automated review. |
| `human-review` | Waiting for a human decision. No further coding. |
| `rework` | Reviewer requested focused fixes only. |
| `done` | Terminal. Exit immediately. |

## State Routing

- **Todo**: Claim the issue once by moving it to `in-progress`, then start work.
- **In Progress**: Implement the smallest complete solution, verify it, push it, ensure the PR exists, then stop.
- **Code Review**: Review the current PR diff. Approve to `human-review` or reject to `rework`. Do not implement feature work in review mode.
- **Human Review**: Do not code, do not poll forever, do not re-dispatch yourself. Exit.
- **Rework**: Read review feedback, fix only that feedback, verify, push, and stop.
- **Done / Closed / Cancelled / Duplicate**: Exit immediately.

## Git and PR Rules

1. The shared branch is `symphony/issue-{{ issue.identifier | remove: "#" }}`.
2. All agents for the same issue use the same branch and the same PR.
3. Check for the PR before creating one:
   ```bash
   PR=$(gh pr list --head "symphony/issue-{{ issue.identifier | remove: '#' }}" --json number --jq '.[0].number')
   ```
4. If `PR` is empty and your role produced code changes, create exactly one PR:
   ```bash
   gh pr create --title "{{ issue.identifier }}: {{ issue.title }}" --body "Closes {{ issue.identifier }}"
   ```
5. If the PR already exists, push to the same branch. Do not create a replacement PR.
6. Use conventional commit messages such as `feat:`, `fix:`, `refactor:`, `test:`, or `docs:`.

## Symphony Workpad

Use one persistent issue comment as your workpad. Update that same comment instead of posting new progress comments.

{% if stage.role %}**Your workpad marker:** `## Symphony Workpad ({{ stage.role }})`{% else %}**Your workpad marker:** `## Symphony Workpad`{% endif %}

**Find or create the workpad**
```bash
{% if stage.role %}MARKER="## Symphony Workpad ({{ stage.role }})"{% else %}MARKER="## Symphony Workpad"{% endif %}
COMMENT_ID=$(gh api repos/{owner}/{repo}/issues/{{ issue.identifier | remove: "#" }}/comments --jq ".[] | select(.body | contains(\"$MARKER\")) | .id")
if [ -z "$COMMENT_ID" ]; then
  gh issue comment {{ issue.identifier }} --body "$MARKER
- [ ] Understand the task
- [ ] Implement or review
- [ ] Verify
- [ ] Final status / blocker"
  COMMENT_ID=$(gh api repos/{owner}/{repo}/issues/{{ issue.identifier | remove: "#" }}/comments --jq ".[] | select(.body | contains(\"$MARKER\")) | .id")
fi
```

**Update the existing workpad**
```bash
gh api repos/{owner}/{repo}/issues/comments/$COMMENT_ID -X PATCH -f body="$MARKER
- [x] Understand the task
- [x] Implement or review
- [x] Verify
- [x] Final status: ready for handoff"
```

When multiple stages run in parallel, each role owns one workpad comment and must not edit another role's workpad. Use Symphony's local coordination surface instead of extra issue comments:

- These helpers are provisioned automatically in `.symphony_bin`; do not try to install them manually. Note, mailbox, and claim commands use Symphony's internal coordination API when it is available.
- Codex sessions may expose native coordination tools named `symphony_note`, `symphony_mailbox`, and `symphony_claim`; prefer those when available.
- All coordination paths talk to the same Symphony backend. Codex native tools and shell helpers used by Claude or future agents can read and write the same mailbox, note, and claim state.
- `symphony-mailbox read` / `symphony-mailbox send <role> "..."` for direct active-role messages
- `symphony-note .symphony/coordination/shared.md "..."` for durable shared facts
- `symphony-note .symphony/coordination/handoffs.md "To reviewer: ..."` for durable future-attempt or end-of-run baton passes
- `symphony-claim list` before broad edits and `symphony-claim claim <scope> "reason"` before taking a shared path outside your normal lane

## Execution Contract

1. Read the issue, the current branch state, the PR state, and your workpad before changing anything.
2. Write a short focused plan in the workpad. Keep it specific to this issue only.
3. Make the smallest set of changes that fully resolves your role's responsibility.
4. Run only the verification needed for your changes. Prefer targeted tests over broad, expensive suites unless the issue requires more.
5. If verification fails because of your change, fix it. If verification fails for an unrelated pre-existing reason, document that clearly in the workpad and stop.
6. Push your work when it is ready. If code changed, ensure the PR exists before you stop.
7. Stop once the issue is ready for the next state. {% if stage.transition_to %}Symphony will transition completed stages toward `{{ stage.transition_to }}` when appropriate.{% else %}If your workflow does not auto-transition this state, update the issue label exactly once and stop.{% endif %}

## Review Contract

When your role is reviewing:

1. Review the current diff with `gh pr diff` and any relevant changed files.
2. Focus on correctness, regressions, tests, security, architectural fit, and workflow hygiene.
3. If parallel work or handoffs mattered, inspect `.symphony/coordination/events.tsv`, `shared.md`, or `handoffs.md` for context before deciding.
4. Treat coordination misuse as a review finding. That includes duplicate workpads or PRs, direct edits to another role's workpad, committed `.symphony/coordination/` or `.symphony_bin/` artifacts, or bypassing scope ownership in a way that caused overlap.
5. If the PR is acceptable, move the issue to `human-review`.
6. If the PR needs changes, leave actionable review feedback and move the issue to `rework`.
7. Do not rewrite the implementation during review unless the workflow explicitly says the reviewer should patch code.

## Rework Contract

When state is `rework`:

1. Read all open review feedback on the existing PR.
2. Fix only the requested feedback or directly related breakage.
3. Re-run the targeted verification for those fixes.
4. Push fixes to the same branch and stop. {% if stage.transition_to %}Symphony will move the issue toward `{{ stage.transition_to }}` when this stage finishes.{% else %}Update the issue back to `code-review` once and stop.{% endif %}

## Blockers and Handoff

If you are blocked by missing requirements, missing credentials, broken infrastructure, conflicting repo state, or repeated failed attempts:

1. Update the workpad with the exact blocker, what you tried, and the smallest useful next action for a human or later agent.
2. Leave the repo in a clean understandable state.
3. Stop. Do not keep retrying the same dead end.

{% if issue.labels.size > 0 %}
## Labels

{% for label in issue.labels %}- {{ label }}
{% endfor %}
{% endif %}

{% for blocker in issue.blocked_by %}
**Blocked by {{ blocker.identifier }} ({{ blocker.state }}).** Only proceed on clearly independent work.
{% endfor %}

## Final Checklist

Before stopping, make sure all of these are true:

- [ ] Scope stayed limited to {{ issue.identifier }}
- [ ] Workpad updated with the final outcome
- [ ] No duplicate branch, PR, or workpad comment was created
- [ ] No `.symphony/coordination/` or `.symphony_bin/` runtime artifacts were committed
- [ ] Verification was run or the reason it could not be run was documented
- [ ] If code changed, commits were pushed to the shared branch
- [ ] If code changed, the shared PR exists
- [ ] The next workflow state is unambiguous
