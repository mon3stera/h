use std::collections::BTreeSet;

use serde_json::{Map, Value};

pub fn sanitize(schema: &Value) -> anyhow::Result<Value> {
    let mut schema = schema.clone();
    let definitions = schema
        .as_object()
        .and_then(|object| object.get("$defs").or_else(|| object.get("definitions")))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    sanitize_node(&mut schema, &definitions)?;

    if let Value::Object(object) = &mut schema {
        object.remove("$defs");
        object.remove("definitions");
    }

    Ok(schema)
}

fn sanitize_node(schema: &mut Value, definitions: &Map<String, Value>) -> anyhow::Result<()> {
    let Value::Object(object) = schema else {
        anyhow::bail!("tool schema nodes must be JSON objects")
    };

    resolve_reference(object, definitions)?;

    if let Some(Value::Object(properties)) = object.get_mut("properties") {
        for property in properties.values_mut() {
            sanitize_node(property, definitions)?;
        }
    }

    if let Some(items) = object.get_mut("items") {
        sanitize_node(items, definitions)?;
    }

    if let Some(Value::Array(items)) = object.get_mut("prefixItems") {
        for item in items {
            sanitize_node(item, definitions)?;
        }
    }

    if let Some(additional) = object.get_mut("additionalProperties")
        && additional.is_object()
    {
        sanitize_node(additional, definitions)?;
    }

    for keyword in ["oneOf", "anyOf", "allOf"] {
        if let Some(Value::Array(branches)) = object.get_mut(keyword) {
            for branch in branches {
                sanitize_node(branch, definitions)?;
            }
        }
    }

    infer_type(object);
    Ok(())
}

fn resolve_reference(
    object: &mut Map<String, Value>,
    definitions: &Map<String, Value>,
) -> anyhow::Result<()> {
    let Some(reference) = object
        .get("$ref")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return Ok(());
    };
    let Some(name) = reference
        .strip_prefix("#/$defs/")
        .or_else(|| reference.strip_prefix("#/definitions/"))
    else {
        return Ok(());
    };
    let Some(definition) = definitions.get(name) else {
        anyhow::bail!("tool schema reference {reference:?} was not found")
    };
    let annotations = std::mem::take(object);
    let mut resolved = definition.clone();
    sanitize_node(&mut resolved, definitions)?;
    let Value::Object(resolved) = resolved else {
        anyhow::bail!("tool schema reference {reference:?} did not resolve to an object")
    };

    *object = resolved;
    for (keyword, value) in annotations {
        if keyword != "$ref" {
            object.insert(keyword, value);
        }
    }

    Ok(())
}

fn infer_type(schema: &mut Map<String, Value>) {
    if schema.contains_key("type") || schema.contains_key("$ref") {
        return;
    }

    let inferred =
        if schema.contains_key("properties") || schema.contains_key("additionalProperties") {
            Some(Value::String("object".to_owned()))
        } else if schema.contains_key("items") || schema.contains_key("prefixItems") {
            Some(Value::String("array".to_owned()))
        } else if let Some(value) = schema.get("const") {
            scalar_type(value).map(|kind| Value::String(kind.to_owned()))
        } else if let Some(Value::Array(values)) = schema.get("enum") {
            enum_type(values)
        } else if [
            "minimum",
            "maximum",
            "exclusiveMinimum",
            "exclusiveMaximum",
            "multipleOf",
        ]
        .iter()
        .any(|keyword| schema.contains_key(*keyword))
        {
            Some(Value::String("number".to_owned()))
        } else if ["minLength", "maxLength", "pattern", "format"]
            .iter()
            .any(|keyword| schema.contains_key(*keyword))
        {
            Some(Value::String("string".to_owned()))
        } else if ["minItems", "maxItems", "uniqueItems", "contains"]
            .iter()
            .any(|keyword| schema.contains_key(*keyword))
        {
            Some(Value::String("array".to_owned()))
        } else {
            union_type(schema).or_else(|| Some(Value::String("object".to_owned())))
        };

    if let Some(inferred) = inferred {
        schema.insert("type".to_owned(), inferred);
    }
}

fn scalar_type(value: &Value) -> Option<&'static str> {
    match value {
        Value::Null => Some("null"),
        Value::Bool(_) => Some("boolean"),
        Value::Number(number) if number.is_i64() || number.is_u64() => Some("integer"),
        Value::Number(_) => Some("number"),
        Value::String(_) => Some("string"),
        Value::Array(_) => Some("array"),
        Value::Object(_) => Some("object"),
    }
}

fn enum_type(values: &[Value]) -> Option<Value> {
    let kinds = values
        .iter()
        .filter_map(scalar_type)
        .collect::<BTreeSet<_>>();

    match kinds.len() {
        0 => None,
        1 => kinds.first().map(|kind| Value::String((*kind).to_owned())),
        _ => Some(Value::Array(
            kinds
                .into_iter()
                .map(|kind| Value::String(kind.to_owned()))
                .collect(),
        )),
    }
}

fn union_type(schema: &Map<String, Value>) -> Option<Value> {
    for keyword in ["oneOf", "anyOf", "allOf"] {
        let Some(Value::Array(branches)) = schema.get(keyword) else {
            continue;
        };
        let kinds = branches
            .iter()
            .filter_map(|branch| branch.get("type"))
            .cloned()
            .collect::<Vec<_>>();

        if kinds.len() == branches.len() && kinds.windows(2).all(|pair| pair[0] == pair[1]) {
            return kinds.into_iter().next();
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn fills_missing_types_and_resolves_local_references() {
        let schema = json!({
            "type": "object",
            "properties": {
                "item": {
                    "description": "An opaque call hierarchy item."
                },
                "options": {
                    "$ref": "#/$defs/Options"
                }
            },
            "$defs": {
                "Options": {
                    "properties": {
                        "exact": { "type": "boolean" }
                    }
                }
            }
        });

        let sanitized = sanitize(&schema).unwrap();

        assert_eq!(sanitized["properties"]["item"]["type"], "object");
        assert_eq!(sanitized["properties"]["options"]["type"], "object");
        assert!(sanitized.get("$defs").is_none());
    }

    #[test]
    fn infers_enum_and_union_types_without_lowering_them() {
        let schema = json!({
            "type": "object",
            "properties": {
                "mode": { "enum": ["fast", "safe"] },
                "value": {
                    "oneOf": [
                        { "type": "string" },
                        { "type": "string" }
                    ]
                }
            }
        });

        let sanitized = sanitize(&schema).unwrap();

        assert_eq!(sanitized["properties"]["mode"]["type"], "string");
        assert_eq!(sanitized["properties"]["value"]["type"], "string");
        assert!(sanitized["properties"]["value"].get("oneOf").is_some());
    }
}
