---
tracker:
  kind: github
  api_key: $GITHUB_TOKEN
  project_slug: your-org/your-repo
  active_states:
    - Todo
    - In Progress
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
    git clone https://github.com/{{ issue.identifier | remove: "#" }}.git . 2>/dev/null || true
  before_run: |
    echo "Preparing workspace for {{ issue.identifier }}"
  after_run: |
    echo "Finished run for {{ issue.identifier }}"

agent:
  max_concurrent_agents: 5
  max_turns: 20
  max_retry_backoff_ms: 300000

codex:
  command: codex app-server
  turn_timeout_ms: 3600000
  read_timeout_ms: 5000
  stall_timeout_ms: 300000
---

You are a coding agent working on issue {{ issue.identifier }}: {{ issue.title }}.

## Issue Details

- **ID**: {{ issue.id }}
- **State**: {{ issue.state }}
- **Priority**: {{ issue.priority }}
- **URL**: {{ issue.url }}

{% if issue.description %}
## Description

{{ issue.description }}
{% endif %}

{% if issue.labels.size > 0 %}
## Labels

{% for label in issue.labels %}- {{ label }}
{% endfor %}
{% endif %}

{% if issue.blocked_by.size > 0 %}
## Blocked By

{% for blocker in issue.blocked_by %}- {{ blocker.identifier }} ({{ blocker.state }})
{% endfor %}
{% endif %}

{% if attempt %}
## Retry Information

This is retry attempt {{ attempt }}. Review what was done in the previous attempt and continue from where you left off.
{% endif %}

## Instructions

1. Read the issue description carefully.
2. Understand the codebase and relevant files.
3. Implement the required changes.
4. Write tests for your changes.
5. Ensure all tests pass.
6. Create a pull request with your changes.

When done, transition the issue to "Human Review" state by adding the appropriate label.
