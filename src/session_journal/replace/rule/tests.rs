use super::*;

#[test]
fn journal_replace_rule_uses_literal_text_and_unicode_case_options() {
    let exact = Rule::compile("Ä.", "$0\\", false, false).unwrap();
    let folded = Rule::compile("Ä.", "$0\\", false, true).unwrap();
    assert_eq!(
        exact.rewrite("Ä. ä. äx").unwrap().unwrap().as_str(),
        "$0\\ ä. äx"
    );
    assert_eq!(
        folded.rewrite("Ä. ä. äx").unwrap().unwrap().as_str(),
        "$0\\ $0\\ äx"
    );
    assert!(folded.rewrite("äx").unwrap().is_none());
    assert!(
        Rule::compile("ä", "ä", false, false)
            .unwrap()
            .rewrite("ä")
            .unwrap()
            .is_none()
    );
}

#[test]
fn journal_replace_rule_applies_java_groups_escaping_and_fallback() {
    let rule = Rule::compile(
        "(?<word>alpha)(beta)?",
        r"${word}:$2:$10:$0:\$",
        true,
        false,
    )
    .unwrap();
    assert_eq!(
        rule.rewrite("alpha alphabeta").unwrap().unwrap().as_str(),
        "alpha::alpha0:alpha:$ alpha:beta:alpha0:alphabeta:$",
    );

    let invalid_group = Rule::compile("(alpha)", r"$1:${unknown}:\$", true, false).unwrap();
    assert_eq!(
        invalid_group.rewrite("alpha").unwrap().unwrap().as_str(),
        r"$1:${unknown}:\$",
    );

    let rule = Rule::compile("a*", "-", true, false).unwrap();
    assert_eq!(rule.rewrite("a").unwrap().unwrap().as_str(), "--");
    assert!(rule.rewrite("").unwrap().is_none());
}

#[test]
fn journal_replace_rule_bounds_and_diagnostics_hide_input() {
    let secret = "sensitive-search[";
    let rule = Rule::compile(secret, "sensitive-replacement", false, false).unwrap();
    let diagnostic = format!("{rule:?}");
    assert!(!diagnostic.contains(secret) && !diagnostic.contains("sensitive-replacement"));
    let error = Rule::compile(secret, "sensitive-replacement", true, false).unwrap_err();
    assert!(!error.to_string().contains(secret));
    assert!(Rule::compile(&"x".repeat(MAX_RULE_BYTES + 1), "", false, false).is_err());
    let expanding = Rule::compile("x", &"y".repeat(MAX_RULE_BYTES), false, false).unwrap();
    let text = "x".repeat(MAX_FIELD_BYTES / MAX_RULE_BYTES + 1);
    assert!(expanding.rewrite(&text).is_err());
}
