//! `mixenc mcp`: an MCP server (JSON-RPC 2.0 over stdin and stdout, one message per line) for VS
//! Code's Copilot and Copilot CLI, with two kinds of tools:
//!
//! - the analysis tools ([`crate::analysis`]): clangd's answers about the C++ code, short, with the
//!   originals' paths, and the files read right; they only read;
//! - the front gate: `edit` replaces text in a file of a workspace and `create` writes a new one,
//!   each in the file's own encoding ([`crate::edit`]).
//!
//! A tool that refuses says why in its result, so that the agent can fix its call.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::analysis::Tools;
use crate::edit;
use crate::judge::Encoding;

/// The protocol versions this server speaks, newest first.
const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "Files in a mixenc workspace may be in CP932 (Shift_JIS), UTF-8 or other \
encodings, a file in one encoding or even line by line. Read them right: with read_lines and search \
here, or from their mirror under the workspace's mirror/ folder (UTF-8, the same lines and paths as \
the repository). For the structure of the C++ code, ask clangd through find_symbol, definition, \
references, call_hierarchy, type_hierarchy and outline before searching text. Change files only with \
this server's edit and create: they write each file back in its own encoding and keep every line you \
did not change byte for byte. Other edit tools write the whole file in one encoding and change bytes \
you did not touch.";

const PLACE: &str = "Name the symbol (Foo::Init, or just Init); add path and line (1-based) of where \
its name is written to pick one, as other answers give them. No columns needed.";

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

/// The server's state: the analysis tools' clangd sessions.
#[derive(Default)]
pub struct Server {
    tools: Tools,
}

/// Serves until stdin closes.
pub fn serve() -> Result<()> {
    let mut server = Server::default();
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(Value::Array(batch)) => {
                let replies: Vec<Value> = batch.iter().filter_map(|m| server.handle(m)).collect();
                (!replies.is_empty()).then_some(Value::Array(replies))
            }
            Ok(message) => server.handle(&message),
            Err(err) => Some(error(Value::Null, -32700, &format!("parse error: {err}"))),
        };
        if let Some(reply) = reply {
            writeln!(out, "{reply}")?;
            out.flush()?;
        }
    }
    Ok(())
}

impl Server {
    /// The reply to one message; none to a notification or a response.
    pub fn handle(&mut self, message: &Value) -> Option<Value> {
        let method = message.get("method").and_then(Value::as_str)?;
        let id = message.get("id")?.clone();
        let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
        let result = match method {
            "initialize" => Ok(initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools() })),
            "tools/call" => self.call(&params),
            _ => Err((-32601, format!("method not found: {method}"))),
        };
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, text)) => error(id, code, &text),
        })
    }

    fn call(&mut self, params: &Value) -> Result<Value, (i64, String)> {
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
                .map(|o| o.summary())
            })(),
            "create" => {
                (|| edit::create(&path_arg(args)?, str_arg(args, "content")?, encoding_arg(args)?).map(|o| o.summary()))(
                )
            }
            _ if ANALYSIS.contains(&name) => self.tools.call(name, args),
            _ => return Err((-32602, format!("unknown tool: {name}"))),
        };
        let (text, is_error) = match outcome {
            Ok(text) => (text, false),
            Err(err) => (format!("{err:#}"), true),
        };
        Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }))
    }
}

/// The analysis tools' names.
pub const ANALYSIS: [&str; 9] = [
    "find_symbol",
    "definition",
    "references",
    "call_hierarchy",
    "type_hierarchy",
    "outline",
    "read_lines",
    "search",
    "diagnose",
];

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
    let place = |extra: Value| {
        let mut properties = json!({
            "symbol": { "type": "string", "description": "The symbol's name: Foo::Init, or Init" },
            "path": { "type": "string", "description": "The file where the name is written (absolute, or as answers give it)" },
            "line": { "type": "integer", "minimum": 1, "description": "The line (1-based) where the name is written in that file" },
        });
        if let (Some(p), Some(e)) = (properties.as_object_mut(), extra.as_object()) {
            p.extend(e.clone());
        }
        json!({ "type": "object", "properties": properties, "required": ["symbol"], "additionalProperties": false })
    };
    let read_only = json!({ "readOnlyHint": true, "openWorldHint": false });
    let file = json!({ "type": "string", "description": "The file (absolute, or as answers give it; a mirror path stands for its original)" });
    json!([
        {
            "name": "find_symbol",
            "title": "Find symbols by name",
            "description": "Find classes, functions, variables and macros by name (or part of one, Foo::Init) in clangd's index of the whole workspace. Answers kind, qualified name and path:line. Start here.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "A name, part of one, or a qualified one (Foo::Init)" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 200, "description": "At most this many (30)" },
                },
                "required": ["query"],
                "additionalProperties": false,
            },
            "annotations": read_only,
        },
        {
            "name": "definition",
            "title": "Go to a definition",
            "description": format!("Where a symbol is defined and declared, with the lines around each. {PLACE}"),
            "inputSchema": place(json!({})),
            "annotations": read_only,
        },
        {
            "name": "references",
            "title": "Find references",
            "description": format!("Every place a symbol is used, as a count and the lines per file (no text; read_lines reads what you need). {PLACE}"),
            "inputSchema": place(json!({
                "include_declaration": { "type": "boolean", "description": "Count declarations and the definition too (false)" },
            })),
            "annotations": read_only,
        },
        {
            "name": "call_hierarchy",
            "title": "Who calls a function, or what it calls",
            "description": format!("The tree of a function's callers (incoming) or callees (outgoing), levels deep, with the lines of the calls. {PLACE}"),
            "inputSchema": place(json!({
                "direction": { "type": "string", "enum": ["incoming", "outgoing"], "description": "incoming: who calls it (default); outgoing: what it calls" },
                "depth": { "type": "integer", "minimum": 1, "maximum": 5, "description": "Levels (2)" },
            })),
            "annotations": read_only,
        },
        {
            "name": "type_hierarchy",
            "title": "Base and derived classes",
            "description": format!("A class's bases and derived classes, as trees. {PLACE}"),
            "inputSchema": place(json!({
                "direction": { "type": "string", "enum": ["both", "supertypes", "subtypes"], "description": "both (default), supertypes (bases) or subtypes (derived)" },
                "depth": { "type": "integer", "minimum": 1, "maximum": 6, "description": "Levels (3)" },
            })),
            "annotations": read_only,
        },
        {
            "name": "outline",
            "title": "A file's outline",
            "description": "The classes, functions and other declarations of a file with their lines, nested, without their bodies.",
            "inputSchema": {
                "type": "object",
                "properties": { "path": file },
                "required": ["path"],
                "additionalProperties": false,
            },
            "annotations": read_only,
        },
        {
            "name": "read_lines",
            "title": "Read lines of a file, decoded right",
            "description": "Lines of a file, numbered, read right whatever its encoding (CP932, mixed, UTF-16). Read only the lines you need (at most 400).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": file,
                    "start": { "type": "integer", "minimum": 1, "description": "The first line (1)" },
                    "end": { "type": "integer", "minimum": 1, "description": "The last line (start + 99)" },
                },
                "required": ["path"],
                "additionalProperties": false,
            },
            "annotations": read_only,
        },
        {
            "name": "search",
            "title": "Search text, encodings read right",
            "description": "Search the repositories' text for a regular expression, every file read right (a CP932 file matches 表示 as written). For strings, comments and files clangd does not compile (.rc, settings); for code structure ask the other tools first. Answers path:line: text.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "A regular expression (Rust's regex, as ripgrep)" },
                    "path": { "type": "string", "description": "Only under this folder or file" },
                    "glob": { "type": "string", "description": "Only files matching this glob (*.cpp, src/**/*.h, *.{h,cpp})" },
                    "ignore_case": { "type": "boolean" },
                    "fixed": { "type": "boolean", "description": "The pattern is plain text" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 500, "description": "At most this many lines (50)" },
                },
                "required": ["pattern"],
                "additionalProperties": false,
            },
            "annotations": read_only,
        },
        {
            "name": "diagnose",
            "title": "Whether the analysis can be trusted",
            "description": "The workspace's state (mixenc doctor, short): files not mirrored or read with doubt, the database, clangd's index; with path, that file's encoding, whether it is in compile_commands.json, and clangd's errors in it.",
            "inputSchema": {
                "type": "object",
                "properties": { "path": file },
                "additionalProperties": false,
            },
            "annotations": read_only,
        },
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
        let mut server = Server::default();
        let mut handle = |m: &Value| server.handle(m);
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
        assert_eq!(names[..ANALYSIS.len()], ANALYSIS);
        assert_eq!(names[ANALYSIS.len()..], ["edit", "create"]);
        assert_eq!(handle(&json!({ "id": 3, "method": "nope" })).unwrap()["error"]["code"], -32601);
    }

    #[test]
    fn a_refusal_is_a_tool_result() {
        let mut server = Server::default();
        let mut handle = |m: &Value| server.handle(m);
        let reply = handle(&json!({ "id": 1, "method": "tools/call", "params": { "name": "edit", "arguments": { "path": "C:/x.cpp" } } }))
            .unwrap();
        assert_eq!(reply["result"]["isError"], true);
        assert!(reply["result"]["content"][0]["text"].as_str().unwrap().contains("old_string is missing"));
        let unknown = handle(&json!({ "id": 2, "method": "tools/call", "params": { "name": "rm" } })).unwrap();
        assert_eq!(unknown["error"]["code"], -32602);
    }
}
