//! Shared PTY harness for the pty_smoke cases.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;
use std::time::{Duration, Instant};

// paths
pub fn repo_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .expect("CARGO_MANIFEST_DIR is bin/tui")
}

pub fn exts_root() -> PathBuf {
    if let Ok(p) = std::env::var("EXTS_ROOT") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let repo = repo_root();
    let sibling = repo.join("../rushi-exts");
    if sibling.join("ui_extensions").is_dir() {
        return sibling.canonicalize().unwrap_or(sibling);
    }
    repo
}

pub fn fixture_root() -> PathBuf {
    if let Ok(p) = std::env::var("EXT_FIXTURES") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let repo = repo_root();
    let in_repo = repo.join("scripts/ext-fixture");
    if in_repo.is_dir() {
        return in_repo;
    }
    let in_harness = repo.join("../rust-unix-harness/scripts/ext-fixture");
    in_harness.canonicalize().unwrap_or(in_harness)
}

pub fn tmpdir() -> PathBuf {
    let p = std::env::temp_dir().join(format!("tui-pty-smoke-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

// screen
pub struct Screen {
    pub rows: usize,
    pub cols: usize,
    pub grid: Vec<Vec<char>>,
    pub r: usize,
    pub c: usize,
    pub raw: Vec<u8>,
    // A partial escape sequence split across two reads is held here and
    // prepended to the next chunk before parsing.
    carry: String,
}

impl Screen {
    pub fn new(rows: usize, cols: usize) -> Self {
        let grid = (0..rows).map(|_| vec![' '; cols]).collect();
        Screen {
            rows,
            cols,
            grid,
            r: 0,
            c: 0,
            raw: Vec::new(),
            carry: String::new(),
        }
    }

    pub fn feed(&mut self, data: &str) {
        self.raw.extend(data.as_bytes());
        let mut full = self.carry.clone();
        full.push_str(data);
        self.carry.clear();
        let safe = safe_prefix_len(&full);
        self.carry = full[safe..].to_string();
        self.parse(&full[..safe]);
    }

    fn parse(&mut self, data: &str) {
        let chars: Vec<char> = data.chars().collect();
        let mut i = 0usize;
        while i < chars.len() {
            match chars[i] {
                '\u{1b}' => {
                    i += 1;
                    if i < chars.len() && chars[i] == '[' {
                        i += 1;
                        let mut params = String::new();
                        let mut private = false;
                        while i < chars.len() {
                            match chars[i] {
                                ch if ch.is_ascii_digit() || ch == ';' => {
                                    params.push(ch);
                                    i += 1;
                                }
                                '?' => {
                                    private = true;
                                    i += 1;
                                }
                                _ => break,
                            }
                        }
                        if i < chars.len() {
                            let fin = chars[i];
                            i += 1;
                            self.apply_csi(fin, &params, private);
                        }
                    }
                }
                '\r' => {
                    self.c = 0;
                    i += 1;
                }
                '\n' => {
                    self.r = (self.r + 1).min(self.rows - 1);
                    self.c = 0;
                    i += 1;
                }
                ch => {
                    if !ch.is_control() && self.c < self.cols {
                        self.grid[self.r][self.c] = ch;
                        self.c = (self.c + 1) % self.cols;
                    }
                    i += 1;
                }
            }
        }
    }

    fn apply_csi(&mut self, fin: char, params: &str, private: bool) {
        match fin {
            'H' | 'F' => {
                let p: Vec<&str> = params.split(';').collect();
                let r0 = p.first().and_then(|s| s.parse::<usize>().ok());
                let c0 = p.get(1).and_then(|s| s.parse::<usize>().ok());
                self.r = r0.unwrap_or(1).saturating_sub(1).min(self.rows - 1);
                self.c = c0.unwrap_or(1).saturating_sub(1).min(self.cols - 1);
            }
            'K' => {
                for k in self.c..self.cols {
                    self.grid[self.r][k] = ' ';
                }
            }
            'J' => {
                self.grid = (0..self.rows).map(|_| vec![' '; self.cols]).collect();
            }
            'd' => {
                if !params.is_empty() && !private {
                    if let Ok(v) = params.parse::<usize>() {
                        self.r = v.saturating_sub(1).min(self.rows - 1);
                    }
                }
                self.c = 0;
            }
            _ => {}
        }
    }

    pub fn text(&self) -> String {
        self.grid
            .iter()
            .map(|row| row.iter().collect::<String>().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// The largest prefix of `s` that does not end mid-escape-sequence.
///
/// A read can split a crossterm escape (ESC / `[` / partial params), so
/// the trailing fragment is withheld for the next chunk. The returned
/// index is always a char boundary (an ESC is a single byte).
fn safe_prefix_len(s: &str) -> usize {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut last_esc: Option<usize> = None;
    for (i, &b) in bytes.iter().enumerate() {
        if b == 0x1B {
            last_esc = Some(i);
        }
    }
    let Some(p) = last_esc else {
        return n;
    };
    if p + 1 >= n {
        // Only the ESC byte is present so far; the next chunk continues.
        return p;
    }
    if bytes[p + 1] != b'[' {
        // Two-byte escape (ESC X); complete once the second byte is in.
        return n;
    }
    // CSI: scan params until the final byte; a missing final byte means
    // the sequence is still open.
    let mut j = p + 2;
    while j < n {
        let b = bytes[j];
        if (0x30..=0x39).contains(&b) || b == b';' || b == b'?' {
            j += 1;
        } else {
            return n;
        }
    }
    p
}

// pty
pub struct Pty {
    pub master: i32,
    pub child: i32,
    pub screen: Screen,
    reaped: bool,
    /// Tail bytes of the last read that do not yet form complete
    /// UTF-8; a multibyte character split across two reads (the
    /// Nerd Font footer glyphs are 3-byte sequences) decodes whole
    /// instead of turning into U+FFFD under `from_utf8_lossy`.
    pending_utf8: Vec<u8>,
}

impl Pty {
    pub fn spawn(bin: &str, session: &str, cfg: &Path, extra_path: Option<&str>) -> Self {
        let mut amaster: libc::c_int = 0;
        let mut aslave: libc::c_int = 0;
        let mut winsz = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pid = unsafe {
            if libc::openpty(
                &mut amaster,
                &mut aslave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut winsz,
            ) != 0
            {
                panic!("openpty failed: {}", std::io::Error::last_os_error());
            }
            let _ = libc::ioctl(amaster, libc::TIOCSWINSZ, &winsz);
            let pid = libc::fork();
            assert!(pid >= 0, "fork failed");
            if pid == 0 {
                // child side
                let _ = libc::setsid();
                let _ = libc::dup2(aslave, 0);
                let _ = libc::dup2(aslave, 1);
                let _ = libc::dup2(aslave, 2);
                let _ = libc::close(amaster);
                let _ = libc::close(aslave);
                if let Some(p) = extra_path {
                    let new_path = format!("{p}:{}", std::env::var("PATH").unwrap_or_default());
                    let key = CString::new("PATH").unwrap();
                    let val = CString::new(new_path).unwrap();
                    let _ = libc::setenv(key.as_ptr(), val.as_ptr(), 1);
                }
                let c_bin = CString::new(bin).expect("bin has no NUL");
                let c_sess = CString::new(session).expect("session has no NUL");
                let c_cfg = CString::new(cfg.to_string_lossy().as_ref()).expect("cfg has no NUL");
                let c_flag = c"--config";
                let args = vec![
                    c_bin.as_ptr(),
                    c_sess.as_ptr(),
                    c_flag.as_ptr(),
                    c_cfg.as_ptr(),
                    std::ptr::null(),
                ];
                libc::execvp(c_bin.as_ptr(), args.as_ptr());
                libc::_exit(127);
            }
            pid
        };
        let _ = unsafe { libc::close(aslave) };
        Pty {
            master: amaster,
            child: pid as i32,
            screen: Screen::new(24, 80),
            reaped: false,
            pending_utf8: Vec::new(),
        }
    }

    pub fn pump(&mut self, secs: f64) {
        let end = Instant::now() + Duration::from_secs_f64(secs);
        while Instant::now() < end {
            let mut pfd = libc::pollfd {
                fd: self.master,
                events: libc::POLLIN,
                revents: 0,
            };
            let n = unsafe { libc::poll(&mut pfd, 1, 100) };
            if n > 0 && pfd.revents & libc::POLLIN != 0 {
                let mut buf = [0u8; 65536];
                let r =
                    unsafe { libc::read(self.master, buf.as_mut_ptr() as *mut _, buf.len() as _) };
                if r <= 0 {
                    break;
                }
                self.pending_utf8.extend_from_slice(&buf[..r as usize]);
                // Feed the screen the largest complete-UTF-8 prefix and
                // carry a partial multibyte tail to the next read. A
                // lossy per-read decode would turn a footer glyph split
                // across reads into U+FFFD and markers would miss the
                // real glyph.
                let complete = std::str::from_utf8(&self.pending_utf8)
                    .map(|s| s.len())
                    .unwrap_or_else(|e| e.valid_up_to());
                if complete > 0 {
                    let chunk = std::str::from_utf8(&self.pending_utf8[..complete]).unwrap();
                    self.screen.feed(chunk);
                    let rest = self.pending_utf8[complete..].to_vec();
                    self.pending_utf8 = rest;
                }
            }
        }
    }

    pub fn write_input(&self, data: &[u8]) {
        let mut off = 0usize;
        while off < data.len() {
            let n = unsafe {
                libc::write(
                    self.master,
                    data.as_ptr().add(off) as *const _,
                    (data.len() - off) as _,
                )
            };
            if n <= 0 {
                break;
            }
            off += n as usize;
        }
    }

    /// True while the child still runs. It reaps the child when it exits.
    pub fn alive(&mut self) -> bool {
        if self.reaped {
            return false;
        }
        let mut status: libc::c_int = 0;
        loop {
            let r = unsafe { libc::waitpid(self.child, &mut status, libc::WNOHANG) };
            if r == self.child {
                self.reaped = true;
                return false;
            }
            if r < 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                // ECHILD: the child was already reaped.
                self.reaped = true;
                return false;
            }
            return true;
        }
    }

    pub fn reap(&mut self) {
        let mut status: libc::c_int = 0;
        loop {
            let r = unsafe { libc::waitpid(self.child, &mut status, 0) };
            if r == self.child {
                self.reaped = true;
                break;
            }
            if r < 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                break;
            }
        }
    }

    pub fn kill(&self, sig: libc::c_int) {
        unsafe {
            libc::kill(self.child, sig);
        }
    }

    pub fn kill_group(&self, sig: libc::c_int) {
        unsafe {
            libc::kill(-self.child, sig);
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        if !self.reaped {
            self.kill(libc::SIGKILL);
            self.reap();
        }
        unsafe {
            libc::close(self.master);
        }
    }
}

/// Press the insert-exit key and then `q q` to quit from normal mode.
/// Returns true when the TUI exited on its own before the deadline.
pub fn double_q_quit(pty: &mut Pty, grace_secs: f64) -> bool {
    pty.write_input(b"\x1b");
    pty.pump(0.3);
    pty.write_input(b"q");
    pty.pump(0.4);
    pty.write_input(b"q");
    let end = Instant::now() + Duration::from_secs_f64(grace_secs);
    while pty.alive() && Instant::now() < end {
        pty.pump(0.2);
    }
    let ok = !pty.alive();
    if !ok {
        pty.kill(libc::SIGKILL);
        pty.reap();
    }
    ok
}

// burst writer and marker waiter
pub fn write_burst(pty: &mut Pty, data: &[u8], deadline_secs: f64) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    let master = pty.master;
    let payload = data.to_vec();
    std::thread::spawn(move || {
        let mut off = 0usize;
        while off < payload.len() {
            let n = unsafe {
                libc::write(
                    master,
                    payload.as_ptr().add(off) as *const _,
                    (payload.len() - off) as _,
                )
            };
            if n <= 0 {
                break;
            }
            off += n as usize;
        }
        let _ = tx.send(());
    });
    let end = Instant::now() + Duration::from_secs_f64(deadline_secs);
    let mut done = false;
    while !done && Instant::now() < end {
        pty.pump(0.05);
        done = rx.try_recv().is_ok();
    }
    done
}

pub fn wait_markers(
    pty: &mut Pty,
    markers: &[&str],
    deadline_secs: f64,
    grace_secs: f64,
) -> Vec<String> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut extended = false;
    let mut deadline = Instant::now() + Duration::from_secs_f64(deadline_secs);
    loop {
        pty.pump(if extended { 0.25 } else { 0.15 });
        let text = pty.screen.text();
        for m in markers {
            if text.contains(m) {
                seen.insert(*m);
            }
        }
        if seen.len() == markers.len() {
            break;
        }
        if !pty.alive() {
            break;
        }
        if Instant::now() >= deadline {
            if !extended {
                extended = true;
                deadline = Instant::now() + Duration::from_secs_f64(grace_secs);
                continue;
            }
            break;
        }
    }
    markers
        .iter()
        .filter(|m| !seen.contains(*m))
        .map(|m| (*m).to_string())
        .collect()
}

// orphan detection
fn ppid_of(pid: u32) -> i32 {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| {
            // skip past the parenthesized comm field
            let rest = s.rsplit(')').next()?;
            rest.split_whitespace().nth(1)?.parse().ok()
        })
        .unwrap_or(-1)
}

/// True when the process is a zombie (dead, awaiting reap).
fn is_zombie(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| {
            // Field 3 after the parenthesized comm field is the state.
            let rest = s.rsplit(')').next()?;
            rest.split_whitespace()
                .nth(0)?
                .chars()
                .next()
                .map(|c| c == 'Z')
        })
        .unwrap_or(false)
}

fn under_prefix(cwd: &Path, prefix: &Path) -> bool {
    let s = prefix.to_string_lossy();
    let c = cwd.to_string_lossy();
    c == s || c.starts_with(&format!("{s}/"))
}

pub fn procs_with_cwd_under(prefix: &Path, any_parent: bool) -> Vec<u32> {
    let mut out = Vec::new();
    for d in std::fs::read_dir("/proc").into_iter().flatten() {
        let Ok(d) = d else { continue };
        let name = d.file_name().to_string_lossy().to_string();
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        if is_zombie(pid) {
            continue;
        }
        let Ok(cwd) = std::fs::read_link(format!("/proc/{pid}/cwd")) else {
            continue;
        };
        if !under_prefix(&cwd, prefix) {
            continue;
        }
        if !any_parent && ppid_of(pid) != 1 {
            continue;
        }
        out.push(pid);
    }
    out
}

pub fn settled_orphans(prefix: &Path, secs: f64) -> Vec<u32> {
    let end = Instant::now() + Duration::from_secs_f64(secs);
    loop {
        let pids = procs_with_cwd_under(prefix, false);
        if pids.is_empty() || Instant::now() >= end {
            return pids;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

pub fn cleanup_layer(child: i32, prefix: &Path) {
    unsafe {
        libc::kill(-child, libc::SIGKILL);
    }
    std::thread::sleep(Duration::from_millis(500));
    for p in settled_orphans(prefix, 6.0) {
        unsafe {
            libc::kill(p as i32, libc::SIGKILL);
        }
    }
}

pub fn purge_strays() {
    let exts = exts_root();
    let prefixes = vec![exts.join("ui_extensions"), exts.join("ext-rs")];
    let mut killed = 0usize;
    for d in std::fs::read_dir("/proc").into_iter().flatten() {
        let Ok(d) = d else { continue };
        let name = d.file_name().to_string_lossy().to_string();
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        let Ok(cwd) = std::fs::read_link(format!("/proc/{pid}/cwd")) else {
            continue;
        };
        if is_zombie(pid) {
            continue;
        }
        let under = prefixes.iter().any(|p| under_prefix(&cwd, p));
        if under && ppid_of(pid) == 1 {
            let _ = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
            killed += 1;
        }
    }
    if killed > 0 {
        std::thread::sleep(Duration::from_millis(500));
        eprintln!("purged {killed} stray layer process(es)");
    }
}

// setup
pub fn ensure_ext_bins() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let exts = exts_root();
        let pkgs: [(&str, &str, &str); 5] = [
            ("ui_extensions/goal", "debug", "goal-ext"),
            ("ui_extensions/mermaid", "debug", "mermaid-ext"),
            ("ext-rs/statusline-rs", "release", "statusline-ext"),
            ("ext-rs/tool_result-rs", "release", "tool_result-ext"),
            ("ext-rs/notify-rs", "release", "notify-ext"),
        ];
        for (rel, profile, bin) in pkgs {
            let dir = exts.join(rel);
            if !dir.is_dir() {
                continue;
            }
            let bin_path = dir.join(format!("target/{profile}/{bin}"));
            if bin_path.is_file() {
                continue;
            }
            let mut cmd = Command::new("cargo");
            cmd.arg("build").current_dir(&dir);
            if profile == "release" {
                cmd.arg("--release");
            }
            let out = cmd
                .output()
                .unwrap_or_else(|e| panic!("cargo in {dir:?}: {e}"));
            if !out.status.success() {
                panic!(
                    "ext build failed in {dir:?}\n{}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
        }
    });
}

// config helpers
pub fn layer_cfg(layer: &Path, active_model: Option<&str>) -> (PathBuf, PathBuf) {
    let tmp = tmpdir();
    let sessions = tmp.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let path = tmp.join("config.toml");
    let mut s = String::new();
    s.push_str("[paths]\n");
    s.push_str(&format!("sessions_root = \"{}\"\n\n", sessions.display()));
    s.push_str("[tui]\n");
    s.push_str(&format!("ext_dirs = [\"{}\"]\n", layer.display()));
    if let Some(m) = active_model {
        s.push_str(&format!("\n[active]\nmodel = \"{m}\"\n"));
    }
    std::fs::write(&path, s).unwrap();
    (path, sessions)
}

pub fn seed_session(sessions: &Path, name: &str, events: &[String]) {
    let d = sessions.join(name);
    std::fs::create_dir_all(&d).unwrap();
    let mut s = String::new();
    for e in events {
        s.push_str(e);
        s.push('\n');
    }
    std::fs::write(d.join("events.jsonl"), s).unwrap();
}

pub fn seed_events() -> Vec<String> {
    let ts = "2026-08-27T10:00:00Z";
    let evs = [
        serde_json::json!({"v": 1, "type": "user_message", "ts": ts, "content": "check the build"}),
        serde_json::json!({
            "v": 1,
            "type": "assistant_message",
            "ts": ts,
            "content": "Running the build now.",
            "tool_calls": [
                {"id": "call_1", "name": "bash", "arguments": {"command": "make"}}
            ],
            "stop_reason": "tool_calls",
            "usage": {"input_tokens": 100, "output_tokens": 50},
        }),
        serde_json::json!({
            "v": 1,
            "type": "tool_call",
            "ts": ts,
            "id": "call_1",
            "name": "bash",
            "arguments": {"command": "make"},
        }),
        serde_json::json!({
            "v": 1,
            "type": "tool_result",
            "ts": ts,
            "id": "call_1",
            "value": {"text": "all good"},
            "is_error": false,
        }),
        serde_json::json!({
            "v": 1,
            "type": "assistant_message",
            "ts": ts,
            "content": "Build finished without errors.",
            "tool_calls": [],
            "stop_reason": "stop",
            "usage": {"input_tokens": 25, "output_tokens": 5},
        }),
    ];
    evs.iter().map(|v| v.to_string()).collect()
}

pub fn seed_goal(sessions: &Path, name: &str) {
    let d = sessions.join(name);
    std::fs::create_dir_all(&d).unwrap();
    let opened = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
        - 154;
    let goal_state = serde_json::json!({
        "id": "g-smoke0001",
        "goal": "shrink the smoke pty",
        "active": true,
        "used_tokens": 12400,
        "opened_at": format!("t+{opened}"),
    })
    .to_string();
    std::fs::write(d.join("goal-g-smoke0001.json"), goal_state).unwrap();
    let current = serde_json::json!({"current_goal": "g-smoke0001"}).to_string();
    std::fs::write(d.join("goal.json"), current).unwrap();
}

pub fn tool_result_only_layer(tmp: &Path, src: &Path) -> PathBuf {
    let layer = tmp.join("tr-layer");
    let d = layer.join("tool_result");
    std::fs::create_dir_all(&d).unwrap();
    for name in ["ext.toml", "tool_result.sh"] {
        let data = std::fs::read(src.join(name)).unwrap();
        std::fs::write(d.join(name), data).unwrap();
    }
    use std::os::unix::fs::PermissionsExt;
    let sh = d.join("tool_result.sh");
    std::fs::set_permissions(&sh, std::fs::Permissions::from_mode(0o755)).unwrap();
    layer
}

pub fn goal_only_layer(tmp: &Path, goal: &Path) -> PathBuf {
    let layer = tmp.join("goal-layer");
    std::fs::create_dir_all(&layer).unwrap();
    std::os::unix::fs::symlink(goal, layer.join("goal")).unwrap();
    layer
}

pub fn empty_layer(tmp: &Path) -> PathBuf {
    let d = tmp.join("bare-layer");
    std::fs::create_dir_all(&d).unwrap();
    d
}

pub fn repo_config_with_ext_dir(ext_dir: &Path) -> PathBuf {
    let repo = repo_root();
    let src = repo.join("config.toml");
    let content = std::fs::read_to_string(&src).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    let ext_line = format!("ext_dirs = [\"{}\"]", ext_dir.display());
    let tui_start = lines
        .iter()
        .position(|l| l.trim() == "[tui]")
        .expect("no [tui] section in config.toml");
    let mut tui_end = lines.len();
    for (j, l) in lines.iter().enumerate().skip(tui_start + 1) {
        if l.trim_start().starts_with('[') {
            tui_end = j;
            break;
        }
    }
    let mut out: Vec<String> = Vec::new();
    for l in &lines[..tui_start] {
        out.push(l.to_string());
    }
    out.push(String::from("[tui]"));
    let mut replaced = false;
    let mut i = tui_start + 1;
    while i < tui_end {
        let line = &lines[i];
        if !replaced && line.trim_start().starts_with("ext_dirs") {
            let mut depth = 0i32;
            let mut j = i;
            loop {
                if j >= tui_end {
                    break;
                }
                depth += lines[j].bytes().filter(|&b| b == b'[').count() as i32;
                depth -= lines[j].bytes().filter(|&b| b == b']').count() as i32;
                j += 1;
                if depth <= 0 {
                    break;
                }
            }
            out.push(ext_line.clone());
            i = j;
            replaced = true;
            continue;
        }
        out.push(line.to_string());
        i += 1;
    }
    if !replaced {
        out.push(ext_line);
    }
    for l in &lines[tui_end..] {
        out.push(l.to_string());
    }
    let path = repo.join(".ext-pty-smoke-cfg.toml");
    std::fs::write(&path, out.join("\n")).unwrap();
    path
}

pub fn active_model_from_config(path: &Path) -> Option<String> {
    let s = std::fs::read_to_string(path).ok()?;
    let mut in_active = false;
    for line in s.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_active = t == "[active]";
            continue;
        }
        if in_active && t.starts_with("model") {
            let v = t.split('=').nth(1)?.trim().trim_matches('"').to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

pub fn git_branch(repo: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("branch")
        .arg("--show-current")
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

pub fn system_git() -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    for d in std::env::split_paths(&paths) {
        if d.as_os_str().is_empty() {
            continue;
        }
        let p = d.join("git");
        if p.is_file() {
            return Some(p);
        }
    }
    None
}
