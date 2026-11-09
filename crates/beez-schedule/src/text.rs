//! Small checks on text.

/// True for one or more ASCII digits and nothing else, so no sign slips in.
pub(crate) fn is_ascii_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}
