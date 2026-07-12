//! Consistent human and machine-readable CLI output.

use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;

use clap::ValueEnum;
use serde::Serialize;
use serde_json::Value;

static OUTPUT_FORMAT: AtomicU8 = AtomicU8::new(0);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum OutputFormat {
    #[default]
    Human,
    Json,
}

pub fn set_format(format: OutputFormat) {
    OUTPUT_FORMAT.store(format as u8, Ordering::Relaxed);
}

pub fn is_json() -> bool {
    OUTPUT_FORMAT.load(Ordering::Relaxed) == OutputFormat::Json as u8
}

pub fn print<T: Serialize>(value: &T, force_json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let value = serde_json::to_value(value)?;
    if force_json || is_json() {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        let mut rendered = String::new();
        render_value(&value, 0, &mut rendered);
        print!("{rendered}");
    }
    Ok(())
}

fn render_value(value: &Value, indent: usize, output: &mut String) {
    match value {
        Value::Object(object) => {
            if object.is_empty() {
                output.push_str("none\n");
                return;
            }
            for (key, value) in object {
                if value.is_null() {
                    continue;
                }
                push_indent(output, indent);
                output.push_str(&humanize_key(key));
                if scalar(value) {
                    output.push_str(": ");
                    render_scalar(value, output);
                    output.push('\n');
                } else {
                    output.push_str(":\n");
                    render_value(value, indent + 2, output);
                }
            }
        }
        Value::Array(values) => {
            if values.is_empty() {
                push_indent(output, indent);
                output.push_str("none\n");
                return;
            }
            for value in values {
                push_indent(output, indent);
                if scalar(value) {
                    output.push_str("- ");
                    render_scalar(value, output);
                    output.push('\n');
                } else {
                    output.push_str("-\n");
                    render_value(value, indent + 2, output);
                }
            }
        }
        _ => {
            push_indent(output, indent);
            render_scalar(value, output);
            output.push('\n');
        }
    }
}

fn scalar(value: &Value) -> bool {
    matches!(
        value,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)
    )
}

fn render_scalar(value: &Value, output: &mut String) {
    match value {
        Value::Null => output.push_str("none"),
        Value::Bool(true) => output.push_str("yes"),
        Value::Bool(false) => output.push_str("no"),
        Value::Number(number) => output.push_str(&number.to_string()),
        Value::String(string) if string.is_empty() => output.push_str("(empty)"),
        Value::String(string) => output.push_str(string),
        _ => {}
    }
}

fn humanize_key(key: &str) -> String {
    let mut output = String::with_capacity(key.len() + 4);
    let mut previous_lowercase = false;
    for character in key.chars() {
        if character == '_' || character == '-' {
            output.push(' ');
            previous_lowercase = false;
        } else if character.is_uppercase() && previous_lowercase {
            output.push(' ');
            output.extend(character.to_lowercase());
            previous_lowercase = false;
        } else {
            output.push(character);
            previous_lowercase = character.is_lowercase() || character.is_ascii_digit();
        }
    }
    output
}

fn push_indent(output: &mut String, indent: usize) {
    output.extend(std::iter::repeat_n(' ', indent));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_nested_values_as_readable_fields_and_bullets() {
        let mut output = String::new();
        render_value(
            &serde_json::json!({
                "projectName": "Session Skein",
                "recursive": true,
                "ignored": null,
                "roots": [{"path": "/tmp/code", "max_depth": 16}]
            }),
            0,
            &mut output,
        );
        assert!(output.contains("project name: Session Skein\n"));
        assert!(output.contains("recursive: yes\n"));
        assert!(output.contains("roots:\n  -\n"));
        assert!(output.contains("path: /tmp/code\n"));
        assert!(output.contains("max depth: 16\n"));
        assert!(!output.contains("ignored"));
    }
}
