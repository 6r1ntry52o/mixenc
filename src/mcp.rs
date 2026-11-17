//! `mixenc mcp`: an MCP server (JSON-RPC 2.0 over stdin and stdout, one message per line) with the
//! front gate's tools for VS Code's Copilot and Copilot CLI: `edit` replaces text in a file of a
//! workspace and `create` writes a new one, each in the file's own encoding ([`crate::edit`]). A
//! tool that refuses says why in its result, so that the agent can fix its call.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::edit;
use crate::judge::Encoding;

/// The protocol versions this server speaks, newest first.
const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "Files in a mixenc workspace may be in CP932 (Shift_JIS), UTF-8 or other \
encodings, a file in one encoding or even line by line. Read them right: from their mirror under the \
workspace's mirror/ folder (UTF-8, the same lines and paths as the repository), or with tools that \
decode them right. Change them only with this server's edit and create: they write each file back in \
its own encoding and keep every line you did not change byte for byte. Other edit tools write the \
whole file in one encoding and change bytes you did not touch.";

const EDIT: &str = "Replace text in a file of a mixenc workspace and write the file back in its own \
encoding (CP932, UTF-8, UTF-16, a mix line by line). Only the lines whose text changes are encoded \
again; every other line keeps its bytes, so the diff shows just your change. old_string must be in \
the file exactly once: copy it from the file's mirror (UTF-8, under mirror/) or from a read that \
decoded the file right. Line breaks may be given as \\n. If a character has no form in the file's \
encoding (an emoji in CP932), nothing is written and the result names it.";

const CREATE: &str = "Create a new file in a mixenc workspace. Non-ASCII text is written in the \
encoding the files around it use (most often CP932), so that MSVC reads it right; if that needs a \
person's decision, the result says to ask and to call again with encoding. Line breaks are written \
as given.";

/// Serves until stdin closes.
pub fn serve() -> Result<()> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(Value::Array(batch)) => {
                let replies: Vec<Value> = batch.iter().filter_map(handle).collect();
                (!replies.is_empty()).then_some(Value::Array(replies))
            }
            Ok(message) => handle(&message),
            Err(err) => Some(error(Value::Null, -32700, &format!("parse error: {err}"))),
        };
        if let Some(reply) = reply {
            writeln!(out, "{reply}")?;
            out.flush()?;
        }
    }
    Ok(())
}

/// The reply to one message; none to a notification or a response.
pub fn handle(message: &Value) -> Option<Value> {
    let method = message.get("method").and_then(Value::as_str)?;
    let id = message.get("id")?.clone();
    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
    let result = match method {
        "initialize" => Ok(initialize(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => call(&params),
        _ => Err((-32601, format!("method not found: {method}"))),
    };
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, text)) => error(id, code, &text),
    })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn initialize(params: &Value) -> Value {
    let asked = params["protocolVersion"].as_str().unwrap_or_default();
    let version = PROTOCOL_VERSIONS.iter().find(|v| **v == asked).unwrap_or(&PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "mixenc", "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS,
    })
}

fn tools() -> Value {
    let path = json!({
        "type": "string",
        "description": "The file's absolute path: the original in the repository, or its mirror under mirror/ (the original is written)",
    });
    let encoding = json!({
        "type": "string",
        "enum": ["cp932", "utf8-bom", "utf8"],
        "description": "Only for a file with no non-ASCII text so far, when the tool asks for it",
    });
    json!([
        {
            "name": "edit",
            "title": "Edit a file, keeping its encoding",
            "description": EDIT,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": path,
                    "old_string": { "type": "string", "description": "The text to replace, exactly as in the file and found once" },
                    "new_string": { "type": "string", "description": "The text to put in its place" },
                    "encoding": encoding,
                },
                "required": ["path", "old_string", "new_string"],
                "additionalProperties": false,
            },
        },
        {
            "name": "create",
            "title": "Create a file in the encoding the files around it use",
            "description": CREATE,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": path,
                    "content": { "type": "string", "description": "The file's text" },
                    "encoding": encoding,
                },
                "required": ["path", "content"],
                "additionalProperties": false,
            },
        },
    ])
}

fn call(params: &Value) -> Result<Value, (i64, String)> {
    let name = params["name"].as_str().unwrap_or_default();
    let args = &params["arguments"];
    let outcome = match name {
        "edit" => (|| {
            edit::replace(
                &path_arg(args)?,
                str_arg(args, "old_string")?,
                str_arg(args, "new_string")?,
                encoding_arg(args)?,
            )
        })(),
        "create" => (|| edit::create(&path_arg(args)?, str_arg(args, "content")?, encoding_arg(args)?))(),
        _ => return Err((-32602, format!("unknown tool: {name}"))),
    };
    let (text, is_error) = match outcome {
        Ok(outcome) => (outcome.summary(), false),
        Err(err) => (format!("{err:#}"), true),
    };
    Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }))
}

fn str_arg<'a>(args: &'a Value, name: &str) -> Result<&'a str> {
    args[name].as_str().ok_or_else(|| anyhow!("{name} is missing (a string)"))
}

fn path_arg(args: &Value) -> Result<PathBuf> {
    let path = str_arg(args, "path")?;
    if path.starts_with("file:") {
        return crate::uri::to_path(path).ok_or_else(|| anyhow!("path {path} is not a file URI mixenc reads"));
    }
    Ok(PathBuf::from(path))
}

fn encoding_arg(args: &Value) -> Result<Option<Encoding>> {
    match args.get("encoding").and_then(Value::as_str) {
        None | Some("") => Ok(None),
        Some(name) => name.parse().map(Some).map_err(|e: String| anyhow!(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_initialize_and_lists_the_tools() {
        let reply = handle(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } }))
            .unwrap();
        assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(reply["result"]["serverInfo"]["name"], "mixenc");
        let newer =
            handle(&json!({ "id": 2, "method": "initialize", "params": { "protocolVersion": "2099-01-01" } })).unwrap();
        assert_eq!(newer["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
        assert!(handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).is_none());
        let list = handle(&json!({ "id": "x", "method": "tools/list" })).unwrap();
        let names: Vec<&str> =
            list["result"]["tools"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(names, ["edit", "create"]);
        assert_eq!(handle(&json!({ "id": 3, "method": "nope" })).unwrap()["error"]["code"], -32601);
    }

    #[test]
    fn a_refusal_is_a_tool_result() {
        let reply = handle(&json!({ "id": 1, "method": "tools/call", "params": { "name": "edit", "arguments": { "path": "C:/x.cpp" } } }))
            .unwrap();
        assert_eq!(reply["result"]["isError"], true);
        assert!(reply["result"]["content"][0]["text"].as_str().unwrap().contains("old_string is missing"));
        let unknown = handle(&json!({ "id": 2, "method": "tools/call", "params": { "name": "rm" } })).unwrap();
        assert_eq!(unknown["error"]["code"], -32602);
    }
}
