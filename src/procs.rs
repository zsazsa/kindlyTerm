//! What runs in the foreground of a terminal, read from /proc. Lets the
//! control API say "this card is running Claude Code" without guessing
//! from window titles.

/// The process group leader currently in the foreground of a terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Foreground {
    pub pid: u32,
    /// Kernel command name (`/proc/<pid>/comm`), e.g. "bash", "claude", "node".
    pub name: String,
    /// Full command line with arguments joined by spaces.
    pub cmdline: String,
}

/// Foreground process of the terminal whose shell has pid `shell_pid`: the
/// terminal's foreground process group, or the shell itself when nothing
/// else is running.
pub fn foreground_of(shell_pid: u32) -> Option<Foreground> {
    let stat = std::fs::read_to_string(format!("/proc/{shell_pid}/stat")).ok()?;
    let tpgid = tpgid_from_stat(&stat)?;
    let pid = if tpgid > 0 { tpgid as u32 } else { shell_pid };
    let name = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?.trim().to_string();
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|b| String::from_utf8_lossy(&b).split('\0').filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    Some(Foreground { pid, name, cmdline })
}

/// The `tpgid` field of a `/proc/<pid>/stat` line. The command name in
/// parentheses may itself contain spaces and parentheses, so the fixed
/// fields are counted from the last ')'.
pub fn tpgid_from_stat(stat: &str) -> Option<i64> {
    let rest = &stat[stat.rfind(')')? + 1..];
    // After the name: state ppid pgrp session tty_nr tpgid ...
    rest.split_whitespace().nth(5)?.parse().ok()
}

/// Which coding agent `fg` is, if any. Claude Code runs as a native
/// `claude` binary or as node running the claude-code package.
pub fn agent_of(fg: &Foreground) -> Option<&'static str> {
    let args: Vec<&str> = fg.cmdline.split(' ').collect();
    let has = |pred: &dyn Fn(&str) -> bool| args.iter().any(|a| pred(a));
    if fg.name == "claude" || has(&|a| a == "claude" || a.ends_with("/claude") || a.contains("@anthropic-ai/claude-code")) {
        return Some("claude");
    }
    if fg.name == "codex" || has(&|a| a == "codex" || a.ends_with("/codex")) {
        return Some("codex");
    }
    if fg.name == "gemini" || has(&|a| a == "gemini" || a.ends_with("/gemini")) {
        return Some("gemini");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tpgid_survives_odd_command_names() {
        let stat = "4242 (my (odd) name) S 1 4242 4242 34816 5678 4194560 100 0 0 0 1 2 0 0 20 0 1 0 12345 100000 200 18446744073709551615";
        assert_eq!(tpgid_from_stat(stat), Some(5678));
        assert_eq!(tpgid_from_stat("garbage"), None);
    }

    #[test]
    fn claude_is_recognised_native_or_under_node() {
        let native = Foreground { pid: 1, name: "claude".into(), cmdline: "claude".into() };
        let node = Foreground { pid: 1, name: "node".into(), cmdline: "node /home/u/.npm-global/bin/claude --resume".into() };
        let pkg = Foreground { pid: 1, name: "node".into(), cmdline: "node /usr/lib/node_modules/@anthropic-ai/claude-code/cli.js".into() };
        let shell = Foreground { pid: 1, name: "bash".into(), cmdline: "bash".into() };
        let vim = Foreground { pid: 1, name: "vim".into(), cmdline: "vim claude-notes.md".into() };
        assert_eq!(agent_of(&native), Some("claude"));
        assert_eq!(agent_of(&node), Some("claude"));
        assert_eq!(agent_of(&pkg), Some("claude"));
        assert_eq!(agent_of(&shell), None);
        assert_eq!(agent_of(&vim), None);
    }

    #[test]
    fn this_test_process_has_a_foreground() {
        // The test binary's own pid always has a readable stat line.
        let me = std::process::id();
        let fg = foreground_of(me);
        assert!(fg.is_some() || !std::path::Path::new("/proc").exists());
    }
}
