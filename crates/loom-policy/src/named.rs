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

/// The error text for a name no value of `T` carries, with every choice.
pub(crate) fn unknown<T: Named>(kind: &str, name: &str) -> String {
    let names = T::all()
        .iter()
        .map(|value| value.named())
        .collect::<Vec<_>>()
        .join(", ");

    format!("unknown {kind} {name:?}; expected one of {names}")
}

/// Gives an enum its name table: `ALL`, `name`, `Display`, and [`Named`].
///
/// The table lists every value once, in the order an error message offers
/// them. A new value needs one line here and its match arms elsewhere.
macro_rules! name_table {
    ($vis:vis $type:ident { $($variant:ident => $name:literal),+ $(,)? }) => {
        impl $type {
            /// Every value, in `FromStr` name order.
            $vis const ALL: &[Self] = &[$(Self::$variant),+];

            /// The name a manifest uses for the value.
            #[must_use]
            $vis fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)+
                }
            }
        }

        impl $crate::named::Named for $type {
            fn all() -> &'static [Self] {
                Self::ALL
            }

            fn named(self) -> &'static str {
                self.name()
            }
        }

        impl ::std::fmt::Display for $type {
            fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                formatter.write_str(self.name())
            }
        }
    };
}

/// Reads a value of a name table, and reports an unknown name with the given
/// error variant.
macro_rules! parse_by_name {
    ($type:ident, $error:ident :: $unknown:ident) => {
        impl ::std::str::FromStr for $type {
            type Err = $error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                $crate::named::from_name(value).ok_or_else(|| $error::$unknown(value.to_owned()))
            }
        }
    };
}

pub(crate) use {name_table, parse_by_name};
