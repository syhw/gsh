# TODO

## Code Cleanup

### Dead Code Removal
The codebase has several unused items marked with `#[allow(dead_code)]`. These should be reviewed and either removed or actually used:

**Provider modules** (`gsh-daemon/src/provider/*.rs`):
- `get_model_capabilities()` functions - defined but never called. Either wire these up to the `Provider::capabilities()` trait method or remove them.
- Response structs (e.g., `AnthropicResponse`, `OpenAIResponse`) - these ARE used by serde for deserialization, the `#[allow(dead_code)]` is correct here.
- `OpenAIErrorResponse::into_provider_error()` - implement proper error handling or remove.

**Tmux module** (`gsh-daemon/src/tmux/mod.rs`):
- `BasicTmuxManager` - legacy struct superseded by `TmuxManager`. Remove if not needed for backward compatibility.

**Flow module**:
- `NodeResult` struct - designed for future use but not yet integrated
- `FlowEngine::set_node_provider/set_node_model` - runtime override methods not yet used

**Session module**:
- `Session::add_system_message`, `update_cwd`, `message_count` - API methods not yet called

**Provider trait methods**:
- Several `Provider` trait methods have default implementations but aren't called (`capabilities`, `model_info`, `chat`, `estimate_tokens`, etc.). These are part of the public API surface - decide if they should be required or removed.

### Principle
Follow YAGNI (You Aren't Gonna Need It) - remove unused code and add it back when actually needed. The git history preserves everything.
