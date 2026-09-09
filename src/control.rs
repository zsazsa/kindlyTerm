//! Control socket: a tiny line-oriented JSON API the UI serves when the
//! user turns it on (Deck → About → "Let tools drive kindlyTerm"). The
//! `--mcp` stdio bridge (see `mcp.rs`) is its only intended client, but
//! anything that can write JSON lines to the socket can use it.
//!
//! One request per line: `{"id":1,"method":"list_terminals","params":{}}`
//! One reply per line:   `{"id":1,"result":…}` or `{"id":1,"error":"…"}`
//!
//! The socket lives in the session directory (mode 0600) and is removed
//! when the API is turned off or the app exits.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use serde_json::Value;

pub fn socket_path() -> PathBuf {
    crate::session::dir().join("control.sock")
}

/// Outcome of one request.
pub type Reply = Result<Value, String>;

/// A request handed from a connection thread to the UI thread.
pub struct Request {
    pub method: String,
    pub params: Value,
    /// Where the UI thread puts the answer.
    pub reply: Sender<Reply>,
}

/// Queue shared between connection threads and the UI thread.
pub type Queue = Arc<Mutex<Vec<Request>>>;

/// Accept loop plus per-connection threads. `wake` is called after each
/// request is queued so the UI thread drains the queue.
pub struct Server {
    queue: Queue,
    stop: Arc<std::sync::atomic::AtomicBool>,
    path: PathBuf,
}

impl Server {
    pub fn start(wake: impl Fn() + Send + Sync + 'static) -> std::io::Result<Self> {
        crate::session::ensure_dir()?;
        let path = socket_path();
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        let queue: Queue = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let wake = Arc::new(wake);
        {
            let queue = queue.clone();
            let stop = stop.clone();
            std::thread::Builder::new().name("control-accept".into()).spawn(move || {
                for conn in listener.incoming() {
                    if stop.load(std::sync::atomic::Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = conn else { continue };
                    let queue = queue.clone();
                    let wake = wake.clone();
                    let _ = std::thread::Builder::new().name("control-conn".into()).spawn(move || serve(stream, queue, wake));
                }
            })?;
        }
        Ok(Self { queue, stop, path })
    }

    /// Take everything queued so far.
    pub fn drain(&self) -> Vec<Request> {
        std::mem::take(&mut *self.queue.lock().unwrap())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = std::fs::remove_file(&self.path);
        // Unblock the accept loop.
        let _ = UnixStream::connect(&self.path);
    }
}

fn serve(stream: UnixStream, queue: Queue, wake: Arc<dyn Fn() + Send + Sync>) {
    let Ok(mut out) = stream.try_clone() else { return };
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let _ = writeln!(out, "{}", serde_json::json!({"id": null, "error": format!("bad json: {e}")}));
                continue;
            }
        };
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("").to_string();
        let params = msg.get("params").cloned().unwrap_or(Value::Object(Default::default()));
        let (tx, rx) = channel::<Reply>();
        queue.lock().unwrap().push(Request { method, params, reply: tx });
        wake();
        let reply = match rx.recv_timeout(std::time::Duration::from_secs(30)) {
            Ok(Ok(v)) => serde_json::json!({"id": id, "result": v}),
            Ok(Err(e)) => serde_json::json!({"id": id, "error": e}),
            Err(_) => serde_json::json!({"id": id, "error": "timed out waiting for the UI"}),
        };
        if writeln!(out, "{reply}").is_err() {
            break;
        }
    }
}

/// Client side: one call over a fresh connection.
pub fn call(method: &str, params: Value) -> Result<Value, String> {
    let path = socket_path();
    let mut stream = UnixStream::connect(&path).map_err(|e| {
        format!("kindlyTerm's control API is not reachable ({e}). Open the Deck (Ctrl+Shift+,) → About and turn on \"Let tools drive kindlyTerm\".")
    })?;
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(35)));
    let req = serde_json::json!({"id": 1, "method": method, "params": params});
    writeln!(stream, "{req}").map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_str(&line).map_err(|e| format!("bad reply: {e}"))?;
    if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
        return Err(err.to_string());
    }
    Ok(v.get("result").cloned().unwrap_or(Value::Null))
}
