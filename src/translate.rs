//! Moving the positions in LSP messages between a misreading client's columns and clangd's.
//!
//! Positions are found by their shape (`{line, character}`, and ranges of two of them) wherever
//! they are, so every request and answer is covered without a list of methods. Each position
//! belongs to a document: the nearest enclosing `uri`, `targetUri` or `textDocument.uri`, else the
//! document the message is about. Exceptions: `originSelectionRange` belongs to the request's
//! document, the `fromRanges` of an incoming call to its `from`, and the keys of a `changes` map
//! are the documents of their edits.

use serde_json::{Map, Value};

use crate::colmap::{FileMap, Way};

/// Moves the positions in `value`. `map` gives the column map of a document (by URI), `None` where
/// its columns need no moving.
pub fn positions(
    value: &mut Value,
    document: Option<&str>,
    way: Way,
    map: &mut dyn FnMut(&str) -> Option<std::sync::Arc<FileMap>>,
) {
    walk(value, document, document, way, map);
}

fn walk(
    value: &mut Value,
    here: Option<&str>,
    request: Option<&str>,
    way: Way,
    map: &mut dyn FnMut(&str) -> Option<std::sync::Arc<FileMap>>,
) {
    match value {
        Value::Array(items) => {
            for item in items {
                walk(item, here, request, way, map);
            }
        }
        Value::Object(object) => {
            if is_range(object) {
                if let Some(m) = here.and_then(&mut *map) {
                    move_position(object.get_mut("start"), &m, way, false);
                    move_position(object.get_mut("end"), &m, way, true);
                }
                return;
            }
            if is_position(object) {
                if let Some(m) = here.and_then(&mut *map) {
                    move_position(Some(value), &m, way, false);
                }
                return;
            }
            let own = uri_of(object);
            let here = own.as_deref().or(here);
            let from = object.get("from").and_then(|f| f.get("uri")).and_then(Value::as_str).map(str::to_string);
            for (key, child) in object.iter_mut() {
                match key.as_str() {
                    "originSelectionRange" => walk(child, request, request, way, map),
                    "fromRanges" if from.is_some() => walk(child, from.as_deref(), request, way, map),
                    "changes" => {
                        if let Value::Object(changes) = child {
                            for (uri, edits) in changes.iter_mut() {
                                walk(edits, Some(uri.as_str()), request, way, map);
                            }
                        }
                    }
                    _ => walk(child, here, request, way, map),
                }
            }
        }
        _ => {}
    }
}

fn uri_of(object: &Map<String, Value>) -> Option<String> {
    object
        .get("uri")
        .or_else(|| object.get("targetUri"))
        .and_then(Value::as_str)
        .or_else(|| object.get("textDocument").and_then(|t| t.get("uri")).and_then(Value::as_str))
        .map(str::to_string)
}

fn is_position(object: &Map<String, Value>) -> bool {
    object.len() == 2
        && object.get("line").is_some_and(Value::is_u64)
        && object.get("character").is_some_and(Value::is_u64)
}

fn is_range(object: &Map<String, Value>) -> bool {
    object.len() == 2
        && ["start", "end"].iter().all(|k| matches!(object.get(*k), Some(Value::Object(p)) if is_position(p)))
}

fn move_position(position: Option<&mut Value>, map: &FileMap, way: Way, end: bool) {
    let Some(Value::Object(p)) = position else { return };
    let (Some(line), Some(character)) =
        (p.get("line").and_then(Value::as_u64), p.get("character").and_then(Value::as_u64))
    else {
        return;
    };
    let moved = map.column(line as u32, character as u32, way, end);
    p.insert("character".into(), Value::from(moved));
}

/// Moves the columns of semantic tokens (`data`: five numbers per token, the line and start
/// relative to the previous token).
pub fn semantic_tokens(data: &mut [Value], map: &FileMap, way: Way) {
    let (mut line, mut start) = (0u64, 0u64);
    let (mut out_line, mut out_start) = (0u64, 0u64);
    for token in data.chunks_mut(5) {
        if token.len() < 5 {
            return;
        }
        let (Some(dl), Some(ds), Some(len)) = (token[0].as_u64(), token[1].as_u64(), token[2].as_u64()) else { return };
        line += dl;
        start = if dl == 0 { start + ds } else { ds };
        let new_start = u64::from(map.column(line as u32, start as u32, way, false));
        let new_end = u64::from(map.column(line as u32, (start + len) as u32, way, true));
        let new_ds = if line == out_line && dl == 0 { new_start - out_start.min(new_start) } else { new_start };
        token[1] = Value::from(new_ds);
        token[2] = Value::from(new_end.saturating_sub(new_start));
        out_line = line;
        out_start = new_start;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::colmap::Units;
    use crate::judge::Encoding;
    use serde_json::json;
    use std::sync::Arc;

    const A: &str = "file:///c%3A/r/a.cpp";
    const B: &str = "file:///c%3A/r/b.h";

    /// Both files: line 0 is `"表"; x` (the client sees `"`, U+FFFD, `\`, `"`).
    fn maps() -> impl FnMut(&str) -> Option<Arc<FileMap>> {
        let m = Arc::new(FileMap::new(b"\"\x95\x5C\"; x", Encoding::Cp932, Units::Utf16).unwrap());
        move |uri: &str| (uri == A || uri == B).then(|| m.clone())
    }

    #[test]
    fn a_request_and_its_answer() {
        let mut map = maps();
        let mut request = json!({"textDocument": {"uri": A}, "position": {"line": 0, "character": 6}});
        positions(&mut request, Some(A), Way::ToMirror, &mut map);
        assert_eq!(request["position"]["character"], 5);

        // A definition in another file, a link with an origin in the request's file.
        let mut answer = json!([
            {"uri": B, "range": {"start": {"line": 0, "character": 5}, "end": {"line": 0, "character": 6}}},
            {"targetUri": "file:///c%3A/other.h",
             "targetRange": {"start": {"line": 0, "character": 5}, "end": {"line": 0, "character": 6}},
             "originSelectionRange": {"start": {"line": 0, "character": 5}, "end": {"line": 0, "character": 6}}}
        ]);
        positions(&mut answer, Some(A), Way::ToClient, &mut map);
        assert_eq!(answer[0]["range"]["start"]["character"], 6);
        assert_eq!(answer[0]["range"]["end"]["character"], 7);
        assert_eq!(answer[1]["targetRange"]["start"]["character"], 5, "other.h has no map");
        assert_eq!(answer[1]["originSelectionRange"]["start"]["character"], 6);
    }

    #[test]
    fn edits_by_document_and_incoming_calls() {
        let mut map = maps();
        let mut edit = json!({"changes": {B: [{"range": {"start": {"line": 0, "character": 5}, "end": {"line": 0, "character": 6}}, "newText": "y"}]}});
        positions(&mut edit, None, Way::ToClient, &mut map);
        assert_eq!(edit["changes"][B][0]["range"]["start"]["character"], 6);

        let mut calls = json!([{"from": {"uri": B, "range": {"start": {"line": 0, "character": 5}, "end": {"line": 0, "character": 6}},
                                        "selectionRange": {"start": {"line": 0, "character": 5}, "end": {"line": 0, "character": 6}}},
                                "fromRanges": [{"start": {"line": 0, "character": 5}, "end": {"line": 0, "character": 6}}]}]);
        positions(&mut calls, Some("file:///c%3A/none.cpp"), Way::ToClient, &mut map);
        assert_eq!(calls[0]["fromRanges"][0]["start"]["character"], 6);
        assert_eq!(calls[0]["from"]["selectionRange"]["start"]["character"], 6);
    }

    #[test]
    fn semantic_tokens_move_their_starts() {
        let m = FileMap::new(b"\"\x95\x5C\"; x", Encoding::Cp932, Units::Utf16).unwrap();
        // Two tokens on line 0: the string at 0 (4 mirror columns), x at 5.
        let mut data: Vec<Value> = [0, 0, 4, 1, 0, 0, 5, 1, 2, 0].iter().map(|&n| Value::from(n)).collect();
        semantic_tokens(&mut data, &m, Way::ToClient);
        let out: Vec<u64> = data.iter().map(|v| v.as_u64().unwrap()).collect();
        assert_eq!(out, [0, 0, 5, 1, 0, 0, 6, 1, 2, 0]);
    }
}
