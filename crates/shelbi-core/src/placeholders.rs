//! `{{var}}` substitution against a task's frontmatter parameters.
//!
//! Workflow git fields support `{{name}}` placeholders that resolve from
//! a task's params at task-load time — see `Plans/workflows.md` §12 and
//! the "Parameterization" subsection. Substitution is plain string
//! replacement: no conditionals, no expressions, no defaults.
//!
//! This module is just the scanner. The error-reporting layer that turns
//! an unresolved `{{feature}}` into the user-facing *"add `feature:
//! <value>` to the task frontmatter"* message lives at the call site
//! (e.g. [`crate::Workflow::resolve_git`]) so it can name the workflow.

use std::collections::BTreeMap;

/// Walk `template` and replace every `{{key}}` with `params[key]`. Keys
/// not present in `params` are recorded into `missing` (no duplicates,
/// first-occurrence order preserved) and the original `{{key}}` text is
/// left in place — callers check `missing` after the call to decide
/// whether to surface a parameterization error or use the returned
/// string.
///
/// Whitespace inside the braces is tolerated: `{{ feature }}` resolves
/// the same as `{{feature}}`. A token whose body trims to empty
/// (`{{}}` or `{{   }}`) is treated as literal text — there's no key to
/// look up, so the source string is preserved unchanged. A `{{` with no
/// matching `}}` is also literal; callers can author files containing
/// `{{` in prose without it being mistaken for an open placeholder.
pub fn substitute_placeholders(
    template: &str,
    params: &BTreeMap<String, String>,
    missing: &mut Vec<String>,
) -> String {
    let mut out = String::with_capacity(template.len());
    let mut remaining = template;

    loop {
        let Some(open) = remaining.find("{{") else {
            out.push_str(remaining);
            return out;
        };
        out.push_str(&remaining[..open]);
        let after_open = &remaining[open + 2..];

        let Some(close) = after_open.find("}}") else {
            // No closing braces — emit the opening `{{` as literal and
            // keep scanning. This matters because a documentation string
            // might legitimately contain `{{` without it being a token.
            out.push_str("{{");
            remaining = after_open;
            continue;
        };

        let raw = &after_open[..close];
        let key = raw.trim();

        if key.is_empty() || key.chars().any(char::is_whitespace) {
            // Empty body (`{{}}`) or internal whitespace (`{{a b}}`):
            // not a syntactically valid token. Preserve the source
            // verbatim so users can see what they wrote.
            out.push_str("{{");
            out.push_str(raw);
            out.push_str("}}");
        } else if let Some(v) = params.get(key) {
            out.push_str(v);
        } else {
            // Record the missing key once, then leave the unresolved
            // token in the output so a caller that ignores `missing`
            // (e.g., a logger) still shows what wasn't found.
            let key_owned = key.to_string();
            if !missing.contains(&key_owned) {
                missing.push(key_owned);
            }
            out.push_str("{{");
            out.push_str(raw);
            out.push_str("}}");
        }

        remaining = &after_open[close + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn substitutes_a_single_token() {
        let mut missing = Vec::new();
        let out = substitute_placeholders(
            "feature/{{feature}}",
            &params(&[("feature", "auth-rewrite")]),
            &mut missing,
        );
        assert_eq!(out, "feature/auth-rewrite");
        assert!(missing.is_empty());
    }

    #[test]
    fn substitutes_multiple_distinct_tokens() {
        let mut missing = Vec::new();
        let out = substitute_placeholders(
            "{{team}}/{{feature}}",
            &params(&[("feature", "dashboard"), ("team", "growth")]),
            &mut missing,
        );
        assert_eq!(out, "growth/dashboard");
        assert!(missing.is_empty());
    }

    #[test]
    fn substitutes_repeated_tokens() {
        let mut missing = Vec::new();
        let out = substitute_placeholders(
            "feature/{{feature}}/{{feature}}",
            &params(&[("feature", "auth-rewrite")]),
            &mut missing,
        );
        assert_eq!(out, "feature/auth-rewrite/auth-rewrite");
    }

    #[test]
    fn tolerates_inner_whitespace_in_token() {
        let mut missing = Vec::new();
        let out = substitute_placeholders(
            "{{ feature }}",
            &params(&[("feature", "x")]),
            &mut missing,
        );
        assert_eq!(out, "x");
        assert!(missing.is_empty());
    }

    #[test]
    fn passes_strings_with_no_tokens_through_untouched() {
        let mut missing = Vec::new();
        let out = substitute_placeholders("main", &params(&[]), &mut missing);
        assert_eq!(out, "main");
        assert!(missing.is_empty());
    }

    #[test]
    fn records_missing_keys_once_in_order() {
        let mut missing = Vec::new();
        let out = substitute_placeholders(
            "{{a}}-{{b}}-{{a}}",
            &params(&[]),
            &mut missing,
        );
        // Unresolved tokens are left in place so the user can see what
        // the source said.
        assert_eq!(out, "{{a}}-{{b}}-{{a}}");
        assert_eq!(missing, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn accumulates_across_multiple_calls() {
        let mut missing = Vec::new();
        substitute_placeholders("{{a}}", &params(&[]), &mut missing);
        substitute_placeholders("{{b}}", &params(&[]), &mut missing);
        // Caller threads the same `missing` buffer through every field
        // it resolves so the final error message can list every gap at
        // once, not one per call.
        assert_eq!(missing, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn treats_empty_body_as_literal() {
        let mut missing = Vec::new();
        let out = substitute_placeholders("{{}}", &params(&[]), &mut missing);
        assert_eq!(out, "{{}}");
        assert!(missing.is_empty());
    }

    #[test]
    fn treats_whitespace_body_as_literal() {
        let mut missing = Vec::new();
        let out =
            substitute_placeholders("{{   }}", &params(&[]), &mut missing);
        assert_eq!(out, "{{   }}");
        assert!(missing.is_empty());
    }

    #[test]
    fn treats_internal_whitespace_in_key_as_literal() {
        // A real token has a single identifier between the braces; `{{a
        // b}}` is suspicious enough that we'd rather preserve the source
        // than silently fail-or-substitute. No key is looked up.
        let mut missing = Vec::new();
        let out = substitute_placeholders(
            "{{a b}}",
            &params(&[("a", "x"), ("b", "y")]),
            &mut missing,
        );
        assert_eq!(out, "{{a b}}");
        assert!(missing.is_empty());
    }

    #[test]
    fn unclosed_open_braces_pass_through() {
        let mut missing = Vec::new();
        let out = substitute_placeholders(
            "prefix {{ no close here",
            &params(&[]),
            &mut missing,
        );
        assert_eq!(out, "prefix {{ no close here");
        assert!(missing.is_empty());
    }

    #[test]
    fn does_not_recurse_into_substituted_value() {
        // A param value that itself contains `{{x}}` is emitted verbatim
        // — single-pass substitution avoids infinite loops and surprise
        // chained resolution.
        let mut missing = Vec::new();
        let out = substitute_placeholders(
            "{{a}}",
            &params(&[("a", "{{a}}")]),
            &mut missing,
        );
        assert_eq!(out, "{{a}}");
        assert!(missing.is_empty());
    }
}
