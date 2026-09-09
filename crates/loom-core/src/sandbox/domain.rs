use std::fmt;
use std::str::FromStr;

/// Reports an invalid domain rule.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DomainRuleError {
    /// The rule has no host.
    Empty,
    /// The host contains a character outside letters, digits, `-`, and `.`.
    InvalidCharacter(char),
    /// A `*` appears anywhere but as the first label.
    MisplacedWildcard,
    /// The port is not a number from 1 to 65535.
    InvalidPort(String),
}

impl fmt::Display for DomainRuleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("domain rule must name a host"),
            Self::InvalidCharacter(character) => {
                write!(
                    formatter,
                    "domain rule contains invalid character {character:?}"
                )
            }
            Self::MisplacedWildcard => {
                formatter.write_str("domain rule may use `*` only as the first label")
            }
            Self::InvalidPort(port) => write!(formatter, "domain rule has invalid port {port:?}"),
        }
    }
}

impl std::error::Error for DomainRuleError {}

/// A host the sandbox may reach, such as `github.com`, `*.npmjs.org`, or `pypi.org:443`.
///
/// A leading `*.` matches any subdomain but not the bare domain. Without a
/// port the rule matches every port.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DomainRule {
    host: String,
    wildcard: bool,
    port: Option<u16>,
}

impl DomainRule {
    /// True when the rule grants access to `host` on `port`.
    #[must_use]
    pub fn matches(&self, host: &str, port: u16) -> bool {
        if self.port.is_some_and(|allowed| allowed != port) {
            return false;
        }
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        if self.wildcard {
            host.strip_suffix(self.host.as_str())
                .is_some_and(|prefix| prefix.ends_with('.') && prefix.len() > 1)
        } else {
            host == self.host
        }
    }
}

impl FromStr for DomainRule {
    type Err = DomainRuleError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (host, port) = match value.rsplit_once(':') {
            Some((host, port)) => {
                let port = port
                    .parse::<u16>()
                    .ok()
                    .filter(|port| *port != 0)
                    .ok_or_else(|| DomainRuleError::InvalidPort(port.to_owned()))?;
                (host, Some(port))
            }
            None => (value, None),
        };
        let (host, wildcard) = match host.strip_prefix("*.") {
            Some(host) => (host, true),
            None => (host, false),
        };
        // A trailing dot names the same host, and `matches` compares without it.
        let host = host.trim_end_matches('.');
        if host.is_empty() {
            return Err(DomainRuleError::Empty);
        }
        if host.contains('*') {
            return Err(DomainRuleError::MisplacedWildcard);
        }
        if let Some(character) = host
            .chars()
            .find(|character| !character.is_ascii_alphanumeric() && !"-.".contains(*character))
        {
            return Err(DomainRuleError::InvalidCharacter(character));
        }

        Ok(Self {
            host: host.to_ascii_lowercase(),
            wildcard,
            port,
        })
    }
}

impl fmt::Display for DomainRule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.wildcard {
            formatter.write_str("*.")?;
        }
        formatter.write_str(&self.host)?;
        if let Some(port) = self.port {
            write!(formatter, ":{port}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{DomainRule, DomainRuleError};

    #[test]
    fn matches_an_exact_host_on_any_port() {
        let sut = "GitHub.com".parse::<DomainRule>().unwrap();

        assert!(sut.matches("github.com", 443));
        assert!(sut.matches("github.com.", 80));
        assert!(!sut.matches("api.github.com", 443));
    }

    #[test]
    fn matches_a_host_whatever_its_case() {
        let sut = "github.com".parse::<DomainRule>().unwrap();

        assert!(sut.matches("GitHub.COM", 443));
        assert!(
            "*.npmjs.org"
                .parse::<DomainRule>()
                .unwrap()
                .matches("Registry.NPMJS.org", 443)
        );
    }

    #[test]
    fn matches_a_host_the_rule_names_with_a_trailing_dot() {
        let sut = "github.com.".parse::<DomainRule>().unwrap();

        assert!(sut.matches("github.com", 443));
        assert_eq!(sut.to_string(), "github.com");
    }

    #[test]
    fn matches_subdomains_of_a_wildcard_but_not_the_apex() {
        let sut = "*.npmjs.org".parse::<DomainRule>().unwrap();

        assert!(sut.matches("registry.npmjs.org", 443));
        assert!(sut.matches("a.b.npmjs.org", 443));
        assert!(!sut.matches("npmjs.org", 443));
        assert!(!sut.matches("evilnpmjs.org", 443));
    }

    #[test]
    fn refuses_a_wildcard_host_without_a_subdomain_label() {
        let sut = "*.npmjs.org".parse::<DomainRule>().unwrap();

        assert!(!sut.matches(".npmjs.org", 443));
        assert!(!sut.matches("npmjs.org.", 443));
    }

    #[test]
    fn keeps_every_rule_form_across_display_and_parsing() {
        for text in [
            "github.com",
            "*.npmjs.org",
            "pypi.org:443",
            "*.pkg.github.com:8080",
        ] {
            let sut = text.parse::<DomainRule>().unwrap();

            assert_eq!(sut.to_string(), text);
            assert_eq!(sut.to_string().parse(), Ok(sut));
        }
    }

    #[test]
    fn restricts_the_port_when_given() {
        let sut = "pypi.org:443".parse::<DomainRule>().unwrap();

        assert!(sut.matches("pypi.org", 443));
        assert!(!sut.matches("pypi.org", 80));
        assert_eq!(sut.to_string(), "pypi.org:443");
    }

    #[test]
    fn rejects_malformed_rules() {
        assert_eq!("".parse::<DomainRule>(), Err(DomainRuleError::Empty));
        assert_eq!("*.".parse::<DomainRule>(), Err(DomainRuleError::Empty));
        assert_eq!(".".parse::<DomainRule>(), Err(DomainRuleError::Empty));
        assert_eq!(
            "*".parse::<DomainRule>(),
            Err(DomainRuleError::MisplacedWildcard)
        );
        assert_eq!(
            "*.*".parse::<DomainRule>(),
            Err(DomainRuleError::MisplacedWildcard)
        );
        assert_eq!(
            "api.*.com".parse::<DomainRule>(),
            Err(DomainRuleError::MisplacedWildcard)
        );
        assert_eq!(
            "bad host".parse::<DomainRule>(),
            Err(DomainRuleError::InvalidCharacter(' '))
        );
        assert_eq!(
            "host:0".parse::<DomainRule>(),
            Err(DomainRuleError::InvalidPort("0".into()))
        );
        assert_eq!(
            "host:http".parse::<DomainRule>(),
            Err(DomainRuleError::InvalidPort("http".into()))
        );
    }
}
