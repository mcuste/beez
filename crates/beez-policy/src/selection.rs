//! The named groups a policy section grants.

use std::collections::BTreeSet;

/// Default groups a section may disable, and groups it adds of its own.
///
/// `defaults` stays unset until a policy names it, so a task that only adds a
/// group keeps what the workflow chose.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GroupSelection<G> {
    defaults: Option<bool>,
    groups: Vec<G>,
    disable: Vec<G>,
}

impl<G> Default for GroupSelection<G> {
    fn default() -> Self {
        Self {
            defaults: None,
            groups: Vec::new(),
            disable: Vec::new(),
        }
    }
}

impl<G: Copy + Ord> GroupSelection<G> {
    pub(crate) fn new(defaults: Option<bool>, groups: Vec<G>, disable: Vec<G>) -> Self {
        Self {
            defaults,
            groups,
            disable,
        }
    }

    /// Adds the groups of `other` to this selection.
    ///
    /// The lists join. A `defaults` `other` sets replaces the value here, and
    /// one it leaves unset keeps the value here.
    pub(crate) fn merge(mut self, other: Self) -> Self {
        self.defaults = other.defaults.or(self.defaults);
        extend(&mut self.groups, other.groups);
        extend(&mut self.disable, other.disable);
        self
    }

    /// The groups the section grants: `default_groups` unless disabled, then
    /// its own.
    ///
    /// A group the section names always applies, even when it is also disabled.
    pub(crate) fn selected(&self, default_groups: &[G]) -> BTreeSet<G> {
        let defaults = self.defaults.unwrap_or(true);
        default_groups
            .iter()
            .filter(|group| defaults && !self.disable.contains(group))
            .chain(&self.groups)
            .copied()
            .collect()
    }
}

/// Adds every value that is not in `target` yet.
pub(crate) fn extend<T: PartialEq>(target: &mut Vec<T>, values: Vec<T>) {
    for value in values {
        if !target.contains(&value) {
            target.push(value);
        }
    }
}
