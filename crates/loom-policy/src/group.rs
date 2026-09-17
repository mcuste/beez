use std::fmt;

use crate::named::{self, name_table, parse_by_name};
use crate::{DomainRule, SandboxPath, parse_valid};

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
            Self::UnknownDomainGroup(name) => {
                formatter.write_str(&named::unknown::<DomainGroup>("domain group", name))
            }
            Self::UnknownExecutableGroup(name) => {
                formatter.write_str(&named::unknown::<ExecutableGroup>("executable group", name))
            }
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
    /// Azure `OpenAI` Service hosts.
    AzureOpenAi,
    /// Mistral API hosts.
    Mistral,
    /// `DeepSeek` API hosts.
    DeepSeek,
    /// `xAI` API hosts.
    Xai,
    /// Groq API hosts.
    Groq,
    /// Ollama model registry.
    Ollama,
    /// Hugging Face models and datasets.
    HuggingFace,
    /// Claude Code updates, plugins, and documentation.
    ClaudeOptional,
    /// Claude Code operational telemetry.
    ClaudeTelemetry,
    /// GitHub web, API, and content hosts.
    Github,
    /// `GitLab` web and API hosts.
    Gitlab,
    /// Bitbucket web and API hosts.
    Bitbucket,
    /// npm registry.
    Npm,
    /// Python package index.
    Pypi,
    /// Rust crate registry and toolchain downloads.
    Crates,
    /// Go module proxy and checksum database.
    Go,
    /// Ruby gem registry.
    Rubygems,
    /// Maven Central and Gradle plugin hosts.
    Maven,
    /// `NuGet` package registry.
    Nuget,
    /// Homebrew formulae and bottles.
    Homebrew,
    /// Docker Hub registry.
    DockerHub,
    /// Container registries other than Docker Hub.
    ContainerRegistry,
    /// `HashiCorp` releases and the Terraform registry.
    Hashicorp,
    /// Playwright browser downloads.
    Playwright,
}

name_table!(pub DomainGroup {
    Anthropic => "anthropic",
    OpenAi => "openai",
    Google => "google",
    OpenRouter => "openrouter",
    Bedrock => "bedrock",
    Vertex => "vertex",
    AzureOpenAi => "azure-openai",
    Mistral => "mistral",
    DeepSeek => "deepseek",
    Xai => "xai",
    Groq => "groq",
    Ollama => "ollama",
    HuggingFace => "huggingface",
    ClaudeOptional => "claude-optional",
    ClaudeTelemetry => "claude-telemetry",
    Github => "github",
    Gitlab => "gitlab",
    Bitbucket => "bitbucket",
    Npm => "npm",
    Pypi => "pypi",
    Crates => "crates",
    Go => "go",
    Rubygems => "rubygems",
    Maven => "maven",
    Nuget => "nuget",
    Homebrew => "homebrew",
    DockerHub => "docker-hub",
    ContainerRegistry => "container-registry",
    Hashicorp => "hashicorp",
    Playwright => "playwright",
});

parse_by_name!(DomainGroup, GroupError::UnknownDomainGroup);

impl DomainGroup {
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
            Self::AzureOpenAi => &["*.openai.azure.com", "login.microsoftonline.com"],
            Self::Mistral => &["api.mistral.ai"],
            Self::DeepSeek => &["api.deepseek.com"],
            Self::Xai => &["api.x.ai"],
            Self::Groq => &["api.groq.com"],
            Self::Ollama => &["registry.ollama.ai", "ollama.com"],
            Self::HuggingFace => &[
                "huggingface.co",
                "cdn-lfs.huggingface.co",
                "cdn-lfs-us-1.huggingface.co",
            ],
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
            Self::Gitlab => &["gitlab.com", "registry.gitlab.com"],
            Self::Bitbucket => &["bitbucket.org", "api.bitbucket.org"],
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
            Self::Rubygems => &["rubygems.org", "index.rubygems.org"],
            Self::Maven => &[
                "repo.maven.apache.org",
                "repo1.maven.org",
                "plugins.gradle.org",
                "services.gradle.org",
            ],
            Self::Nuget => &["api.nuget.org", "www.nuget.org"],
            Self::Homebrew => &["formulae.brew.sh", "ghcr.io", "*.pkg.github.com"],
            Self::DockerHub => &[
                "registry-1.docker.io",
                "auth.docker.io",
                "index.docker.io",
                "production.cloudflare.docker.com",
            ],
            Self::ContainerRegistry => &[
                "ghcr.io",
                "*.pkg.github.com",
                "quay.io",
                "gcr.io",
                "mcr.microsoft.com",
            ],
            Self::Hashicorp => &[
                "releases.hashicorp.com",
                "registry.terraform.io",
                "checkpoint-api.hashicorp.com",
            ],
            Self::Playwright => &[
                "cdn.playwright.dev",
                "playwright.azureedge.net",
                "storage.googleapis.com",
            ],
        }
    }

    /// Host rules in the group. A rule that fails to parse is dropped.
    #[must_use]
    pub fn rules(self) -> Vec<DomainRule> {
        parse_valid(self.domains())
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
    /// JVM toolchain.
    Jvm,
    /// Ruby toolchain.
    Ruby,
    /// .NET toolchain.
    Dotnet,
    /// Zig toolchain.
    Zig,
    /// Build systems.
    Build,
    /// Container and cluster clients.
    Container,
    /// Infrastructure as code tools.
    Iac,
    /// Process inspection tools.
    Process,
}

name_table!(pub ExecutableGroup {
    Coreutils => "coreutils",
    Text => "text",
    Git => "git",
    Net => "net",
    Node => "node",
    Python => "python",
    Rust => "rust",
    Go => "go",
    Jvm => "jvm",
    Ruby => "ruby",
    Dotnet => "dotnet",
    Zig => "zig",
    Build => "build",
    Container => "container",
    Iac => "iac",
    Process => "process",
});

parse_by_name!(ExecutableGroup, GroupError::UnknownExecutableGroup);

impl ExecutableGroup {
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
            Self::Git => &["git", "ssh", "gh", "glab"],
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
            Self::Jvm => &["java", "javac", "jar", "mvn", "gradle"],
            Self::Ruby => &["ruby", "gem", "bundle", "bundler", "rake", "irb"],
            Self::Dotnet => &["dotnet"],
            Self::Zig => &["zig"],
            Self::Build => &["make", "cmake", "ninja", "pkg-config"],
            Self::Container => &["docker", "docker-compose", "podman", "kubectl", "helm"],
            Self::Iac => &[
                "terraform",
                "tofu",
                "terragrunt",
                "ansible",
                "ansible-playbook",
            ],
            Self::Process => &["ps", "kill", "pkill", "pgrep", "lsof", "top"],
        }
    }

    /// Directories the group's programs need to write. A path that fails to
    /// parse is dropped.
    #[must_use]
    pub fn state_sandbox_paths(self) -> Vec<SandboxPath> {
        self.state_group()
            .map(PathGroup::sandbox_paths)
            .unwrap_or_default()
    }

    /// The path group that holds the caches of this toolchain, when it has one.
    fn state_group(self) -> Option<PathGroup> {
        Some(match self {
            Self::Node => PathGroup::NodeState,
            Self::Python => PathGroup::PythonState,
            Self::Rust => PathGroup::RustState,
            Self::Go => PathGroup::GoState,
            Self::Jvm => PathGroup::JvmState,
            Self::Ruby => PathGroup::RubyState,
            Self::Dotnet => PathGroup::DotnetState,
            Self::Zig => PathGroup::ZigState,
            Self::Iac => PathGroup::IacState,
            Self::Coreutils
            | Self::Text
            | Self::Git
            | Self::Net
            | Self::Build
            | Self::Container
            | Self::Process => return None,
        })
    }
}

/// A named set of paths a policy applies.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum PathGroup {
    /// Private keys.
    Keys,
    /// Cloud provider credentials.
    Cloud,
    /// Cluster, registry, and host logins.
    Logins,
    /// Registry and forge tokens.
    Tokens,
    /// Shell history, which often holds a token typed on a command line.
    History,
    /// Keychains and password stores.
    Secrets,
    /// The task's working directory and the temporary directory.
    Workspace,
    /// Git files that run commands or point Git at other code.
    Git,
    /// Pipeline definitions that run on a later push.
    Ci,
    /// Editor and container configuration that runs commands on open.
    Editor,
    /// Files that change how a build tool runs.
    Toolchain,
    /// Harness configuration a run could use to execute code outside the
    /// sandbox later.
    Harness,
    /// The artifacts of the run itself.
    Artifacts,
    /// Claude Code configuration and state.
    ClaudeState,
    /// Codex configuration and state.
    CodexState,
    /// Pi configuration and state.
    PiState,
    /// Oh My Pi configuration and state.
    OmpState,
    /// Node.js caches.
    NodeState,
    /// Python caches.
    PythonState,
    /// Rust toolchain and registry caches.
    RustState,
    /// Go module and build caches.
    GoState,
    /// Maven and Gradle caches.
    JvmState,
    /// Ruby gem and bundler caches.
    RubyState,
    /// .NET package and toolchain caches.
    DotnetState,
    /// Zig caches.
    ZigState,
    /// Terraform plugin cache.
    IacState,
}

name_table!(pub(crate) PathGroup {
    Keys => "keys",
    Cloud => "cloud",
    Logins => "logins",
    Tokens => "tokens",
    History => "history",
    Secrets => "secrets",
    Workspace => "workspace",
    Git => "git",
    Ci => "ci",
    Editor => "editor",
    Toolchain => "toolchain",
    Harness => "harness",
    Artifacts => "artifacts",
    ClaudeState => "claude-state",
    CodexState => "codex-state",
    PiState => "pi-state",
    OmpState => "omp-state",
    NodeState => "node-state",
    PythonState => "python-state",
    RustState => "rust-state",
    GoState => "go-state",
    JvmState => "jvm-state",
    RubyState => "ruby-state",
    DotnetState => "dotnet-state",
    ZigState => "zig-state",
    IacState => "iac-state",
});

impl PathGroup {
    /// Paths in the group, in `SandboxPath` syntax.
    pub(crate) fn paths(self) -> &'static [&'static str] {
        match self {
            Self::Keys => &["~/.ssh", "~/.gnupg"],
            Self::Cloud => &[
                "~/.aws",
                "~/.config/gcloud",
                "~/.azure",
                "~/.oci",
                "~/.databrickscfg",
                "~/.terraform.d/credentials.tfrc.json",
            ],
            Self::Logins => &["~/.kube", "~/.docker", "~/.netrc"],
            Self::Tokens => &[
                "~/.git-credentials",
                "~/.config/gh",
                "~/.config/glab-cli",
                "~/.npmrc",
                "~/.pypirc",
                "~/.cargo/credentials.toml",
                "~/.gem/credentials",
                "~/.gradle/gradle.properties",
            ],
            Self::History => &[
                "~/.bash_history",
                "~/.zsh_history",
                "~/.local/share/fish/fish_history",
            ],
            Self::Secrets => &["~/Library/Keychains", "~/.password-store", "~/.config/op"],
            Self::Workspace => &[".", "/tmp"],
            Self::Git => &[
                ".git/hooks",
                ".git/config",
                ".husky",
                ".pre-commit-config.yaml",
            ],
            Self::Ci => &[".github/workflows", ".gitlab-ci.yml"],
            Self::Editor => &[".vscode", ".idea", ".devcontainer"],
            Self::Toolchain => &[".envrc", ".cargo/config.toml", ".npmrc", ".yarnrc.yml"],
            Self::Harness => &[".claude", ".mcp.json", ".codex", ".agents", ".pi", ".omp"],
            Self::Artifacts => &[".loom"],
            Self::ClaudeState => &["~/.claude", "~/.claude.json"],
            Self::CodexState => &["~/.codex"],
            Self::PiState => &["~/.pi"],
            Self::OmpState => &["~/.omp"],
            Self::NodeState => &["~/.npm", "~/.cache", "~/.bun", "~/.local/share/pnpm"],
            Self::PythonState => &["~/.cache"],
            Self::RustState => &["~/.cargo", "~/.rustup"],
            Self::GoState => &["~/go", "~/.cache"],
            Self::JvmState => &["~/.m2", "~/.gradle"],
            Self::RubyState => &["~/.gem", "~/.bundle"],
            Self::DotnetState => &["~/.nuget", "~/.dotnet"],
            Self::ZigState => &["~/.cache/zig"],
            Self::IacState => &["~/.terraform.d"],
        }
    }

    /// Paths in the group. A path that fails to parse is dropped.
    pub(crate) fn sandbox_paths(self) -> Vec<SandboxPath> {
        parse_valid(self.paths())
    }
}

#[cfg(test)]
mod tests {
    use super::{DomainGroup, ExecutableGroup, GroupError, PathGroup};
    use crate::DomainRule;

    #[test]
    fn parses_every_group_name() {
        for &group in DomainGroup::ALL {
            assert_eq!(group.name().parse::<DomainGroup>(), Ok(group));
        }
        for &group in ExecutableGroup::ALL {
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

    /// `sandbox_paths` drops a path that fails to parse, which removes a rule.
    #[test]
    fn every_group_path_is_a_valid_sandbox_path() {
        for group in PathGroup::ALL {
            assert_eq!(
                group.sandbox_paths().len(),
                group.paths().len(),
                "{group}: {:?}",
                group.paths()
            );
        }
    }

    #[test]
    fn every_group_names_at_least_one_program() {
        for group in ExecutableGroup::ALL {
            assert!(!group.programs().is_empty(), "{group}");
        }
    }
}
