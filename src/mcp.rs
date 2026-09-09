//! `kindlyterm --mcp`: a Model Context Protocol server over stdio that
//! lets Claude Code (or any MCP client) drive a running kindlyTerm through
//! its control socket. Register it once with:
//!
//!     claude mcp add kindlyterm -- kindlyterm --mcp
//!
//! Every tool is a thin call into the control API (`control.rs`); the
//! running app only answers when the user has turned the API on.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

const PROTOCOL: &str = "2024-11-05";

struct Tool {
    name: &'static str,
    description: &'static str,
    schema: Value,
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required})
}

fn tools() -> Vec<Tool> {
    let tid = json!({"type": "integer", "description": "terminal id from list_terminals"});
    let iid = json!({"type": "integer", "description": "item id from list_terminals / list_canvases"});
    vec![
        Tool { name: "list_canvases", description: "List windows and their tabs (canvases) with every item on them: terminals, mirrors, images, groups.", schema: obj(json!({}), &[]) },
        Tool { name: "list_terminals", description: "List running terminals: id, item id, canvas, title, size, session id, seconds since last output, flags.", schema: obj(json!({}), &[]) },
        Tool { name: "read_screen", description: "Text of a terminal's visible screen (what the user sees), trailing blank lines trimmed.", schema: obj(json!({"terminal_id": tid}), &["terminal_id"]) },
        Tool { name: "read_scrollback", description: "The last N lines of a terminal including scrollback (default 200, max 5000).", schema: obj(json!({"terminal_id": tid, "lines": {"type": "integer"}}), &["terminal_id"]) },
        Tool { name: "send_text", description: "Type text into a terminal. Set enter=true to press Enter afterwards. The user sees a notice.", schema: obj(json!({"terminal_id": tid, "text": {"type": "string"}, "enter": {"type": "boolean"}}), &["terminal_id", "text"]) },
        Tool { name: "send_key", description: "Press a key: enter, tab, escape, backspace, up/down/left/right, home, end, pageup, pagedown, f1..f12, or ctrl+<letter> (e.g. ctrl+c, ctrl+d, ctrl+z).", schema: obj(json!({"terminal_id": tid, "key": {"type": "string"}}), &["terminal_id", "key"]) },
        Tool { name: "create_terminal", description: "Open a new shell on a canvas (the active tab unless canvas_id is given; a plain tab becomes a canvas). Optional command runs in the shell, cwd sets the directory, name titles it.", schema: obj(json!({"canvas_id": {"type": "integer"}, "command": {"type": "string"}, "cwd": {"type": "string"}, "name": {"type": "string"}, "x": {"type": "number"}, "y": {"type": "number"}, "cols": {"type": "integer"}, "rows": {"type": "integer"}}), &[]) },
        Tool { name: "close_terminal", description: "End a terminal's shell and remove it (mirrors of it go too).", schema: obj(json!({"terminal_id": tid}), &["terminal_id"]) },
        Tool { name: "focus", description: "Focus a terminal or item and switch to its tab so keyboard input goes there.", schema: obj(json!({"item_id": iid}), &["item_id"]) },
        Tool { name: "move_item", description: "Move an item's top-left corner to world coordinates (pixels at zoom 1).", schema: obj(json!({"item_id": iid, "x": {"type": "number"}, "y": {"type": "number"}}), &["item_id", "x", "y"]) },
        Tool { name: "resize_item", description: "Resize a terminal item to a grid size in columns and rows (images: width in pixels).", schema: obj(json!({"item_id": iid, "cols": {"type": "integer"}, "rows": {"type": "integer"}, "width": {"type": "number"}}), &["item_id"]) },
        Tool { name: "rename_item", description: "Set an item's title (empty string clears a custom title).", schema: obj(json!({"item_id": iid, "name": {"type": "string"}}), &["item_id", "name"]) },
        Tool { name: "zoom_to", description: "Zoom the view to an item, a group, or the whole canvas (omit both ids to fit everything).", schema: obj(json!({"item_id": iid, "group_id": {"type": "integer"}}), &[]) },
        Tool { name: "set_viewport", description: "Set a canvas view: world x, y of the top-left and zoom.", schema: obj(json!({"canvas_id": {"type": "integer"}, "x": {"type": "number"}, "y": {"type": "number"}, "zoom": {"type": "number"}}), &["canvas_id"]) },
        Tool { name: "create_canvas", description: "Open a new empty canvas tab.", schema: obj(json!({"name": {"type": "string"}}), &[]) },
        Tool { name: "rename_canvas", description: "Rename a canvas tab.", schema: obj(json!({"canvas_id": {"type": "integer"}, "name": {"type": "string"}}), &["canvas_id", "name"]) },
        Tool { name: "delete_canvas", description: "Close a canvas tab and end every shell on it.", schema: obj(json!({"canvas_id": {"type": "integer"}}), &["canvas_id"]) },
        Tool { name: "create_group", description: "Frame items as a named group.", schema: obj(json!({"item_ids": {"type": "array", "items": {"type": "integer"}}, "name": {"type": "string"}}), &["item_ids"]) },
        Tool { name: "delete_group", description: "Dissolve a group (items stay).", schema: obj(json!({"group_id": {"type": "integer"}}), &["group_id"]) },
        Tool { name: "pin_item", description: "Pin an item to the screen (true) or release it (false).", schema: obj(json!({"item_id": iid, "pinned": {"type": "boolean"}}), &["item_id", "pinned"]) },
        Tool { name: "mirror_terminal", description: "Add a second live view of a terminal beside it.", schema: obj(json!({"item_id": iid}), &["item_id"]) },
        Tool { name: "set_monitor", description: "Watch a terminal for silence: after `seconds` without output its frame blinks. seconds=null turns it off.", schema: obj(json!({"item_id": iid, "seconds": {"type": ["integer", "null"]}}), &["item_id"]) },
        Tool { name: "get_activity", description: "Seconds since each terminal last produced output, and which are alerting as quiet.", schema: obj(json!({}), &[]) },
        Tool { name: "place_image", description: "Put an image file (png/jpg/gif/webp) on a canvas.", schema: obj(json!({"path": {"type": "string"}, "canvas_id": {"type": "integer"}}), &["path"]) },
        Tool { name: "remove_item", description: "Remove an image or mirror item (for terminals use close_terminal).", schema: obj(json!({"item_id": iid}), &["item_id"]) },
        Tool { name: "list_sessions", description: "Detached shell sessions (hosts) known to this machine, attached or not.", schema: obj(json!({}), &[]) },
        Tool { name: "screenshot", description: "Save a PNG of the active window to `path` (must end in .png).", schema: obj(json!({"path": {"type": "string"}}), &["path"]) },
    ]
}

fn reply(out: &mut impl Write, id: Value, result: Value) {
    let _ = writeln!(out, "{}", json!({"jsonrpc": "2.0", "id": id, "result": result}));
    let _ = out.flush();
}

fn reply_err(out: &mut impl Write, id: Value, code: i64, msg: &str) {
    let _ = writeln!(out, "{}", json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}}));
    let _ = out.flush();
}

/// Run the stdio server until stdin closes.
pub fn run() -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let tool_list = tools();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                reply_err(&mut out, Value::Null, -32700, &format!("parse error: {e}"));
                continue;
            }
        };
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        match method {
            "initialize" => reply(
                &mut out,
                id,
                json!({
                    "protocolVersion": PROTOCOL,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "kindlyterm", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": "Tools drive the running kindlyTerm window: list_terminals first to get ids, read_screen to see output, send_text/send_key to type. The user must have enabled the control API in the Deck (About page)."
                }),
            ),
            "notifications/initialized" | "notifications/cancelled" => {}
            "ping" => reply(&mut out, id, json!({})),
            "tools/list" => {
                let list: Vec<Value> = tool_list.iter().map(|t| json!({"name": t.name, "description": t.description, "inputSchema": t.schema})).collect();
                reply(&mut out, id, json!({"tools": list}));
            }
            "tools/call" => {
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                if !tool_list.iter().any(|t| t.name == name) {
                    reply_err(&mut out, id, -32602, &format!("unknown tool {name}"));
                    continue;
                }
                match crate::control::call(name, args) {
                    Ok(v) => {
                        let text = match &v {
                            Value::String(s) => s.clone(),
                            other => serde_json::to_string_pretty(other).unwrap_or_default(),
                        };
                        reply(&mut out, id, json!({"content": [{"type": "text", "text": text}]}));
                    }
                    Err(e) => reply(&mut out, id, json!({"content": [{"type": "text", "text": e}], "isError": true})),
                }
            }
            "resources/list" => reply(&mut out, id, json!({"resources": []})),
            "prompts/list" => reply(&mut out, id, json!({"prompts": []})),
            _ if id.is_null() => {}
            other => reply_err(&mut out, id, -32601, &format!("method not found: {other}")),
        }
    }
    Ok(())
}
