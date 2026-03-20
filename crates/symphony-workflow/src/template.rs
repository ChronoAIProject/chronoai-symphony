//! Liquid prompt template rendering per Section 5.4 of the Symphony spec.
//!
//! Renders a Liquid template string with issue context and attempt number,
//! producing the final prompt sent to the agent.

use liquid::model::{Value as LiquidValue, to_value};
use liquid::{Object, Parser, ParserBuilder};
use symphony_core::domain::Issue;
use symphony_core::error::SymphonyError;

/// Default prompt used when the template string is empty.
const DEFAULT_PROMPT: &str = "You are working on an issue from GitHub.";

/// Render a Liquid prompt template with issue context.
///
/// # Arguments
///
/// - `template_str` - A Liquid template string. If empty, a default prompt is used.
/// - `issue` - The issue providing context variables.
/// - `attempt` - `None` for first run, `Some(n)` for retry attempt number.
///
/// # Template Variables
///
/// - `issue.id` - Stable tracker ID
/// - `issue.identifier` - Human-readable key (e.g., `#123`)
/// - `issue.title` - Issue title
/// - `issue.description` - Issue description (may be nil)
/// - `issue.priority` - Priority number (may be nil)
/// - `issue.state` - Current state string
/// - `issue.branch_name` - Suggested branch name (may be nil)
/// - `issue.url` - URL to the issue (may be nil)
/// - `issue.labels` - Array of label strings
/// - `issue.blocked_by` - Array of blocker objects with `id`, `identifier`, `state`
/// - `attempt` - nil on first run, integer on retry
///
/// # Errors
///
/// Returns `TemplateRenderError` if the template contains unknown variables,
/// unknown filters, or has syntax errors.
pub fn render_prompt(
    template_str: &str,
    issue: &Issue,
    attempt: Option<u32>,
) -> Result<String, SymphonyError> {
    let effective_template = if template_str.trim().is_empty() {
        DEFAULT_PROMPT
    } else {
        template_str
    };

    let parser = build_strict_parser()?;

    let compiled = parser.parse(effective_template).map_err(|e| {
        SymphonyError::TemplateRenderError {
            detail: format!("template parse error: {e}"),
        }
    })?;

    let globals = build_globals(issue, attempt)?;

    let rendered = compiled.render(&globals).map_err(|e| {
        SymphonyError::TemplateRenderError {
            detail: format!("template render error: {e}"),
        }
    })?;

    Ok(rendered)
}

/// Context for a pipeline stage, passed to `render_prompt_with_stage`.
#[derive(Clone, Debug, Default)]
pub struct StageContext {
    pub role: Option<String>,
    pub transition_to: Option<String>,
    pub reject_to: Option<String>,
    pub default_prompt: String,
}

/// Render a Liquid prompt template with both issue context and pipeline stage
/// context.
///
/// Adds the following Liquid variables on top of the standard `issue.*` and
/// `attempt` variables:
///
/// - `stage.role` - The role assigned to this stage (may be nil).
/// - `stage.transition_to` - The next state on success (may be nil).
/// - `stage.reject_to` - The next state on rejection (may be nil).
/// - `default_prompt` - The rendered default prompt (useful for two-pass
///   rendering where a stage prompt wraps the base prompt).
pub fn render_prompt_with_stage(
    template_str: &str,
    issue: &Issue,
    attempt: Option<u32>,
    stage: Option<&StageContext>,
) -> Result<String, SymphonyError> {
    let effective_template = if template_str.trim().is_empty() {
        DEFAULT_PROMPT
    } else {
        template_str
    };

    let parser = build_strict_parser()?;

    let compiled = parser.parse(effective_template).map_err(|e| {
        SymphonyError::TemplateRenderError {
            detail: format!("template parse error: {e}"),
        }
    })?;

    let mut globals = build_globals(issue, attempt)?;

    // Add stage variables when present.
    if let Some(ctx) = stage {
        let mut stage_obj = Object::new();
        stage_obj.insert(
            "role".into(),
            option_to_liquid(&ctx.role),
        );
        stage_obj.insert(
            "transition_to".into(),
            option_to_liquid(&ctx.transition_to),
        );
        stage_obj.insert(
            "reject_to".into(),
            option_to_liquid(&ctx.reject_to),
        );
        globals.insert("stage".into(), LiquidValue::Object(stage_obj));
        globals.insert(
            "default_prompt".into(),
            if ctx.default_prompt.is_empty() {
                LiquidValue::Nil
            } else {
                to_liquid_str(&ctx.default_prompt)
            },
        );
    }

    let rendered = compiled.render(&globals).map_err(|e| {
        SymphonyError::TemplateRenderError {
            detail: format!("template render error: {e}"),
        }
    })?;

    Ok(rendered)
}

/// Build a strict-mode Liquid parser that rejects unknown variables and filters.
fn build_strict_parser() -> Result<Parser, SymphonyError> {
    ParserBuilder::with_stdlib()
        .build()
        .map_err(|e| SymphonyError::TemplateRenderError {
            detail: format!("failed to build template parser: {e}"),
        })
}

/// Build the Liquid globals object from an `Issue` and optional attempt number.
fn build_globals(issue: &Issue, attempt: Option<u32>) -> Result<Object, SymphonyError> {
    let mut globals = Object::new();

    let issue_obj = build_issue_object(issue)?;
    globals.insert("issue".into(), LiquidValue::Object(issue_obj));

    let attempt_val = match attempt {
        Some(n) => to_value(&n).unwrap_or(LiquidValue::Nil),
        None => LiquidValue::Nil,
    };
    globals.insert("attempt".into(), attempt_val);

    Ok(globals)
}

/// Convert an `Issue` into a Liquid Object with all fields.
fn build_issue_object(issue: &Issue) -> Result<Object, SymphonyError> {
    let mut obj = Object::new();

    obj.insert("id".into(), to_liquid_str(&issue.id));
    obj.insert("identifier".into(), to_liquid_str(&issue.identifier));
    obj.insert("title".into(), to_liquid_str(&issue.title));
    obj.insert(
        "description".into(),
        option_to_liquid(&issue.description),
    );
    obj.insert(
        "priority".into(),
        issue
            .priority
            .map(|p| LiquidValue::Scalar(liquid::model::Scalar::new(p as i64)))
            .unwrap_or(LiquidValue::Nil),
    );
    obj.insert("state".into(), to_liquid_str(&issue.state));
    obj.insert(
        "branch_name".into(),
        option_to_liquid(&issue.branch_name),
    );
    obj.insert("url".into(), option_to_liquid(&issue.url));

    // Labels as array of strings.
    let labels: Vec<LiquidValue> = issue
        .labels
        .iter()
        .map(|l| to_liquid_str(l))
        .collect();
    obj.insert("labels".into(), LiquidValue::Array(labels));

    // Blocked-by as array of objects.
    let blocked_by: Vec<LiquidValue> = issue
        .blocked_by
        .iter()
        .map(|b| {
            let mut blocker = Object::new();
            blocker.insert("id".into(), option_to_liquid(&b.id));
            blocker.insert("identifier".into(), option_to_liquid(&b.identifier));
            blocker.insert("state".into(), option_to_liquid(&b.state));
            LiquidValue::Object(blocker)
        })
        .collect();
    obj.insert("blocked_by".into(), LiquidValue::Array(blocked_by));

    Ok(obj)
}

fn to_liquid_str(s: &str) -> LiquidValue {
    LiquidValue::Scalar(liquid::model::Scalar::new(s.to_string()))
}

fn option_to_liquid(opt: &Option<String>) -> LiquidValue {
    match opt {
        Some(s) => to_liquid_str(s),
        None => LiquidValue::Nil,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symphony_core::domain::BlockerRef;

    fn sample_issue() -> Issue {
        Issue {
            id: "issue-42".to_string(),
            identifier: "#42".to_string(),
            title: "Fix login bug".to_string(),
            description: Some("Users cannot log in with SSO.".to_string()),
            priority: Some(1),
            state: "Todo".to_string(),
            branch_name: Some("fix/login-bug".to_string()),
            url: Some("https://github.com/org/repo/issues/42".to_string()),
            labels: vec!["bug".to_string(), "auth".to_string()],
            blocked_by: vec![BlockerRef {
                id: Some("issue-41".to_string()),
                identifier: Some("#41".to_string()),
                state: Some("In Progress".to_string()),
            }],
            created_at: None,
            updated_at: None,
        }
    }

    #[test]
    fn basic_variable_substitution() {
        let template = "Fix {{ issue.identifier }}: {{ issue.title }}";
        let result = render_prompt(template, &sample_issue(), None).unwrap();
        assert_eq!(result, "Fix #42: Fix login bug");
    }

    #[test]
    fn description_access() {
        let template = "Issue: {{ issue.description }}";
        let result = render_prompt(template, &sample_issue(), None).unwrap();
        assert_eq!(result, "Issue: Users cannot log in with SSO.");
    }

    #[test]
    fn labels_iteration() {
        let template =
            "Labels: {% for label in issue.labels %}{{ label }}{% unless forloop.last %}, {% endunless %}{% endfor %}";
        let result = render_prompt(template, &sample_issue(), None).unwrap();
        assert_eq!(result, "Labels: bug, auth");
    }

    #[test]
    fn blocked_by_iteration() {
        let template = "Blocked by: {% for b in issue.blocked_by %}{{ b.identifier }}{% endfor %}";
        let result = render_prompt(template, &sample_issue(), None).unwrap();
        assert_eq!(result, "Blocked by: #41");
    }

    #[test]
    fn attempt_nil_on_first_run() {
        let template = "{% if attempt %}Retry {{ attempt }}{% else %}First run{% endif %}";
        let result = render_prompt(template, &sample_issue(), None).unwrap();
        assert_eq!(result, "First run");
    }

    #[test]
    fn attempt_integer_on_retry() {
        let template = "{% if attempt %}Retry {{ attempt }}{% else %}First run{% endif %}";
        let result = render_prompt(template, &sample_issue(), Some(2)).unwrap();
        assert_eq!(result, "Retry 2");
    }

    #[test]
    fn empty_template_uses_default() {
        let result = render_prompt("", &sample_issue(), None).unwrap();
        assert_eq!(result, DEFAULT_PROMPT);
    }

    #[test]
    fn whitespace_only_template_uses_default() {
        let result = render_prompt("   \n  ", &sample_issue(), None).unwrap();
        assert_eq!(result, DEFAULT_PROMPT);
    }

    #[test]
    fn priority_access() {
        let template = "Priority: {{ issue.priority }}";
        let result = render_prompt(template, &sample_issue(), None).unwrap();
        assert_eq!(result, "Priority: 1");
    }

    #[test]
    fn nil_description_renders_empty() {
        let mut issue = sample_issue();
        issue.description = None;
        let template = "Desc: {{ issue.description }}";
        let result = render_prompt(template, &issue, None).unwrap();
        assert_eq!(result, "Desc: ");
    }

    #[test]
    fn state_and_url_access() {
        let template = "State: {{ issue.state }}, URL: {{ issue.url }}";
        let result = render_prompt(template, &sample_issue(), None).unwrap();
        assert_eq!(
            result,
            "State: Todo, URL: https://github.com/org/repo/issues/42"
        );
    }

    #[test]
    fn syntax_error_returns_template_error() {
        let template = "{% if %}broken";
        let err = render_prompt(template, &sample_issue(), None).unwrap_err();
        match err {
            SymphonyError::TemplateRenderError { .. } => {}
            other => panic!("expected TemplateRenderError, got: {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // render_prompt_with_stage tests
    // -----------------------------------------------------------------------

    #[test]
    fn render_with_stage_role() {
        let template = "Role: {{ stage.role }}";
        let ctx = StageContext {
            role: Some("reviewer".to_string()),
            transition_to: None,
            reject_to: None,
            default_prompt: String::new(),
        };
        let result =
            render_prompt_with_stage(template, &sample_issue(), None, Some(&ctx)).unwrap();
        assert_eq!(result, "Role: reviewer");
    }

    #[test]
    fn render_with_stage_transition_targets() {
        let template = "Next: {{ stage.transition_to }}, Back: {{ stage.reject_to }}";
        let ctx = StageContext {
            role: None,
            transition_to: Some("human-review".to_string()),
            reject_to: Some("rework".to_string()),
            default_prompt: String::new(),
        };
        let result =
            render_prompt_with_stage(template, &sample_issue(), None, Some(&ctx)).unwrap();
        assert_eq!(result, "Next: human-review, Back: rework");
    }

    #[test]
    fn render_with_default_prompt() {
        let base_template = "Base for {{ issue.identifier }}";
        let base_rendered = render_prompt(base_template, &sample_issue(), None).unwrap();

        let stage_template = "Stage prompt. {{ default_prompt }}";
        let ctx = StageContext {
            role: Some("implementer".to_string()),
            transition_to: None,
            reject_to: None,
            default_prompt: base_rendered.clone(),
        };
        let result =
            render_prompt_with_stage(stage_template, &sample_issue(), None, Some(&ctx))
                .unwrap();
        assert_eq!(result, format!("Stage prompt. {base_rendered}"));
    }

    #[test]
    fn render_with_stage_nil_role() {
        let template = "{% if stage.role %}R: {{ stage.role }}{% else %}no role{% endif %}";
        let ctx = StageContext {
            role: None,
            transition_to: None,
            reject_to: None,
            default_prompt: String::new(),
        };
        let result =
            render_prompt_with_stage(template, &sample_issue(), None, Some(&ctx)).unwrap();
        assert_eq!(result, "no role");
    }

    #[test]
    fn render_with_stage_none_falls_back_to_regular() {
        let template = "Issue: {{ issue.title }}";
        let result =
            render_prompt_with_stage(template, &sample_issue(), None, None).unwrap();
        assert_eq!(result, "Issue: Fix login bug");
    }
}
