//! Token usage tracking and cost estimation
//!
//! Tracks token usage across providers and estimates costs based on
//! published pricing. Prices are approximate and may not reflect
//! current pricing - always verify with the provider.

use crate::provider::UsageStats;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Pricing per million tokens (input, output)
#[derive(Debug, Clone, Copy)]
pub struct ModelPricing {
    /// Cost per million input tokens in USD
    pub input_per_million: f64,
    /// Cost per million output tokens in USD
    pub output_per_million: f64,
    /// Cost per million cached input tokens (if applicable)
    pub cache_read_per_million: Option<f64>,
}

impl ModelPricing {
    pub const fn new(input: f64, output: f64) -> Self {
        Self {
            input_per_million: input,
            output_per_million: output,
            cache_read_per_million: None,
        }
    }

    pub const fn with_cache(input: f64, output: f64, cache_read: f64) -> Self {
        Self {
            input_per_million: input,
            output_per_million: output,
            cache_read_per_million: Some(cache_read),
        }
    }

    /// Calculate cost for the given token counts
    pub fn calculate(&self, input_tokens: u64, output_tokens: u64, cache_read_tokens: Option<u64>) -> f64 {
        let input_cost = (input_tokens as f64 / 1_000_000.0) * self.input_per_million;
        let output_cost = (output_tokens as f64 / 1_000_000.0) * self.output_per_million;
        let cache_cost = if let (Some(cache_tokens), Some(cache_price)) = (cache_read_tokens, self.cache_read_per_million) {
            (cache_tokens as f64 / 1_000_000.0) * cache_price
        } else {
            0.0
        };
        input_cost + output_cost + cache_cost
    }
}

/// Get pricing for a model (approximate, verify with provider)
pub fn get_model_pricing(provider: &str, model: &str) -> Option<ModelPricing> {
    let model_lower = model.to_lowercase();

    match provider {
        "anthropic" => {
            // Anthropic pricing as of early 2025 (verify current prices)
            if model_lower.contains("opus") {
                Some(ModelPricing::with_cache(15.0, 75.0, 1.5))
            } else if model_lower.contains("sonnet") {
                Some(ModelPricing::with_cache(3.0, 15.0, 0.3))
            } else if model_lower.contains("haiku") {
                Some(ModelPricing::with_cache(0.25, 1.25, 0.03))
            } else {
                // Default to Sonnet pricing for unknown Claude models
                Some(ModelPricing::with_cache(3.0, 15.0, 0.3))
            }
        }
        "openai" => {
            // OpenAI pricing varies significantly by model
            if model_lower.contains("gpt-4o-mini") {
                Some(ModelPricing::new(0.15, 0.60))
            } else if model_lower.contains("gpt-4o") {
                Some(ModelPricing::new(2.50, 10.0))
            } else if model_lower.contains("gpt-4-turbo") {
                Some(ModelPricing::new(10.0, 30.0))
            } else if model_lower.contains("o3") || model_lower.contains("o4") {
                // Reasoning models - approximate
                Some(ModelPricing::new(15.0, 60.0))
            } else {
                Some(ModelPricing::new(2.50, 10.0))
            }
        }
        "together" => {
            // Together.AI pricing
            if model_lower.contains("kimi") || model_lower.contains("k2") {
                Some(ModelPricing::new(0.30, 0.30))
            } else if model_lower.contains("qwen") {
                Some(ModelPricing::new(0.20, 0.20))
            } else if model_lower.contains("llama") {
                Some(ModelPricing::new(0.20, 0.20))
            } else {
                Some(ModelPricing::new(0.30, 0.30))
            }
        }
        "z" | "zai" | "zhipu" => {
            // Zhipu GLM pricing (approximate, in USD equivalent)
            if model_lower.contains("glm-4") {
                Some(ModelPricing::new(1.0, 1.0))
            } else {
                Some(ModelPricing::new(0.5, 0.5))
            }
        }
        "moonshot" | "kimi" => {
            // Moonshot pricing
            Some(ModelPricing::new(0.50, 0.50))
        }
        "ollama" => {
            // Local models - no cost
            Some(ModelPricing::new(0.0, 0.0))
        }
        _ => None,
    }
}

/// Accumulated usage statistics for a session or agent
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccumulatedUsage {
    /// Total input tokens
    pub total_input_tokens: u64,
    /// Total output tokens
    pub total_output_tokens: u64,
    /// Total cache read tokens
    pub total_cache_read_tokens: u64,
    /// Total cache creation tokens
    pub total_cache_creation_tokens: u64,
    /// Number of requests made
    pub request_count: u64,
    /// Estimated total cost in USD
    pub estimated_cost_usd: f64,
    /// Breakdown by model
    pub by_model: HashMap<String, ModelUsage>,
}

/// Usage for a specific model
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub request_count: u64,
    pub estimated_cost_usd: f64,
}

impl AccumulatedUsage {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add usage from a request
    pub fn add(&mut self, provider: &str, model: &str, usage: &UsageStats) {
        self.total_input_tokens += usage.input_tokens;
        self.total_output_tokens += usage.output_tokens;
        self.total_cache_read_tokens += usage.cache_read_tokens.unwrap_or(0);
        self.total_cache_creation_tokens += usage.cache_creation_tokens.unwrap_or(0);
        self.request_count += 1;

        // Calculate cost for this request
        let cost = if let Some(pricing) = get_model_pricing(provider, model) {
            pricing.calculate(usage.input_tokens, usage.output_tokens, usage.cache_read_tokens)
        } else {
            0.0
        };
        self.estimated_cost_usd += cost;

        // Update per-model breakdown
        let model_key = format!("{}:{}", provider, model);
        let model_usage = self.by_model.entry(model_key).or_default();
        model_usage.input_tokens += usage.input_tokens;
        model_usage.output_tokens += usage.output_tokens;
        model_usage.cache_read_tokens += usage.cache_read_tokens.unwrap_or(0);
        model_usage.request_count += 1;
        model_usage.estimated_cost_usd += cost;
    }

    /// Merge another AccumulatedUsage into this one
    pub fn merge(&mut self, other: &AccumulatedUsage) {
        self.total_input_tokens += other.total_input_tokens;
        self.total_output_tokens += other.total_output_tokens;
        self.total_cache_read_tokens += other.total_cache_read_tokens;
        self.total_cache_creation_tokens += other.total_cache_creation_tokens;
        self.request_count += other.request_count;
        self.estimated_cost_usd += other.estimated_cost_usd;

        for (model, usage) in &other.by_model {
            let entry = self.by_model.entry(model.clone()).or_default();
            entry.input_tokens += usage.input_tokens;
            entry.output_tokens += usage.output_tokens;
            entry.cache_read_tokens += usage.cache_read_tokens;
            entry.request_count += usage.request_count;
            entry.estimated_cost_usd += usage.estimated_cost_usd;
        }
    }

    /// Format as a human-readable summary
    pub fn summary(&self) -> String {
        format!(
            "Tokens: {}in/{}out | Requests: {} | Est. cost: ${:.4}",
            self.total_input_tokens,
            self.total_output_tokens,
            self.request_count,
            self.estimated_cost_usd
        )
    }
}

/// Cost tracker for the entire daemon
#[derive(Debug, Default)]
pub struct CostTracker {
    /// Usage by session ID
    sessions: HashMap<String, AccumulatedUsage>,
    /// Usage by flow name
    flows: HashMap<String, AccumulatedUsage>,
    /// Global accumulated usage
    global: AccumulatedUsage,
}

impl CostTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record usage for a session
    pub fn record(&mut self, session_id: &str, provider: &str, model: &str, usage: &UsageStats) {
        // Update session-specific usage
        let session_usage = self.sessions.entry(session_id.to_string()).or_default();
        session_usage.add(provider, model, usage);

        // Update global usage
        self.global.add(provider, model, usage);
    }

    /// Record usage for a flow (also records to session and global)
    pub fn record_flow(&mut self, flow_name: &str, session_id: &str, provider: &str, model: &str, usage: &UsageStats) {
        // Update flow-specific usage
        let flow_usage = self.flows.entry(flow_name.to_string()).or_default();
        flow_usage.add(provider, model, usage);

        // Also record to session and global
        self.record(session_id, provider, model, usage);
    }

    /// Get usage for a specific session
    pub fn session_usage(&self, session_id: &str) -> Option<&AccumulatedUsage> {
        self.sessions.get(session_id)
    }

    /// Get usage for a specific flow
    pub fn flow_usage(&self, flow_name: &str) -> Option<&AccumulatedUsage> {
        self.flows.get(flow_name)
    }

    /// Get global usage
    pub fn global_usage(&self) -> &AccumulatedUsage {
        &self.global
    }

    /// Get all sessions with their usage
    pub fn all_sessions(&self) -> &HashMap<String, AccumulatedUsage> {
        &self.sessions
    }

    /// Get all flows with their usage
    pub fn all_flows(&self) -> &HashMap<String, AccumulatedUsage> {
        &self.flows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_pricing_calculation() {
        let pricing = ModelPricing::new(3.0, 15.0);
        let cost = pricing.calculate(1_000_000, 500_000, None);
        // 1M input at $3/M = $3, 0.5M output at $15/M = $7.50
        assert!((cost - 10.5).abs() < 0.001);
    }

    #[test]
    fn test_pricing_with_cache() {
        let pricing = ModelPricing::with_cache(3.0, 15.0, 0.3);
        let cost = pricing.calculate(1_000_000, 500_000, Some(200_000));
        // $3 + $7.50 + $0.06 (cache)
        assert!((cost - 10.56).abs() < 0.001);
    }

    #[test]
    fn test_accumulated_usage() {
        let mut usage = AccumulatedUsage::new();

        let stats = UsageStats {
            input_tokens: 1000,
            output_tokens: 500,
            total_tokens: 1500,
            cache_read_tokens: None,
            cache_creation_tokens: None,
        };

        usage.add("anthropic", "claude-sonnet-4", &stats);

        assert_eq!(usage.total_input_tokens, 1000);
        assert_eq!(usage.total_output_tokens, 500);
        assert_eq!(usage.request_count, 1);
        assert!(usage.estimated_cost_usd > 0.0);
    }

    #[test]
    fn test_cost_tracker() {
        let mut tracker = CostTracker::new();

        let stats = UsageStats {
            input_tokens: 1000,
            output_tokens: 500,
            total_tokens: 1500,
            cache_read_tokens: None,
            cache_creation_tokens: None,
        };

        tracker.record("session-1", "anthropic", "claude-sonnet-4", &stats);
        tracker.record("session-1", "anthropic", "claude-sonnet-4", &stats);
        tracker.record("session-2", "openai", "gpt-4o", &stats);

        assert_eq!(tracker.global_usage().request_count, 3);
        assert_eq!(tracker.session_usage("session-1").unwrap().request_count, 2);
        assert_eq!(tracker.session_usage("session-2").unwrap().request_count, 1);
    }

    #[test]
    fn test_flow_cost_tracking() {
        let mut tracker = CostTracker::new();

        let stats = UsageStats {
            input_tokens: 2000,
            output_tokens: 1000,
            total_tokens: 3000,
            cache_read_tokens: None,
            cache_creation_tokens: None,
        };

        tracker.record_flow("code-review", "session-1", "anthropic", "claude-sonnet-4", &stats);
        tracker.record_flow("code-review", "session-1", "anthropic", "claude-sonnet-4", &stats);
        tracker.record_flow("research", "session-2", "openai", "gpt-4o", &stats);

        // Flow-level tracking
        let review_usage = tracker.flow_usage("code-review").unwrap();
        assert_eq!(review_usage.request_count, 2);
        assert_eq!(review_usage.total_input_tokens, 4000);

        let research_usage = tracker.flow_usage("research").unwrap();
        assert_eq!(research_usage.request_count, 1);

        assert!(tracker.flow_usage("nonexistent").is_none());

        // Also recorded at session and global level
        assert_eq!(tracker.global_usage().request_count, 3);
        assert_eq!(tracker.session_usage("session-1").unwrap().request_count, 2);
        assert_eq!(tracker.all_flows().len(), 2);
    }

    #[test]
    fn test_ollama_free() {
        let pricing = get_model_pricing("ollama", "llama3.2").unwrap();
        let cost = pricing.calculate(1_000_000, 1_000_000, None);
        assert_eq!(cost, 0.0);
    }
}
