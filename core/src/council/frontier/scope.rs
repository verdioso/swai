//! Audit scope, changed files, and finding types.

use serde::{Deserialize, Serialize};

/// What to review. Carries a human-readable title (commit message, phase name,
/// or manual audit label) plus the files to review.
///
/// Each file may carry either a unified diff OR its full content — the prompt
/// builder prefers the diff when present (Phase 35 §3: "the diff (or full
/// changed files if the diff is small)").
#[derive(Debug, Clone, Default)]
pub struct AuditScope {
    /// A short label for the review (shown to the auditor for context).
    pub title: String,
    /// Files to review.
    pub files: Vec<ChangedFile>,
}

impl AuditScope {
    /// Create an empty scope with the given title.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            files: Vec::new(),
        }
    }

    /// Add a file to review. `diff` is a unified-diff string; `content` is the
    /// full file text. At least one should be set.
    pub fn with_file(mut self, path: impl Into<String>, diff: Option<String>, content: Option<String>) -> Self {
        self.files.push(ChangedFile {
            path: path.into(),
            diff,
            content,
        });
        self
    }
}

/// A single file included in an audit scope.
#[derive(Debug, Clone, Default)]
pub struct ChangedFile {
    /// Repository-relative path.
    pub path: String,
    /// Unified diff hunks, when reviewing a diff.
    pub diff: Option<String>,
    /// Full file content, when reviewing whole files.
    pub content: Option<String>,
}

/// Severity of a single audit finding. Ordered least → most severe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FindingSeverity {
    /// Trivial / cosmetic.
    Info,
    /// Minor, low impact.
    Low,
    /// Moderate impact.
    Medium,
    /// Serious, should be fixed.
    High,
    /// Critical / security-relevant.
    Critical,
}

impl FindingSeverity {
    /// Parse a free-text severity label from an auditor response. Falls back to
    /// [`FindingSeverity::Medium`] for unrecognized values.
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_lowercase().as_str() {
            s if s.contains("critical") || s.contains("blocker") || s.contains("urgent") => {
                FindingSeverity::Critical
            }
            s if s.contains("high") || s.contains("serious") || s.contains("severity: high") => {
                FindingSeverity::High
            }
            s if s.contains("medium") || s.contains("med") || s.contains("moderate") => {
                FindingSeverity::Medium
            }
            s if s.contains("low") => FindingSeverity::Low,
            s if s.contains("info") || s.contains("informational") || s.contains("cosmetic") => {
                FindingSeverity::Info
            }
            _ => FindingSeverity::Medium,
        }
    }

    /// Numeric ordering (higher = more severe), handy for sorting/reporting.
    pub fn rank(self) -> u8 {
        match self {
            FindingSeverity::Info => 0,
            FindingSeverity::Low => 1,
            FindingSeverity::Medium => 2,
            FindingSeverity::High => 3,
            FindingSeverity::Critical => 4,
        }
    }
}

/// A single structured finding produced by an auditor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Severity of the finding.
    pub severity: FindingSeverity,
    /// Component / subsystem the finding relates to (e.g. "network", "parsing").
    pub component: String,
    /// Location hint (file:line, function name, etc.).
    pub location: String,
    /// Human-readable description of the issue.
    pub description: String,
    /// Suggested remediation, if the auditor provided one.
    #[serde(default)]
    pub suggestion: String,
}

/// The complete result of an audit call.
#[derive(Debug, Clone)]
pub struct AuditResult {
    /// Which provider produced this result.
    pub provider: FrontierProvider,
    /// Model id used.
    pub model: String,
    /// Structured findings (empty when `inconclusive`).
    pub findings: Vec<Finding>,
    /// True when the response could not be parsed into findings. An inconclusive
    /// result must be surfaced to the user as "audit inconclusive", NOT as a
    /// clean pass (Phase 35 §6).
    pub inconclusive: bool,
    /// Short excerpt of the raw response for debugging / manual review.
    pub raw_excerpt: String,
    /// Estimated input tokens (prompt size / ~4 tokens/char).
    pub input_tokens: usize,
    /// Estimated output tokens (derived from finding count).
    pub output_tokens: usize,
}

use super::provider::FrontierProvider;
