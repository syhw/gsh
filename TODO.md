# TODO

## Code Cleanup

Dead code removal done (2026-03-14). Remaining items still carrying `#[allow(dead_code)]` are either:
- Enum variants used by serde deserialization (correct to suppress)
- Public API surface (`Provider` trait default methods — `capabilities`, `model_info`, `estimate_tokens`) that ARE called internally
