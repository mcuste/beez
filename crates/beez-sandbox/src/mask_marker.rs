//! Names of the files that mark a mask directory as in use by a run.

/// Starts the name of every marker, which goes on with the owner's process
/// ID and a sequence number.
pub(crate) const MARKER_PREFIX: &str = ".beez-mask-";

/// The process ID of the run that wrote the marker `name`.
///
/// `None` for a name that carries no process ID, which no live run wrote.
pub(crate) fn marker_owner(name: &str) -> Option<u32> {
    name.strip_prefix(MARKER_PREFIX)?
        .split('-')
        .next()?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::{MARKER_PREFIX, marker_owner};

    #[test]
    fn reads_the_owner_out_of_a_marker_name() {
        assert_eq!(marker_owner(&format!("{MARKER_PREFIX}4242-7")), Some(4242));
    }

    #[test]
    fn finds_no_owner_in_a_marker_without_a_process_id() {
        for name in [
            MARKER_PREFIX.to_owned(),
            format!("{MARKER_PREFIX}-3"),
            format!("{MARKER_PREFIX}self-3"),
            format!("{MARKER_PREFIX}99999999999-3"),
            "unrelated".to_owned(),
        ] {
            assert_eq!(marker_owner(&name), None, "{name}");
        }
    }
}
