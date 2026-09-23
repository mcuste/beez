//! Reads lists of policy values written as text.

use std::fmt::Display;
use std::str::FromStr;

/// Parses every value, and stops at the first one Beez cannot read.
///
/// Manifests and command-line flags both hold policy values as text, so both
/// read them the same way.
pub fn parse_all<T>(values: &[String]) -> Result<Vec<T>, String>
where
    T: FromStr,
    T::Err: Display,
{
    values
        .iter()
        .map(|value| value.parse::<T>().map_err(|error| error.to_string()))
        .collect()
}

/// Parses every value, and drops the ones Beez cannot read.
///
/// Only Beez's own group tables use this, and a test checks that every one
/// of them parses.
pub(crate) fn parse_valid<T: FromStr>(values: &[&str]) -> Vec<T> {
    values
        .iter()
        .map(|value| value.parse())
        .filter_map(Result::ok)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::parse_all;
    use crate::SandboxPath;

    #[test]
    fn keeps_the_order_of_the_values() {
        let values = ["/work".to_owned(), "/tmp".to_owned()];

        let parsed = parse_all::<SandboxPath>(&values).unwrap();

        let text: Vec<String> = parsed.iter().map(SandboxPath::to_string).collect();
        assert_eq!(text, ["/work", "/tmp"]);
    }

    #[test]
    fn stops_at_the_value_it_cannot_read() {
        let values = ["/work".to_owned(), String::new()];

        let error = parse_all::<SandboxPath>(&values).unwrap_err();

        assert!(!error.is_empty());
    }
}
