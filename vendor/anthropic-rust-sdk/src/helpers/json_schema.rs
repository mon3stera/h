//! JSON Schema 辅助，对齐上游 `src/helpers/json-schema.ts`。

use serde_json::{json, Value};

/// 构建 JSON Schema 结构化输出格式配置。
pub fn json_schema_output_format(name: &str, schema: Value, strict: bool) -> Value {
    json!({
        "type": "json_schema",
        "schema": schema,
        "name": name,
        "strict": strict,
    })
}

/// 将 output_config.format 包装为消息请求字段。
pub fn output_config_with_format(format: Value) -> Value {
    json!({ "format": format })
}
