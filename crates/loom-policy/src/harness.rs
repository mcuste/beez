use std::fmt;

use crate::named::{self, name_table, parse_by_name};

/// A headless coding harness Loom can prompt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeadlessHarness {
    /// Pi coding agent.
    Pi,
    /// Oh My Pi coding agent.
    Omp,
    /// Claude Code agent.
    Claude,
    /// Codex coding agent.
    Codex,
}

name_table!(pub HeadlessHarness {
    Pi => "pi",
    Omp => "omp",
    Claude => "claude",
    Codex => "codex",
});

parse_by_name!(HeadlessHarness, HeadlessHarnessError::Unknown);

/// Reports an unsupported harness selector.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HeadlessHarnessError {
    /// The selector does not name a supported harness.
    Unknown(String),
}

impl fmt::Display for HeadlessHarnessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown(value) => {
                write!(
                    formatter,
                    "unsupported headless harness {value:?}; expected one of {}",
                    named::names::<HeadlessHarness>()
                )
            }
        }
    }
}

impl std::error::Error for HeadlessHarnessError {}

/// Model and effort settings for a harness prompt.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HarnessOptions {
    model: Option<String>,
    effort: Option<String>,
}

impl HarnessOptions {
    /// Builds harness options; `None` leaves the harness default.
    #[must_use]
    pub fn new(model: Option<String>, effort: Option<String>) -> Self {
        Self { model, effort }
    }

    /// The requested model.
    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// The requested reasoning effort.
    #[must_use]
    pub fn effort(&self) -> Option<&str> {
        self.effort.as_deref()
    }

    /// True when neither model nor effort is set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.model.is_none() && self.effort.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::{HeadlessHarness, HeadlessHarnessError};

    #[test]
    fn parses_the_name_of_every_supported_harness() {
        for &harness in HeadlessHarness::ALL {
            assert_eq!(harness.name().parse(), Ok(harness));
        }
    }

    #[test]
    fn offers_every_supported_name_in_the_error_message() {
        let offered = "cursor".parse::<HeadlessHarness>().unwrap_err().to_string();

        for harness in HeadlessHarness::ALL {
            assert!(
                offered.contains(harness.name()),
                "{offered} omits {harness}"
            );
        }
    }

    #[test]
    fn rejects_a_selector_that_differs_only_in_case() {
        assert_eq!(
            "Claude".parse::<HeadlessHarness>(),
            Err(HeadlessHarnessError::Unknown("Claude".into()))
        );
    }

    #[test]
    fn rejects_an_unsupported_harness() {
        let error = "cursor".parse::<HeadlessHarness>().unwrap_err();

        assert_eq!(error, HeadlessHarnessError::Unknown("cursor".into()));
        assert_eq!(
            error.to_string(),
            "unsupported headless harness \"cursor\"; expected one of pi, omp, claude, codex"
        );
    }
}
