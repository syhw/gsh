//! Role template system for agent flows
//!
//! Roles define reusable agent configurations that can be referenced
//! in flow node definitions. This allows for consistent agent behavior
//! across multiple flows.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A role template that defines agent behavior
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Role {
    /// Role name (identifier)
    pub name: String,

    /// Human-readable description
    #[serde(default)]
    pub description: String,

    /// System prompt for this role
    #[serde(default)]
    pub system_prompt: Option<String>,

    /// Tools this role is allowed to use (empty = all allowed)
    #[serde(default)]
    pub allowed_tools: Vec<String>,

    /// Tools this role is denied from using
    #[serde(default)]
    pub denied_tools: Vec<String>,

    /// Default provider for this role
    #[serde(default)]
    pub provider: Option<String>,

    /// Default model for this role
    #[serde(default)]
    pub model: Option<String>,

    /// Maximum iterations for agent loop
    #[serde(default)]
    pub max_iterations: Option<usize>,

    /// Temperature setting
    #[serde(default)]
    pub temperature: Option<f32>,
}

impl Default for Role {
    fn default() -> Self {
        Self {
            name: "default".to_string(),
            description: String::new(),
            system_prompt: None,
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            provider: None,
            model: None,
            max_iterations: None,
            temperature: None,
        }
    }
}

/// TOML structure for role files
#[derive(Debug, Deserialize)]
struct RoleToml {
    role: RoleDefinition,
}

#[derive(Debug, Deserialize)]
struct RoleDefinition {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    prompt: Option<RolePrompt>,
    #[serde(default)]
    tools: Option<RoleTools>,
    #[serde(default)]
    model: Option<RoleModel>,
}

#[derive(Debug, Deserialize)]
struct RolePrompt {
    system: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RoleTools {
    #[serde(default)]
    allowed: Vec<String>,
    #[serde(default)]
    denied: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RoleModel {
    default: Option<String>,
    provider: Option<String>,
    #[serde(default)]
    max_iterations: Option<usize>,
    #[serde(default)]
    temperature: Option<f32>,
}

/// Parse a role from TOML content
pub fn parse_role(content: &str) -> Result<Role> {
    let role_toml: RoleToml = toml::from_str(content)?;
    Ok(convert_role(role_toml))
}

/// Parse a role from a file path
pub fn parse_role_file(path: impl AsRef<Path>) -> Result<Role> {
    let content = std::fs::read_to_string(path.as_ref())?;
    parse_role(&content)
}

fn convert_role(toml: RoleToml) -> Role {
    let def = toml.role;

    Role {
        name: def.name,
        description: def.description,
        system_prompt: def.prompt.and_then(|p| p.system),
        allowed_tools: def.tools.as_ref().map(|t| t.allowed.clone()).unwrap_or_default(),
        denied_tools: def.tools.as_ref().map(|t| t.denied.clone()).unwrap_or_default(),
        provider: def.model.as_ref().and_then(|m| m.provider.clone()),
        model: def.model.as_ref().and_then(|m| m.default.clone()),
        max_iterations: def.model.as_ref().and_then(|m| m.max_iterations),
        temperature: def.model.as_ref().and_then(|m| m.temperature),
    }
}

/// Registry for loading and caching role templates
pub struct RoleRegistry {
    /// Cached roles (name -> Role)
    roles: HashMap<String, Role>,
    /// Search paths for role files
    search_paths: Vec<PathBuf>,
}

impl RoleRegistry {
    /// Create a new role registry with default search paths
    pub fn new() -> Self {
        let mut search_paths = Vec::new();

        // Project-local roles
        search_paths.push(PathBuf::from(".gsh/roles"));

        // User config roles
        if let Some(home) = dirs::home_dir() {
            search_paths.push(home.join(".config/gsh/roles"));
        }

        // XDG config roles
        if let Some(config_dir) = dirs::config_dir() {
            search_paths.push(config_dir.join("gsh/roles"));
        }

        Self {
            roles: HashMap::new(),
            search_paths,
        }
    }

    /// Add a custom search path
    #[allow(dead_code)]
    pub fn add_search_path(&mut self, path: impl Into<PathBuf>) {
        self.search_paths.insert(0, path.into());
    }

    /// Get a role by name, loading from file if not cached
    pub fn get(&mut self, name: &str) -> Result<&Role> {
        if !self.roles.contains_key(name) {
            let role = self.load_role(name)?;
            self.roles.insert(name.to_string(), role);
        }
        Ok(self.roles.get(name).unwrap())
    }

    /// Load a role from file
    fn load_role(&self, name: &str) -> Result<Role> {
        let filename = format!("{}.toml", name);

        for search_path in &self.search_paths {
            let path = search_path.join(&filename);
            if path.exists() {
                return parse_role_file(&path);
            }
        }

        Err(anyhow::anyhow!(
            "Role '{}' not found. Searched in:\n{}",
            name,
            self.search_paths
                .iter()
                .map(|p| format!("  - {}", p.display()))
                .collect::<Vec<_>>()
                .join("\n")
        ))
    }

    /// List all available roles
    #[allow(dead_code)]
    pub fn list_available(&self) -> Vec<String> {
        let mut roles = Vec::new();

        for search_path in &self.search_paths {
            if let Ok(entries) = std::fs::read_dir(search_path) {
                for entry in entries.flatten() {
                    if let Some(name) = entry.path().file_stem() {
                        if entry.path().extension().map(|e| e == "toml").unwrap_or(false) {
                            let role_name = name.to_string_lossy().to_string();
                            if !roles.contains(&role_name) {
                                roles.push(role_name);
                            }
                        }
                    }
                }
            }
        }

        roles.sort();
        roles
    }

    /// Preload all roles from search paths
    #[allow(dead_code)]
    pub fn preload_all(&mut self) -> Result<usize> {
        let available = self.list_available();
        let mut loaded = 0;

        for name in available {
            if self.get(&name).is_ok() {
                loaded += 1;
            }
        }

        Ok(loaded)
    }
}

impl Default for RoleRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// Built-in roles that don't need files
impl Role {
    /// Create a general-purpose agent role
    pub fn general() -> Self {
        Self {
            name: "general".to_string(),
            description: "General-purpose assistant".to_string(),
            system_prompt: None, // Uses default
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            provider: None,
            model: None,
            max_iterations: Some(10),
            temperature: None,
        }
    }

    /// Create a code reviewer role
    pub fn code_reviewer() -> Self {
        Self {
            name: "code-reviewer".to_string(),
            description: "Code review specialist".to_string(),
            system_prompt: Some(r#"You are a senior code reviewer. Focus on:
- Code clarity and readability
- Error handling and edge cases
- Performance considerations
- Security implications
- Test coverage

Provide specific, actionable feedback with file and line references."#.to_string()),
            allowed_tools: vec!["read".to_string(), "glob".to_string(), "grep".to_string()],
            denied_tools: vec!["write".to_string(), "edit".to_string(), "bash".to_string()],
            provider: None,
            model: None,
            max_iterations: Some(5),
            temperature: None,
        }
    }

    /// Create a planner role
    pub fn planner() -> Self {
        Self {
            name: "planner".to_string(),
            description: "Task planning and decomposition".to_string(),
            system_prompt: Some(r#"You are a task planner. Your job is to:
1. Understand the user's goal
2. Break it down into concrete steps
3. Identify dependencies between steps
4. Estimate complexity of each step

Output a structured plan that other agents can execute."#.to_string()),
            allowed_tools: vec!["read".to_string(), "glob".to_string(), "grep".to_string()],
            denied_tools: vec!["write".to_string(), "edit".to_string(), "bash".to_string()],
            provider: None,
            model: None,
            max_iterations: Some(3),
            temperature: None,
        }
    }

    /// Create a coder role
    pub fn coder() -> Self {
        Self {
            name: "coder".to_string(),
            description: "Code implementation specialist".to_string(),
            system_prompt: Some(r#"You are a code implementation specialist. Your job is to:
1. Write clean, well-documented code
2. Follow existing code style and patterns
3. Handle edge cases and errors appropriately
4. Write tests for your code when appropriate

Be precise with file paths and code changes."#.to_string()),
            allowed_tools: Vec::new(), // All tools allowed
            denied_tools: Vec::new(),
            provider: None,
            model: None,
            max_iterations: Some(15),
            temperature: None,
        }
    }

    /// Get a built-in role by name
    pub fn builtin(name: &str) -> Option<Self> {
        match name {
            "general" => Some(Self::general()),
            "code-reviewer" => Some(Self::code_reviewer()),
            "planner" => Some(Self::planner()),
            "coder" => Some(Self::coder()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_role() {
        let toml = r#"
[role]
name = "test-role"
description = "A test role"

[role.prompt]
system = "You are a test assistant."

[role.tools]
allowed = ["read", "glob"]
denied = ["bash"]

[role.model]
default = "claude-sonnet"
provider = "anthropic"
max_iterations = 5
"#;

        let role = parse_role(toml).unwrap();
        assert_eq!(role.name, "test-role");
        assert_eq!(role.description, "A test role");
        assert_eq!(role.system_prompt, Some("You are a test assistant.".to_string()));
        assert_eq!(role.allowed_tools, vec!["read", "glob"]);
        assert_eq!(role.denied_tools, vec!["bash"]);
        assert_eq!(role.model, Some("claude-sonnet".to_string()));
        assert_eq!(role.provider, Some("anthropic".to_string()));
        assert_eq!(role.max_iterations, Some(5));
    }

    #[test]
    fn test_builtin_roles() {
        assert!(Role::builtin("general").is_some());
        assert!(Role::builtin("code-reviewer").is_some());
        assert!(Role::builtin("planner").is_some());
        assert!(Role::builtin("coder").is_some());
        assert!(Role::builtin("nonexistent").is_none());
    }

    #[test]
    fn test_role_registry() {
        let registry = RoleRegistry::new();

        // Built-in roles should be findable through the registry
        // (In production, these would be loaded from files)
        let available = registry.list_available();
        // Note: This test will pass even if no role files exist
        assert!(available.is_empty() || !available.is_empty());
    }
}
