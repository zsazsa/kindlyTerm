//! Detached sessions: the wire protocol between the UI and a PTY host, plus
//! where host sockets live.
//!
//! A host (`kindlyterm --host <id>`) owns one PTY and its shell. The UI
//! connects over a Unix socket, sends what it has already seen, and the
//! host either replays the missing output or sends a snapshot of the
//! screen re-materialised as escape sequences. Everything is local; the
//! socket directory is private to the user.
//!
//! Frames are `[kind: u8][len: u32 LE][payload]`. Numbers inside payloads
//! are little-endian.

use std::io::{self, Read};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::PathBuf;

/// Largest payload a peer may send. Output frames are chunked well below.
pub const MAX_FRAME: usize = 8 * 1024 * 1024;

/// Sentinel `last_seq` meaning "I have nothing yet".
pub const SEQ_NONE: u64 = u64::MAX;

/// Messages from the UI to the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToHost {
    /// First frame after connecting. `last_seq` is how many output bytes
    /// the client has already applied (`SEQ_NONE` for a fresh client).
    Hello { last_seq: u64, cols: u16, rows: u16, cell_w: u16, cell_h: u16 },
    Input(Vec<u8>),
    Resize { cols: u16, rows: u16, cell_w: u16, cell_h: u16 },
    /// Hang up the shell and exit. The socket goes away with the host.
    Kill,
    Ping,
    /// Ask who is here without attaching: answered with `Attached` and
    /// nothing else. Used by `--sessions` and orphan adoption.
    Query,
}

/// Messages from the host to the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromHost {
    /// Answer to `Hello`. What follows until `Live` is catch-up data:
    /// either a byte-exact replay or a snapshot; either way the client
    /// should stay quiet (answer no queries) until `Live`.
    Attached { seq: u64, replay: bool, title: Option<String>, program: String, cwd: Option<String>, pid: u32 },
    /// Raw PTY output starting at byte `seq`.
    Output { seq: u64, bytes: Vec<u8> },
    /// Catch-up complete; output from here on is live.
    Live { seq: u64 },
    /// The shell exited. The host exits right after sending this.
    Exited { code: i32 },
    Pong,
}

// Frame kinds.
const K_HELLO: u8 = 1;
const K_INPUT: u8 = 2;
const K_RESIZE: u8 = 3;
const K_KILL: u8 = 4;
const K_PING: u8 = 5;
const K_QUERY: u8 = 6;
const K_ATTACHED: u8 = 0x81;
const K_OUTPUT: u8 = 0x82;
const K_LIVE: u8 = 0x83;
const K_EXITED: u8 = 0x84;
const K_PONG: u8 = 0x85;

fn put_u16(b: &mut Vec<u8>, v: u16) {
    b.extend_from_slice(&v.to_le_bytes());
}
fn put_u32(b: &mut Vec<u8>, v: u32) {
    b.extend_from_slice(&v.to_le_bytes());
}
fn put_u64(b: &mut Vec<u8>, v: u64) {
    b.extend_from_slice(&v.to_le_bytes());
}
fn put_str(b: &mut Vec<u8>, s: &str) {
    put_u32(b, s.len() as u32);
    b.extend_from_slice(s.as_bytes());
}
fn put_opt_str(b: &mut Vec<u8>, s: Option<&str>) {
    match s {
        Some(s) => {
            b.push(1);
            put_str(b, s);
        }
        None => b.push(0),
    }
}

/// Cursor over a payload with bounds-checked reads.
struct Cur<'a>(&'a [u8]);

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        if self.0.len() < n {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "short frame"));
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> io::Result<i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn str(&mut self) -> io::Result<String> {
        let n = self.u32()? as usize;
        let s = self.take(n)?;
        String::from_utf8(s.to_vec()).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad utf-8"))
    }
    fn opt_str(&mut self) -> io::Result<Option<String>> {
        Ok(if self.u8()? == 1 { Some(self.str()?) } else { None })
    }
    fn rest(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.0).to_vec()
    }
}

impl ToHost {
    pub fn encode(&self) -> Vec<u8> {
        let mut p = Vec::new();
        let kind = match self {
            ToHost::Hello { last_seq, cols, rows, cell_w, cell_h } => {
                put_u64(&mut p, *last_seq);
                for v in [cols, rows, cell_w, cell_h] {
                    put_u16(&mut p, *v);
                }
                K_HELLO
            }
            ToHost::Input(b) => {
                p.extend_from_slice(b);
                K_INPUT
            }
            ToHost::Resize { cols, rows, cell_w, cell_h } => {
                for v in [cols, rows, cell_w, cell_h] {
                    put_u16(&mut p, *v);
                }
                K_RESIZE
            }
            ToHost::Kill => K_KILL,
            ToHost::Ping => K_PING,
            ToHost::Query => K_QUERY,
        };
        frame(kind, &p)
    }

    fn decode(kind: u8, payload: &[u8]) -> io::Result<Self> {
        let mut c = Cur(payload);
        Ok(match kind {
            K_HELLO => {
                let last_seq = c.u64()?;
                ToHost::Hello { last_seq, cols: c.u16()?, rows: c.u16()?, cell_w: c.u16()?, cell_h: c.u16()? }
            }
            K_INPUT => ToHost::Input(c.rest()),
            K_RESIZE => ToHost::Resize { cols: c.u16()?, rows: c.u16()?, cell_w: c.u16()?, cell_h: c.u16()? },
            K_KILL => ToHost::Kill,
            K_PING => ToHost::Ping,
            K_QUERY => ToHost::Query,
            _ => return Err(io::Error::new(io::ErrorKind::InvalidData, format!("unknown frame kind {kind:#x}"))),
        })
    }
}

impl FromHost {
    pub fn encode(&self) -> Vec<u8> {
        let mut p = Vec::new();
        let kind = match self {
            FromHost::Attached { seq, replay, title, program, cwd, pid } => {
                put_u64(&mut p, *seq);
                p.push(*replay as u8);
                put_opt_str(&mut p, title.as_deref());
                put_str(&mut p, program);
                put_opt_str(&mut p, cwd.as_deref());
                put_u32(&mut p, *pid);
                K_ATTACHED
            }
            FromHost::Output { seq, bytes } => {
                put_u64(&mut p, *seq);
                p.extend_from_slice(bytes);
                K_OUTPUT
            }
            FromHost::Live { seq } => {
                put_u64(&mut p, *seq);
                K_LIVE
            }
            FromHost::Exited { code } => {
                p.extend_from_slice(&code.to_le_bytes());
                K_EXITED
            }
            FromHost::Pong => K_PONG,
        };
        frame(kind, &p)
    }

    fn decode(kind: u8, payload: &[u8]) -> io::Result<Self> {
        let mut c = Cur(payload);
        Ok(match kind {
            K_ATTACHED => {
                let seq = c.u64()?;
                let replay = c.u8()? == 1;
                FromHost::Attached { seq, replay, title: c.opt_str()?, program: c.str()?, cwd: c.opt_str()?, pid: c.u32()? }
            }
            K_OUTPUT => {
                let seq = c.u64()?;
                FromHost::Output { seq, bytes: c.rest() }
            }
            K_LIVE => FromHost::Live { seq: c.u64()? },
            K_EXITED => FromHost::Exited { code: c.i32()? },
            K_PONG => FromHost::Pong,
            _ => return Err(io::Error::new(io::ErrorKind::InvalidData, format!("unknown frame kind {kind:#x}"))),
        })
    }
}

fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(5 + payload.len());
    f.push(kind);
    put_u32(&mut f, payload.len() as u32);
    f.extend_from_slice(payload);
    f
}

/// Read one raw frame. `Ok(None)` on a clean EOF at a frame boundary.
fn read_frame<R: Read>(r: &mut R) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut head = [0u8; 5];
    let mut got = 0;
    while got < head.len() {
        match r.read(&mut head[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "eof inside frame header")),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let kind = head[0];
    let len = u32::from_le_bytes(head[1..5].try_into().unwrap()) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    Ok(Some((kind, payload)))
}

pub fn read_to_host<R: Read>(r: &mut R) -> io::Result<Option<ToHost>> {
    Ok(match read_frame(r)? {
        Some((k, p)) => Some(ToHost::decode(k, &p)?),
        None => None,
    })
}

pub fn read_from_host<R: Read>(r: &mut R) -> io::Result<Option<FromHost>> {
    Ok(match read_frame(r)? {
        Some((k, p)) => Some(FromHost::decode(k, &p)?),
        None => None,
    })
}

// ---------------------------------------------------------------------------
// Session directory
// ---------------------------------------------------------------------------

/// Private directory holding one socket (and log) per live host.
pub fn dir() -> PathBuf {
    // Debug builds of a test run keep their sessions apart from real ones.
    if std::env::var_os("KINDLYTERM_DEBUG").is_some()
        && let Some(d) = std::env::var_os("KINDLYTERM_SESSION_DIR")
    {
        return PathBuf::from(d);
    }
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| {
            let uid = unsafe { libc::getuid() };
            std::env::temp_dir().join(format!("kindlyterm-{uid}"))
        });
    base.join("kindlyterm")
}

/// Create the session directory with private permissions.
pub fn ensure_dir() -> io::Result<PathBuf> {
    let d = dir();
    if !d.exists() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&d)?;
    }
    // Tighten in case it pre-existed with looser bits.
    let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700));
    Ok(d)
}

pub fn socket_path(id: &str) -> PathBuf {
    dir().join(format!("{id}.sock"))
}

pub fn log_path(id: &str) -> PathBuf {
    dir().join(format!("{id}.log"))
}

/// A session id is a short hex string: safe in file names and unguessable
/// enough for a private directory.
pub fn new_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let mut h = std::hash::RandomState::new().build_hasher();
    h.write_u128(t);
    h.write_u32(std::process::id());
    format!("{:012x}", h.finish() & 0xffff_ffff_ffff)
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Ids of every socket in the session directory (live or stale).
pub fn list_ids() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir()) {
        for e in rd.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if let Some(id) = name.strip_suffix(".sock")
                && valid_id(id)
            {
                out.push(id.to_string());
            }
        }
    }
    out.sort();
    out
}

/// Remove a session's socket and log.
pub fn remove_files(id: &str) {
    let _ = std::fs::remove_file(socket_path(id));
    let _ = std::fs::remove_file(log_path(id));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let msgs = vec![
            ToHost::Hello { last_seq: SEQ_NONE, cols: 80, rows: 24, cell_w: 8, cell_h: 17 },
            ToHost::Input(b"ls\r".to_vec()),
            ToHost::Resize { cols: 100, rows: 30, cell_w: 9, cell_h: 18 },
            ToHost::Kill,
            ToHost::Ping,
        ];
        let mut buf = Vec::new();
        for m in &msgs {
            buf.extend(m.encode());
        }
        let mut r = &buf[..];
        for m in &msgs {
            assert_eq!(read_to_host(&mut r).unwrap().unwrap(), *m);
        }
        assert!(read_to_host(&mut r).unwrap().is_none());

        let msgs = vec![
            FromHost::Attached { seq: 7, replay: true, title: Some("vim".into()), program: "bash".into(), cwd: None, pid: 42 },
            FromHost::Output { seq: 7, bytes: b"\x1b[31mhi".to_vec() },
            FromHost::Live { seq: 15 },
            FromHost::Exited { code: -1 },
            FromHost::Pong,
        ];
        let mut buf = Vec::new();
        for m in &msgs {
            buf.extend(m.encode());
        }
        let mut r = &buf[..];
        for m in &msgs {
            assert_eq!(read_from_host(&mut r).unwrap().unwrap(), *m);
        }
    }

    #[test]
    fn ids() {
        let a = new_id();
        assert!(valid_id(&a));
        assert!(!valid_id("../x"));
        assert!(!valid_id(""));
    }
}
