//! The named groups a policy section grants.

use std::collections::BTreeSet;

/// A setting that stays unset until a policy names it.
///
/// On merge a value the later policy sets wins, and one it leaves unset keeps
/// the earlier value, so a task that only adds a rule keeps what the workflow
/// chose.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Override<T: Copy>(Option<T>);

impl<T: Copy> Default for Override<T> {
    fn default() -> Self {
        Self(None)
    }
}

impl<T: Copy> From<Option<T>> for Override<T> {
    fn from(value: Option<T>) -> Self {
        Self(value)
    }
}

impl<T: Copy> Override<T> {
    /// The value `later` sets, or else the value here.
    pub(crate) fn merge(self, later: Self) -> Self {
        Self(later.0.or(self.0))
    }

    /// The value set, or `default` while unset.
    pub(crate) fn get_or(self, default: T) -> T {
        self.0.unwrap_or(default)
    }
}

/// Default groups a section may disable, and groups it adds of its own.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GroupSelection<G> {
    defaults: Override<bool>,
    groups: Vec<G>,
    disable: Vec<G>,
}

impl<G> Default for GroupSelection<G> {
    fn default() -> Self {
        Self {
            defaults: Override::default(),
            groups: Vec::new(),
            disable: Vec::new(),
        }
    }
}

impl<G: Copy + Ord> GroupSelection<G> {
    /// Sets whether the default groups apply; `None` leaves it unset.
    pub(crate) fn with_defaults(mut self, defaults: Option<bool>) -> Self {
        self.defaults = defaults.into();
        self
    }

    /// Sets the groups the section adds.
    pub(crate) fn with_groups(mut self, groups: Vec<G>) -> Self {
        self.groups = groups;
        self
    }

    /// Sets the default groups the section disables.
    pub(crate) fn with_disable(mut self, disable: Vec<G>) -> Self {
        self.disable = disable;
        self
    }

    /// Adds the groups of `other` to this selection.
    ///
    /// The lists join. A `defaults` `other` sets replaces the value here, and
    /// one it leaves unset keeps the value here.
    pub(crate) fn merge(mut self, other: Self) -> Self {
        self.defaults = self.defaults.merge(other.defaults);
        extend(&mut self.groups, other.groups);
        extend(&mut self.disable, other.disable);
        self
    }

    /// The groups the section grants: `default_groups` unless disabled, then
    /// its own.
    ///
    /// A group the section names always applies, even when it is also disabled.
    pub(crate) fn selected(&self, default_groups: &[G]) -> BTreeSet<G> {
        let defaults = self.defaults.get_or(true);
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
