use std::fmt;
use std::str::FromStr;

/// Reports an unknown group name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GroupError {
    /// The name does not match a domain group.
    UnknownDomainGroup(String),
    /// The name does not match an executable group.
    UnknownExecutableGroup(String),
}

impl fmt::Display for GroupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownDomainGroup(name) => write!(
                formatter,
                "unknown domain group {name:?}; expected one of {}",
                DomainGroup::ALL
                    .iter()
                    .map(|group| group.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::UnknownExecutableGroup(name) => write!(
                formatter,
                "unknown executable group {name:?}; expected one of {}",
                ExecutableGroup::ALL
                    .iter()
                    .map(|group| group.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

impl std::error::Error for GroupError {}

/// A named set of hosts a task may need.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DomainGroup {
    /// Anthropic API and account hosts.
    Anthropic,
    /// `OpenAI` API and account hosts.
    OpenAi,
    /// Google AI Studio hosts.
    Google,
    /// `OpenRouter` API hosts.
    OpenRouter,
    /// Amazon Bedrock runtime hosts.
    Bedrock,
    /// Google Vertex AI hosts.
    Vertex,
    /// Claude Code updates, plugins, and documentation.
    ClaudeOptional,
    /// Claude Code operational telemetry.
    ClaudeTelemetry,
    /// GitHub web, API, and content hosts.
    Github,
    /// npm registry.
    Npm,
    /// Python package index.
    Pypi,
    /// Rust crate registry and toolchain downloads.
    Crates,
    /// Go module proxy and checksum database.
    Go,
    /// Homebrew formulae and bottles.
    Homebrew,
    /// Docker Hub registry.
    DockerHub,
}

impl DomainGroup {
    /// Every domain group, in `FromStr` name order.
    pub const ALL: [Self; 15] = [
        Self::Anthropic,
        Self::OpenAi,
        Self::Google,
        Self::OpenRouter,
        Self::Bedrock,
        Self::Vertex,
        Self::ClaudeOptional,
        Self::ClaudeTelemetry,
        Self::Github,
        Self::Npm,
        Self::Pypi,
        Self::Crates,
        Self::Go,
        Self::Homebrew,
        Self::DockerHub,
    ];

    /// The name a manifest uses for the group.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::Google => "google",
            Self::OpenRouter => "openrouter",
            Self::Bedrock => "bedrock",
            Self::Vertex => "vertex",
            Self::ClaudeOptional => "claude-optional",
            Self::ClaudeTelemetry => "claude-telemetry",
            Self::Github => "github",
            Self::Npm => "npm",
            Self::Pypi => "pypi",
            Self::Crates => "crates",
            Self::Go => "go",
            Self::Homebrew => "homebrew",
            Self::DockerHub => "docker-hub",
        }
    }

    /// Host rules in the group, in `DomainRule` syntax.
    #[must_use]
    pub fn domains(self) -> &'static [&'static str] {
        match self {
            Self::Anthropic => &["api.anthropic.com", "platform.claude.com", "claude.ai"],
            Self::OpenAi => &["api.openai.com", "chatgpt.com", "auth.openai.com"],
            Self::Google => &["generativelanguage.googleapis.com", "oauth2.googleapis.com"],
            Self::OpenRouter => &["openrouter.ai"],
            Self::Bedrock => &["*.amazonaws.com"],
            Self::Vertex => &["*.aiplatform.googleapis.com", "oauth2.googleapis.com"],
            Self::ClaudeOptional => &[
                "downloads.claude.ai",
                "code.claude.com",
                "raw.githubusercontent.com",
                "registry.npmjs.org",
                "storage.googleapis.com",
            ],
            Self::ClaudeTelemetry => &[
                "http-intake.logs.us5.datadoghq.com",
                "browser-intake-us5-datadoghq.com",
            ],
            Self::Github => &[
                "github.com",
                "api.github.com",
                "codeload.github.com",
                "*.githubusercontent.com",
            ],
            Self::Npm => &["registry.npmjs.org"],
            Self::Pypi => &["pypi.org", "files.pythonhosted.org"],
            Self::Crates => &[
                "crates.io",
                "index.crates.io",
                "static.crates.io",
                "static.rust-lang.org",
            ],
            Self::Go => &[
                "proxy.golang.org",
                "sum.golang.org",
                "storage.googleapis.com",
            ],
            Self::Homebrew => &["formulae.brew.sh", "ghcr.io", "*.pkg.github.com"],
            Self::DockerHub => &[
                "registry-1.docker.io",
                "auth.docker.io",
                "index.docker.io",
                "production.cloudflare.docker.com",
            ],
        }
    }
}

impl FromStr for DomainGroup {
    type Err = GroupError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|group| group.name() == value)
            .ok_or_else(|| GroupError::UnknownDomainGroup(value.to_owned()))
    }
}

impl fmt::Display for DomainGroup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// A named set of programs a task may run.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ExecutableGroup {
    /// Basic file and shell utilities.
    Coreutils,
    /// Text search and processing tools.
    Text,
    /// Git and its helpers.
    Git,
    /// Network clients.
    Net,
    /// Node.js toolchain.
    Node,
    /// Python toolchain.
    Python,
    /// Rust toolchain.
    Rust,
    /// Go toolchain.
    Go,
}

impl ExecutableGroup {
    /// Every executable group, in `FromStr` name order.
    pub const ALL: [Self; 8] = [
        Self::Coreutils,
        Self::Text,
        Self::Git,
        Self::Net,
        Self::Node,
        Self::Python,
        Self::Rust,
        Self::Go,
    ];

    /// The name a manifest uses for the group.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Coreutils => "coreutils",
            Self::Text => "text",
            Self::Git => "git",
            Self::Net => "net",
            Self::Node => "node",
            Self::Python => "python",
            Self::Rust => "rust",
            Self::Go => "go",
        }
    }

    /// Program names in the group, looked up on `PATH` at launch.
    #[must_use]
    pub fn programs(self) -> &'static [&'static str] {
        match self {
            Self::Coreutils => &[
                "ls", "cat", "cp", "mv", "rm", "mkdir", "rmdir", "touch", "chmod", "ln", "head",
                "tail", "wc", "sort", "uniq", "cut", "tr", "tee", "echo", "printf", "pwd", "true",
                "false", "test", "[", "dirname", "basename", "realpath", "readlink", "stat",
                "date", "sleep", "xargs", "env", "id", "uname", "which", "mktemp", "du", "df",
                "tar", "gzip", "gunzip", "zip", "unzip",
            ],
            Self::Text => &[
                "grep", "egrep", "fgrep", "rg", "sed", "awk", "find", "diff", "patch", "jq",
                "less", "file", "tree",
            ],
            Self::Git => &["git", "ssh"],
            Self::Net => &["curl", "wget", "nc", "ssh"],
            Self::Node => &["node", "npm", "npx", "pnpm", "bun", "yarn", "corepack"],
            Self::Python => &["python", "python3", "pip", "pip3", "uv", "uvx", "poetry"],
            Self::Rust => &[
                "cargo",
                "rustc",
                "rustup",
                "rustfmt",
                "cargo-clippy",
                "clippy-driver",
                "rust-analyzer",
                "cc",
                "c++",
                "ld",
                "clang",
                "gcc",
                "ar",
            ],
            Self::Go => &["go", "gofmt", "cc", "gcc"],
        }
    }

    /// Directories the group's programs need to write, in `SandboxPath` syntax.
    #[must_use]
    pub fn state_paths(self) -> &'static [&'static str] {
        match self {
            Self::Node => &["~/.npm", "~/.cache", "~/.bun", "~/.local/share/pnpm"],
            Self::Python => &["~/.cache"],
            Self::Rust => &["~/.cargo", "~/.rustup"],
            Self::Go => &["~/go", "~/.cache"],
            Self::Coreutils | Self::Text | Self::Git | Self::Net => &[],
        }
    }
}

impl FromStr for ExecutableGroup {
    type Err = GroupError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|group| group.name() == value)
            .ok_or_else(|| GroupError::UnknownExecutableGroup(value.to_owned()))
    }
}

impl fmt::Display for ExecutableGroup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::{DomainGroup, ExecutableGroup, GroupError};
    use crate::{DomainRule, SandboxPath};

    #[test]
    fn parses_every_group_name() {
        for group in DomainGroup::ALL {
            assert_eq!(group.name().parse::<DomainGroup>(), Ok(group));
        }
        for group in ExecutableGroup::ALL {
            assert_eq!(group.name().parse::<ExecutableGroup>(), Ok(group));
        }
    }

    #[test]
    fn reports_unknown_groups_with_the_known_names() {
        let error = "nope".parse::<DomainGroup>().unwrap_err();
        assert_eq!(error, GroupError::UnknownDomainGroup("nope".into()));
        assert!(error.to_string().contains("anthropic, openai"));

        let error = "nope".parse::<ExecutableGroup>().unwrap_err();
        assert_eq!(error, GroupError::UnknownExecutableGroup("nope".into()));
        assert!(error.to_string().contains("coreutils, text"));
    }

    #[test]
    fn every_group_domain_is_a_valid_rule() {
        for group in DomainGroup::ALL {
            for domain in group.domains() {
                assert!(domain.parse::<DomainRule>().is_ok(), "{group}: {domain}");
            }
        }
    }

    /// A caller drops a state path that fails to parse, which makes it read-only.
    #[test]
    fn every_group_state_path_is_a_valid_sandbox_path() {
        for group in ExecutableGroup::ALL {
            for path in group.state_paths() {
                assert!(path.parse::<SandboxPath>().is_ok(), "{group}: {path}");
            }
        }
    }

    #[test]
    fn every_group_names_at_least_one_program() {
        for group in ExecutableGroup::ALL {
            assert!(!group.programs().is_empty(), "{group}");
        }
    }
}
