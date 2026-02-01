use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub llm: LlmConfig,
    #[serde(default)]
    pub context: ContextConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    /// Socket path (default: /tmp/gsh-$USER.sock)
    pub socket_path: Option<PathBuf>,
    /// Log level (trace, debug, info, warn, error)
    #[serde(default = "default_log_level")]
    pub log_level: String,
    /// Log file path
    pub log_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    /// Default provider (anthropic, openai, moonshot, ollama)
    #[serde(default = "default_provider")]
    pub default_provider: String,
    #[serde(default)]
    pub anthropic: AnthropicConfig,
    #[serde(default)]
    pub openai: OpenAIConfig,
    #[serde(default)]
    pub moonshot: MoonshotConfig,
    #[serde(default)]
    pub ollama: OllamaConfig,
    /// Maximum tokens in response
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// System prompt prefix
    pub system_prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AnthropicConfig {
    /// API key (can also use ANTHROPIC_API_KEY env var)
    pub api_key: Option<String>,
    /// Model to use
    #[serde(default = "default_anthropic_model")]
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OpenAIConfig {
    /// API key (can also use OPENAI_API_KEY env var)
    pub api_key: Option<String>,
    /// Model to use
    #[serde(default = "default_openai_model")]
    pub model: String,
    /// Base URL (for OpenAI-compatible APIs)
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MoonshotConfig {
    /// API key (can also use MOONSHOT_API_KEY env var)
    pub api_key: Option<String>,
    /// Model to use
    #[serde(default = "default_moonshot_model")]
    pub model: String,
    /// Base URL
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaConfig {
    /// Model to use
    #[serde(default = "default_ollama_model")]
    pub model: String,
    /// Base URL (default: http://localhost:11434)
    pub base_url: Option<String>,
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self {
            model: default_ollama_model(),
            base_url: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextConfig {
    /// Maximum number of shell events to keep
    #[serde(default = "default_max_events")]
    pub max_events: usize,
    /// Maximum context length in characters
    #[serde(default = "default_max_context_chars")]
    pub max_context_chars: usize,
    /// Include command outputs in context
    #[serde(default = "default_true")]
    pub include_outputs: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsConfig {
    /// Enable bash tool
    #[serde(default = "default_true")]
    pub bash_enabled: bool,
    /// Enable file read tool
    #[serde(default = "default_true")]
    pub read_enabled: bool,
    /// Enable file write tool
    #[serde(default = "default_true")]
    pub write_enabled: bool,
    /// Enable file edit tool
    #[serde(default = "default_true")]
    pub edit_enabled: bool,
    /// Enable glob tool
    #[serde(default = "default_true")]
    pub glob_enabled: bool,
    /// Enable grep tool
    #[serde(default = "default_true")]
    pub grep_enabled: bool,
    /// Paths to exclude from file operations
    #[serde(default)]
    pub excluded_paths: Vec<String>,
    /// Maximum file size to read (bytes)
    #[serde(default = "default_max_file_size")]
    pub max_file_size: usize,
}

// Default value functions
fn default_log_level() -> String {
    "info".to_string()
}

fn default_provider() -> String {
    "anthropic".to_string()
}

fn default_max_tokens() -> u32 {
    4096
}

fn default_anthropic_model() -> String {
    "claude-sonnet-4-20250514".to_string()
}

fn default_openai_model() -> String {
    "gpt-4o".to_string()
}

fn default_moonshot_model() -> String {
    "moonshot-v1-128k".to_string()
}

fn default_ollama_model() -> String {
    "llama3.2".to_string()
}

fn default_max_events() -> usize {
    100
}

fn default_max_context_chars() -> usize {
    50000
}

fn default_true() -> bool {
    true
}

fn default_max_file_size() -> usize {
    1024 * 1024 // 1MB
}

impl Default for Config {
    fn default() -> Self {
        Self {
            daemon: DaemonConfig::default(),
            llm: LlmConfig::default(),
            context: ContextConfig::default(),
            tools: ToolsConfig::default(),
        }
    }
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: None,
            log_level: default_log_level(),
            log_file: None,
        }
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            default_provider: default_provider(),
            anthropic: AnthropicConfig::default(),
            openai: OpenAIConfig::default(),
            moonshot: MoonshotConfig::default(),
            ollama: OllamaConfig::default(),
            max_tokens: default_max_tokens(),
            system_prompt: None,
        }
    }
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            max_events: default_max_events(),
            max_context_chars: default_max_context_chars(),
            include_outputs: default_true(),
        }
    }
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            bash_enabled: default_true(),
            read_enabled: default_true(),
            write_enabled: default_true(),
            edit_enabled: default_true(),
            glob_enabled: default_true(),
            grep_enabled: default_true(),
            excluded_paths: vec![],
            max_file_size: default_max_file_size(),
        }
    }
}

impl Config {
    /// Load configuration from the default path (~/.config/gsh/config.toml)
    pub fn load() -> Result<Self> {
        let config_path = Self::default_config_path();
        if config_path.exists() {
            Self::load_from(&config_path)
        } else {
            Ok(Self::default())
        }
    }

    /// Load configuration from a specific path
    pub fn load_from(path: &PathBuf) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;
        let config: Config = toml::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {}", path.display()))?;
        Ok(config)
    }

    /// Get the default config file path
    /// Checks ~/.config/gsh/config.toml first (XDG style), then falls back to
    /// platform-specific config dir (~/Library/Application Support on macOS)
    pub fn default_config_path() -> PathBuf {
        // Prefer ~/.config (XDG style) for CLI tools
        if let Some(home) = dirs::home_dir() {
            let xdg_path = home.join(".config").join("gsh").join("config.toml");
            if xdg_path.exists() {
                return xdg_path;
            }
        }

        // Fall back to platform-specific config dir
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("gsh")
            .join("config.toml")
    }

    /// Get the socket path, using default if not configured
    pub fn socket_path(&self) -> PathBuf {
        self.daemon.socket_path.clone().unwrap_or_else(|| {
            let user = std::env::var("USER").unwrap_or_else(|_| "unknown".to_string());
            PathBuf::from(format!("/tmp/gsh-{}.sock", user))
        })
    }

    /// Get API key for Anthropic (config or env var)
    pub fn anthropic_api_key(&self) -> Option<String> {
        self.llm
            .anthropic
            .api_key
            .clone()
            .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
    }

    /// Get API key for OpenAI (config or env var)
    pub fn openai_api_key(&self) -> Option<String> {
        self.llm
            .openai
            .api_key
            .clone()
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
    }

    /// Get API key for Moonshot (config or env var)
    pub fn moonshot_api_key(&self) -> Option<String> {
        self.llm
            .moonshot
            .api_key
            .clone()
            .or_else(|| std::env::var("MOONSHOT_API_KEY").ok())
    }
}
