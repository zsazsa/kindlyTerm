//! Single instance: the first kindlyTerm process owns the saved layout and
//! the detached shells; a second launch (dock click, `kindlyterm` typed in a
//! shell) hands its request to the running one and exits, instead of
//! restoring the same state.json and attaching to the same sessions twice.
//!
//! The socket lives in the session directory. One JSON line per request,
//! one line back.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub fn socket_path() -> PathBuf {
    crate::session::dir().join("instance.sock")
}

/// What a second launch asks the running instance to do.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Request {
    /// Open the Control Deck in the new window (`--deck`).
    #[serde(default)]
    pub deck: bool,
    /// Startup-notification token from the launcher, so the new window
    /// takes focus and GNOME stops its "launching" spinner.
    #[serde(default)]
    pub token: Option<String>,
    /// Working directory of the launching shell, for the new tab.
    #[serde(default)]
    pub cwd: Option<String>,
}

/// Try to hand `req` to an already running instance. `true` means it was
/// accepted and this process should exit.
pub fn forward(req: &Request) -> bool {
    let path = socket_path();
    if !path.exists() {
        return false;
    }
    let mut stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) => {
            // Nobody listening: a previous instance died without cleaning up.
            log::info!("stale instance socket ({e}); taking over");
            let _ = std::fs::remove_file(&path);
            return false;
        }
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let Ok(mut line) = serde_json::to_string(req) else { return false };
    line.push('\n');
    if stream.write_all(line.as_bytes()).is_err() {
        return false;
    }
    let mut reply = String::new();
    match BufReader::new(stream).read_line(&mut reply) {
        Ok(n) if n > 0 => {
            log::info!("handed off to the running instance");
            true
        }
        _ => false,
    }
}

/// Accept loop for the owning instance. Requests are queued and `wake` is
/// called so the UI thread drains them.
pub struct Listener {
    queue: Arc<Mutex<Vec<Request>>>,
    path: PathBuf,
}

impl Listener {
    pub fn start(wake: impl Fn() + Send + Sync + 'static) -> std::io::Result<Self> {
        crate::session::ensure_dir()?;
        let path = socket_path();
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        let queue: Arc<Mutex<Vec<Request>>> = Arc::new(Mutex::new(Vec::new()));
        {
            let queue = queue.clone();
            std::thread::Builder::new().name("instance-accept".into()).spawn(move || {
                for conn in listener.incoming() {
                    let Ok(stream) = conn else { continue };
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let mut reader = BufReader::new(&stream);
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() {
                        continue;
                    }
                    let req: Request = serde_json::from_str(line.trim()).unwrap_or_default();
                    queue.lock().unwrap().push(req);
                    wake();
                    let mut w = &stream;
                    let _ = w.write_all(b"{\"ok\":true}\n");
                }
            })?;
        }
        Ok(Self { queue, path })
    }

    pub fn drain(&self) -> Vec<Request> {
        std::mem::take(&mut *self.queue.lock().unwrap())
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
