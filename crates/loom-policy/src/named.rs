//! Values a manifest selects by name, such as a group or a harness.

/// A value with a fixed set of names.
pub(crate) trait Named: Copy + 'static {
    /// Every value, in the order an error message offers them.
    fn all() -> &'static [Self];

    /// The name a manifest writes for this value.
    fn named(self) -> &'static str;
}

/// The value `name` selects, or nothing when no value carries that name.
pub(crate) fn from_name<T: Named>(name: &str) -> Option<T> {
    T::all().iter().copied().find(|value| value.named() == name)
}

/// Every name, for an error that offers the choices.
pub(crate) fn names<T: Named>() -> String {
    T::all()
        .iter()
        .map(|value| value.named())
        .collect::<Vec<_>>()
        .join(", ")
}
