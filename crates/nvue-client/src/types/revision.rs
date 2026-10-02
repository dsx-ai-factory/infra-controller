use std::collections::BTreeMap;

use serde_json::Value as JsonValue;

/// The configuration difference returned by NVUE's config diff operation.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(transparent)]
pub struct RevisionConfigDiff(
    // We're intentionally typing this fairly loosely for code brevity, since the
    // sole user of this is concerned only with whether the diff is empty.
    serde_json::Map<String, serde_json::Value>,
);

impl RevisionConfigDiff {
    /// Return `true` for an empty diff, which means a no-op change between the
    /// revisions.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RevisionData {
    pub message: Option<String>,
    pub state: Option<String>,
    pub transition: Option<RevisionTransition>,
    pub last_apply: Option<JsonValue>,
    pub additional_data: Option<JsonValue>,
    pub auto_prompt: Option<JsonValue>,
    pub state_controls: Option<JsonValue>,
}

impl RevisionData {
    pub(crate) fn apply_status(&self) -> RevisionApplyStatus {
        let error_issues = self.error_issue_summaries();
        if !error_issues.is_empty() {
            return RevisionApplyStatus::Failed(error_issues);
        }

        if self.state.as_deref() == Some("applied") {
            return RevisionApplyStatus::Applied;
        }

        RevisionApplyStatus::Pending
    }

    pub(crate) fn transition_progress(&self) -> Option<&str> {
        self.transition
            .as_ref()
            .and_then(|transition| transition.progress.as_deref())
    }

    pub(crate) fn error_issue_summaries(&self) -> Vec<RevisionIssueSummary> {
        self.transition
            .as_ref()
            .and_then(|transition| transition.issue.as_ref())
            .into_iter()
            .flat_map(|issues| issues.iter())
            .filter(|(_, issue)| issue.severity == Some(RevisionIssueSeverity::Error))
            .map(|(issue_id, issue)| RevisionIssueSummary {
                issue_id: issue_id.clone(),
                severity: RevisionIssueSeverity::Error,
                code: issue.code.clone(),
                message: issue.message.clone(),
                data: issue.data.clone(),
            })
            .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
// Note that this doesn't exactly model the `status` field of an NVUE revision,
// despite some resemblance in the variant names.
pub(crate) enum RevisionApplyStatus {
    Applied,
    Pending,
    Failed(Vec<RevisionIssueSummary>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RevisionIssueSummary {
    pub issue_id: String,
    pub severity: RevisionIssueSeverity,
    pub code: Option<String>,
    pub message: Option<String>,
    pub data: Option<BTreeMap<String, String>>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RevisionTransition {
    pub progress: Option<String>,
    pub issue: Option<BTreeMap<String, RevisionIssue>>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RevisionIssue {
    pub severity: Option<RevisionIssueSeverity>,
    pub code: Option<String>,
    pub message: Option<String>,
    pub data: Option<BTreeMap<String, String>>,
}

#[derive(Clone, Copy, Debug, serde::Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum RevisionIssueSeverity {
    Error,
    Warning,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_config_diff_requires_object_root_and_reports_empty_root() {
        struct Case {
            name: &'static str,
            json: &'static str,
            expected: Option<(bool, JsonValue)>,
        }

        let cases = [
            Case {
                name: "empty root object means no difference",
                json: r#"{}"#,
                expected: Some((true, serde_json::json!({}))),
            },
            Case {
                name: "nested null remains a significant removal",
                json: r#"{"interface":{"swp1":{"link":{"state":null}}}}"#,
                expected: Some((
                    false,
                    serde_json::json!({
                        "interface": {
                            "swp1": {
                                "link": {
                                    "state": null
                                }
                            }
                        }
                    }),
                )),
            },
            Case {
                name: "null root is invalid",
                json: "null",
                expected: None,
            },
            Case {
                name: "array root is invalid",
                json: "[]",
                expected: None,
            },
            Case {
                name: "scalar root is invalid",
                json: r#""value""#,
                expected: None,
            },
        ];

        for case in cases {
            match (
                serde_json::from_str::<RevisionConfigDiff>(case.json),
                case.expected,
            ) {
                (Ok(diff), Some((expected_empty, expected_value))) => {
                    assert_eq!(diff.is_empty(), expected_empty, "{}", case.name);
                    assert_eq!(JsonValue::Object(diff.0), expected_value, "{}", case.name);
                }
                (Err(_), None) => {}
                (Ok(_), None) => panic!("{}: expected deserialization to fail", case.name),
                (Err(error), Some(_)) => {
                    panic!(
                        "{}: expected deserialization to succeed: {error}",
                        case.name
                    )
                }
            }
        }
    }

    #[test]
    fn classifies_revision_apply_status() {
        struct Case {
            name: &'static str,
            revision: RevisionData,
            expected_status: RevisionApplyStatus,
            expected_progress: Option<&'static str>,
        }

        let cases = [
            Case {
                name: "applied revision without issues succeeds",
                revision: revision(Some("applied"), None, vec![]),
                expected_status: RevisionApplyStatus::Applied,
                expected_progress: None,
            },
            Case {
                name: "applying revision without issues remains pending",
                revision: revision(Some("apply"), Some("checking"), vec![]),
                expected_status: RevisionApplyStatus::Pending,
                expected_progress: Some("checking"),
            },
            Case {
                name: "unknown state without issues remains pending",
                revision: revision(Some("unknown"), None, vec![]),
                expected_status: RevisionApplyStatus::Pending,
                expected_progress: None,
            },
            Case {
                name: "missing state without issues remains pending",
                revision: revision(None, None, vec![]),
                expected_status: RevisionApplyStatus::Pending,
                expected_progress: None,
            },
            Case {
                name: "warning-only issues remain pending",
                revision: revision(
                    Some("apply"),
                    Some("validating"),
                    vec![(
                        "1",
                        issue(
                            RevisionIssueSeverity::Warning,
                            Some("sample-warning"),
                            Some("sample warning"),
                            None,
                        ),
                    )],
                ),
                expected_status: RevisionApplyStatus::Pending,
                expected_progress: Some("validating"),
            },
            Case {
                name: "error issue fails with issue summary",
                revision: revision(
                    Some("apply"),
                    Some("failed"),
                    vec![(
                        "2",
                        issue(
                            RevisionIssueSeverity::Error,
                            Some("sample-error"),
                            Some("sample error"),
                            Some(BTreeMap::from([(
                                "path".to_string(),
                                "/system".to_string(),
                            )])),
                        ),
                    )],
                ),
                expected_status: RevisionApplyStatus::Failed(vec![RevisionIssueSummary {
                    issue_id: "2".to_string(),
                    severity: RevisionIssueSeverity::Error,
                    code: Some("sample-error".to_string()),
                    message: Some("sample error".to_string()),
                    data: Some(BTreeMap::from([(
                        "path".to_string(),
                        "/system".to_string(),
                    )])),
                }]),
                expected_progress: Some("failed"),
            },
        ];

        for case in cases {
            assert_eq!(
                case.revision.apply_status(),
                case.expected_status,
                "{}",
                case.name
            );
            assert_eq!(
                case.revision.transition_progress(),
                case.expected_progress,
                "{}",
                case.name
            );
        }
    }

    fn revision(
        state: Option<&str>,
        progress: Option<&str>,
        issues: Vec<(&str, RevisionIssue)>,
    ) -> RevisionData {
        RevisionData {
            message: None,
            state: state.map(str::to_owned),
            transition: Some(RevisionTransition {
                progress: progress.map(str::to_owned),
                issue: (!issues.is_empty()).then(|| {
                    issues
                        .into_iter()
                        .map(|(issue_id, issue)| (issue_id.to_string(), issue))
                        .collect()
                }),
            }),
            last_apply: None,
            additional_data: None,
            auto_prompt: None,
            state_controls: None,
        }
    }

    fn issue(
        severity: RevisionIssueSeverity,
        code: Option<&str>,
        message: Option<&str>,
        data: Option<BTreeMap<String, String>>,
    ) -> RevisionIssue {
        RevisionIssue {
            severity: Some(severity),
            code: code.map(str::to_owned),
            message: message.map(str::to_owned),
            data,
        }
    }

    #[test]
    fn parses_revision_data_with_transition_issue() {
        let json = r#"
        {
            "additional-data": {
                "parent-revision-id": "applied"
            },
            "auto-prompt": {
                "ays": "ays_yes"
            },
            "last-apply": {
                "apply-id": "rev_1_apply_1"
            },
            "message": "apply config",
            "state": "apply",
            "state-controls": {
                "apply-type": "API"
            },
            "transition": {
                "progress": "checking",
                "issue": {
                    "1": {
                        "severity": "warning",
                        "code": "sample-warning",
                        "message": "sample warning",
                        "data": {
                            "path": "/system"
                        }
                    }
                }
            }
        }
        "#;

        let revision: RevisionData = serde_json::from_str(json).expect("revision should parse");

        assert_eq!(revision.message.as_deref(), Some("apply config"));
        assert_eq!(revision.state.as_deref(), Some("apply"));
        assert!(revision.additional_data.is_some());
        assert!(revision.auto_prompt.is_some());
        assert!(revision.last_apply.is_some());
        assert!(revision.state_controls.is_some());

        let transition = revision.transition.expect("transition should parse");
        assert_eq!(transition.progress.as_deref(), Some("checking"));

        let issues = transition.issue.expect("issues should parse");
        let issue = issues.get("1").expect("issue should parse");
        assert_eq!(issue.severity, Some(RevisionIssueSeverity::Warning));
        assert_eq!(issue.code.as_deref(), Some("sample-warning"));
        assert_eq!(issue.message.as_deref(), Some("sample warning"));
        assert_eq!(
            issue
                .data
                .as_ref()
                .and_then(|data| data.get("path"))
                .map(String::as_str),
            Some("/system")
        );
    }
}
