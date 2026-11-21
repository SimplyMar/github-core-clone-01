use rustc_hash::{FxHashMap, FxHashSet};
use sha1::{Digest, Sha1};
use std::collections::VecDeque;
use std::sync::Arc;

pub type Oid = [u8; 20];
pub const ZERO_OID: Oid = [0u8; 20];
pub const ZERO_OID_HEX: &str = "0000000000000000000000000000000000000000";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitObjectKind {
    Commit = 1,
    Tree = 2,
    Blob = 3,
    Tag = 4,
}

impl GitObjectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            GitObjectKind::Commit => "commit",
            GitObjectKind::Tree => "tree",
            GitObjectKind::Blob => "blob",
            GitObjectKind::Tag => "tag",
        }
    }

    pub fn from_num(n: u8) -> Option<Self> {
        match n {
            1 => Some(GitObjectKind::Commit),
            2 => Some(GitObjectKind::Tree),
            3 => Some(GitObjectKind::Blob),
            4 => Some(GitObjectKind::Tag),
            _ => None,
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "commit" => Some(GitObjectKind::Commit),
            "tree" => Some(GitObjectKind::Tree),
            "blob" => Some(GitObjectKind::Blob),
            "tag" => Some(GitObjectKind::Tag),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CommitMeta {
    pub tree: Oid,
    pub parents: Vec<Oid>,
}

#[derive(Clone, Debug)]
pub struct GitObject {
    pub kind: GitObjectKind,
    pub data: Vec<u8>,
    pub raw_zlib: Option<Vec<u8>>,
    pub commit_meta: Option<CommitMeta>,
}

#[derive(Clone, Debug)]
pub struct TreeEntry {
    pub mode: String,
    pub name: String,
    pub oid: Oid,
}

#[derive(Clone, Debug)]
pub struct ParsedCommit {
    pub tree: Oid,
    pub parents: Vec<Oid>,
    pub author: String,
    pub committer: String,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct ParsedTag {
    pub object: Oid,
    pub kind: GitObjectKind,
    pub tag: String,
    pub tagger: String,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct Label {
    pub id: u64,
    pub node_id: String,
    pub name: String,
    pub color: String,
    pub description: String,
}

#[derive(Clone, Debug)]
pub struct IssueComment {
    pub id: u64,
    pub node_id: String,
    pub body: String,
    pub user: UserObj,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug)]
pub struct Issue {
    pub id: u64,
    pub node_id: String,
    pub number: u64,
    pub title: String,
    pub body: String,
    pub state: String,
    pub labels: Vec<Label>,
    pub user: Arc<UserObj>,
    pub comments: Vec<IssueComment>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    pub created_seq: u64,
    pub cached_http_get: Option<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub struct PullRequest {
    pub id: u64,
    pub node_id: String,
    pub number: u64,
    pub title: String,
    pub body: String,
    pub state: String,
    pub head_ref: String,
    pub head_sha: Oid,
    pub base_ref: String,
    pub base_sha: Oid,
    pub merged: bool,
    pub merged_at: Option<String>,
    pub merge_commit_sha: Option<Oid>,
    pub user: UserObj,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
}

#[derive(Clone, Debug)]
pub struct UserObj {
    pub id: u64,
    pub node_id: String,
    pub login: String,
    pub name: String,
    pub email: String,
}

pub struct Repo {
    pub id: u64,
    pub node_id: String,
    pub name: String,
    pub owner: UserObj,
    pub full_name: String,
    pub description: String,
    pub private: bool,
    pub default_branch: String,
    pub has_issues: bool,
    pub created_at: String,
    pub updated_at: String,
    pub objects: FxHashMap<Oid, GitObject>,
    pub refs: FxHashMap<String, Oid>,
    pub labels: Vec<Label>,
    pub label_index: FxHashMap<String, usize>,
    pub issues: Vec<Issue>,
    pub pulls: Vec<PullRequest>,
    pub next_issue_number: u64,
    pub next_pull_number: u64,
    pub receive_refs_http_cache: Option<Vec<u8>>,
    pub ls_refs_http_cache: FxHashMap<Vec<u8>, Vec<u8>>,
    pub fetch_http_cache: FxHashMap<Vec<u8>, Vec<u8>>,
}

impl Repo {
    pub fn invalidate_git_caches(&mut self) {
        self.receive_refs_http_cache = None;
        self.ls_refs_http_cache.clear();
        self.fetch_http_cache.clear();
    }

    pub fn get_issue(&self, number: u64) -> Option<&Issue> {
        if number == 0 {
            return None;
        }
        let idx = (number - 1) as usize;
        self.issues.get(idx).filter(|i| i.number == number)
    }

    pub fn get_issue_mut(&mut self, number: u64) -> Option<&mut Issue> {
        if number == 0 {
            return None;
        }
        let idx = (number - 1) as usize;
        self.issues.get_mut(idx).filter(|i| i.number == number)
    }

    pub fn get_pull(&self, number: u64) -> Option<&PullRequest> {
        if number == 0 {
            return None;
        }
        let idx = (number - 1) as usize;
        self.pulls.get(idx).filter(|p| p.number == number)
    }

    pub fn get_pull_mut(&mut self, number: u64) -> Option<&mut PullRequest> {
        if number == 0 {
            return None;
        }
        let idx = (number - 1) as usize;
        self.pulls.get_mut(idx).filter(|p| p.number == number)
    }
}

pub struct DiffFile {
    pub filename: String,
    pub status: &'static str,
    pub additions: usize,
    pub deletions: usize,
    pub changes: usize,
}

pub struct ForgeStore {
    pub token: String,
    pub bearer_expected: Vec<u8>,
    pub token_expected: Vec<u8>,
    pub now_iso: String,
    pub epoch_sec: i64,
    pub viewer: Arc<UserObj>,
    pub repos: Vec<Repo>,
    pub repo_by_name: FxHashMap<String, usize>,
    pub repo_by_id: FxHashMap<u64, usize>,
    pub issue_by_id: FxHashMap<u64, (usize, u64)>,
    pub pull_by_id: FxHashMap<u64, (usize, u64)>,
    pub deleted_repos: FxHashSet<usize>,
    next_repo_id: u64,
    next_label_id: u64,
    next_issue_id: u64,
    next_pull_id: u64,
    next_comment_id: u64,
    seq: u64,
}

const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";

#[inline]
pub fn oid_to_hex(oid: &Oid) -> String {
    let mut out = vec![0u8; 40];
    write_oid_hex(oid, &mut out);
    unsafe { String::from_utf8_unchecked(out) }
}

#[inline]
pub fn write_oid_hex(oid: &Oid, dst: &mut [u8]) {
    for i in 0..20 {
        let b = oid[i];
        dst[i * 2] = HEX_CHARS[(b >> 4) as usize];
        dst[i * 2 + 1] = HEX_CHARS[(b & 0x0f) as usize];
    }
}

#[inline]
fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[inline]
pub fn hex_to_oid(s: &[u8]) -> Option<Oid> {
    if s.len() != 40 {
        return None;
    }
    let mut out = [0u8; 20];
    for i in 0..20 {
        let hi = hex_val(s[i * 2])?;
        let lo = hex_val(s[i * 2 + 1])?;
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

#[inline]
pub fn hash_object(kind: GitObjectKind, data: &[u8]) -> Oid {
    let mut hasher = Sha1::new();
    let mut hdr_buf = [0u8; 32];
    let prefix = kind.as_str().as_bytes();
    let mut idx = 0;
    hdr_buf[..prefix.len()].copy_from_slice(prefix);
    idx += prefix.len();
    hdr_buf[idx] = b' ';
    idx += 1;
    let mut len_str = [0u8; 20];
    let len_bytes = fmt_usize(data.len(), &mut len_str);
    hdr_buf[idx..idx + len_bytes.len()].copy_from_slice(len_bytes);
    idx += len_bytes.len();
    hdr_buf[idx] = 0;
    idx += 1;
    hasher.update(&hdr_buf[..idx]);
    hasher.update(data);
    let digest = hasher.finalize();
    let mut oid = [0u8; 20];
    oid.copy_from_slice(&digest);
    oid
}

#[inline]
pub fn fmt_usize(mut n: usize, buf: &mut [u8; 20]) -> &[u8] {
    if n == 0 {
        buf[19] = b'0';
        return &buf[19..];
    }
    let mut pos = 20;
    while n > 0 {
        pos -= 1;
        buf[pos] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    &buf[pos..]
}

#[inline]
pub fn parse_commit_meta(data: &[u8]) -> CommitMeta {
    let mut tree = ZERO_OID;
    let mut parents = Vec::with_capacity(2);
    let mut pos = 0;
    while pos < data.len() {
        let nl = data[pos..].iter().position(|&b| b == b'\n').map(|i| pos + i).unwrap_or(data.len());
        let line = &data[pos..nl];
        if line.is_empty() {
            break;
        }
        if line.len() >= 45 && &line[..5] == b"tree " {
            if let Some(o) = hex_to_oid(&line[5..45]) {
                tree = o;
            }
        } else if line.len() >= 47 && &line[..7] == b"parent " {
            if let Some(o) = hex_to_oid(&line[7..47]) {
                parents.push(o);
            }
        }
        pos = nl + 1;
    }
    CommitMeta { tree, parents }
}

pub fn parse_commit(data: &[u8]) -> ParsedCommit {
    let text = String::from_utf8_lossy(data);
    let (header_part, message) = match text.find("\n\n") {
        Some(idx) => (&text[..idx], text[idx + 2..].to_string()),
        None => (text.as_ref(), String::new()),
    };
    let mut tree = ZERO_OID;
    let mut parents = Vec::new();
    let mut author = String::new();
    let mut committer = String::new();
    for line in header_part.split('\n') {
        if let Some(rest) = line.strip_prefix("tree ") {
            if let Some(o) = hex_to_oid(rest.trim().as_bytes()) {
                tree = o;
            }
        } else if let Some(rest) = line.strip_prefix("parent ") {
            if let Some(o) = hex_to_oid(rest.trim().as_bytes()) {
                parents.push(o);
            }
        } else if let Some(rest) = line.strip_prefix("author ") {
            author = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("committer ") {
            committer = rest.to_string();
        }
    }
    ParsedCommit { tree, parents, author, committer, message }
}

pub fn parse_tag(data: &[u8]) -> ParsedTag {
    let text = String::from_utf8_lossy(data);
    let (header_part, message) = match text.find("\n\n") {
        Some(idx) => (&text[..idx], text[idx + 2..].to_string()),
        None => (text.as_ref(), String::new()),
    };
    let mut object = ZERO_OID;
    let mut kind = GitObjectKind::Commit;
    let mut tag = String::new();
    let mut tagger = String::new();
    for line in header_part.split('\n') {
        if let Some(rest) = line.strip_prefix("object ") {
            if let Some(o) = hex_to_oid(rest.trim().as_bytes()) {
                object = o;
            }
        } else if let Some(rest) = line.strip_prefix("type ") {
            if let Some(k) = GitObjectKind::from_str(rest.trim()) {
                kind = k;
            }
        } else if let Some(rest) = line.strip_prefix("tag ") {
            tag = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("tagger ") {
            tagger = rest.to_string();
        }
    }
    ParsedTag { object, kind, tag, tagger, message }
}

pub fn build_tree(mut entries: Vec<TreeEntry>) -> Vec<u8> {
    entries.sort_by(|a, b| {
        let a_dir = a.mode == "40000" || a.mode == "040000";
        let b_dir = b.mode == "40000" || b.mode == "040000";
        let ak = if a_dir { format!("{}/", a.name) } else { a.name.clone() };
        let bk = if b_dir { format!("{}/", b.name) } else { b.name.clone() };
        ak.as_bytes().cmp(bk.as_bytes())
    });
    let mut out = Vec::with_capacity(entries.len() * 48);
    for entry in entries {
        let mode = if entry.mode == "040000" { "40000" } else { entry.mode.as_str() };
        out.extend_from_slice(mode.as_bytes());
        out.push(b' ');
        out.extend_from_slice(entry.name.as_bytes());
        out.push(0);
        out.extend_from_slice(&entry.oid);
    }
    out
}

pub fn parse_tree(data: &[u8]) -> Vec<TreeEntry> {
    let mut entries = Vec::new();
    let mut off = 0;
    while off < data.len() {
        let sp = match data[off..].iter().position(|&b| b == b' ') {
            Some(i) => off + i,
            None => break,
        };
        let mode = String::from_utf8_lossy(&data[off..sp]).into_owned();
        let nul = match data[sp + 1..].iter().position(|&b| b == 0) {
            Some(i) => sp + 1 + i,
            None => break,
        };
        if nul + 21 > data.len() {
            break;
        }
        let name = String::from_utf8_lossy(&data[sp + 1..nul]).into_owned();
        let mut oid = [0u8; 20];
        oid.copy_from_slice(&data[nul + 1..nul + 21]);
        entries.push(TreeEntry { mode, name, oid });
        off = nul + 21;
    }
    entries
}

pub fn build_commit(
    tree: &Oid,
    parents: &[Oid],
    author: &str,
    committer: &str,
    timestamp: i64,
    message: &str,
) -> Vec<u8> {
    let mut s = String::with_capacity(256 + message.len());
    s.push_str("tree ");
    s.push_str(&oid_to_hex(tree));
    s.push('\n');
    for p in parents {
        s.push_str("parent ");
        s.push_str(&oid_to_hex(p));
        s.push('\n');
    }
    s.push_str("author ");
    s.push_str(author);
    s.push(' ');
    s.push_str(&timestamp.to_string());
    s.push_str(" +0000\ncommitter ");
    s.push_str(committer);
    s.push(' ');
    s.push_str(&timestamp.to_string());
    s.push_str(" +0000\n\n");
    s.push_str(message);
    if !message.ends_with('\n') {
        s.push('\n');
    }
    s.into_bytes()
}

pub fn format_iso8601(epoch_sec: i64) -> String {
    let days = epoch_sec.div_euclid(86400);
    let rem = epoch_sec.rem_euclid(86400);
    let hour = rem / 3600;
    let min = (rem % 3600) / 60;
    let sec = rem % 60;
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.000Z", year, m, d, hour, min, sec)
}

pub fn parse_iso8601_epoch(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let y: i64 = s.get(0..4)?.parse().ok()?;
    let m: i64 = s.get(5..7)?.parse().ok()?;
    let d: i64 = s.get(8..10)?.parse().ok()?;
    let hh: i64 = s.get(11..13)?.parse().ok()?;
    let mm: i64 = s.get(14..16)?.parse().ok()?;
    let ss: i64 = s.get(17..19)?.parse().ok()?;
    let y_adj = if m <= 2 { y - 1 } else { y };
    let era = if y_adj >= 0 { y_adj } else { y_adj - 399 } / 400;
    let yoe = (y_adj - era * 400) as u64;
    let mp = if m > 2 { (m - 3) as u64 } else { (m + 9) as u64 };
    let doy = (153 * mp + 2) / 5 + (d as u64) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + (doe as i64) - 719468;
    Some(days * 86400 + hh * 3600 + mm * 60 + ss)
}

const B64_STD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const B64_URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

pub fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    let mut i = 0;
    while i < input.len() {
        let b0 = input[i] as u32;
        let b1 = if i + 1 < input.len() { input[i + 1] as u32 } else { 0 };
        let b2 = if i + 2 < input.len() { input[i + 2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64_STD[((n >> 18) & 63) as usize] as char);
        out.push(B64_STD[((n >> 12) & 63) as usize] as char);
        if i + 1 < input.len() {
            out.push(B64_STD[((n >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if i + 2 < input.len() {
            out.push(B64_STD[(n & 63) as usize] as char);
        } else {
            out.push('=');
        }
        i += 3;
    }
    out
}

pub fn base64url_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    let mut i = 0;
    while i < input.len() {
        let b0 = input[i] as u32;
        let b1 = if i + 1 < input.len() { input[i + 1] as u32 } else { 0 };
        let b2 = if i + 2 < input.len() { input[i + 2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64_URL[((n >> 18) & 63) as usize] as char);
        out.push(B64_URL[((n >> 12) & 63) as usize] as char);
        if i + 1 < input.len() {
            out.push(B64_URL[((n >> 6) & 63) as usize] as char);
        }
        if i + 2 < input.len() {
            out.push(B64_URL[(n & 63) as usize] as char);
        }
        i += 3;
    }
    out
}

pub fn base64_decode(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits: u8 = 0;
    for &b in input {
        let val = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => continue,
        };
        buf = (buf << 6) | (val as u32);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    out
}

pub fn encode_node_id(prefix: &str, id: u64) -> String {
    let raw = format!("{}:{}", prefix, id);
    base64url_encode(raw.as_bytes())
}

pub fn decode_node_id(node_id: &str) -> Option<(String, u64)> {
    let raw_bytes = base64_decode(node_id.as_bytes());
    let raw = String::from_utf8(raw_bytes).ok()?;
    let idx = raw.find(':')?;
    if idx == 0 {
        return None;
    }
    let id = raw[idx + 1..].parse::<u64>().ok()?;
    Some((raw[..idx].to_string(), id))
}

fn count_lines(data: &[u8]) -> usize {
    if data.is_empty() {
        return 0;
    }
    let mut n = data.iter().filter(|&&b| b == b'\n').count();
    if *data.last().unwrap() != b'\n' {
        n += 1;
    }
    n
}

impl ForgeStore {
    pub fn new() -> Self {
        let token = std::env::var("MAKEFUN_SHOP_TOKEN").unwrap_or_else(|_| "dev-token".to_string());
        let raw_now = std::env::var("MAKEFUN_TRIAL_NOW")
            .unwrap_or_else(|_| "2026-11-14T00:00:00.000Z".to_string());
        let epoch_sec = parse_iso8601_epoch(&raw_now).unwrap_or(1700000000);
        let now_iso = format_iso8601(epoch_sec);
        let login = std::env::var("MAKEFUN_FORGE_LOGIN")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "adaiversen".to_string());
        let viewer = Arc::new(UserObj {
            id: 1000,
            node_id: encode_node_id("U", 1000),
            login,
            name: "ada iversen".to_string(),
            email: "adaiversen@example.invalid".to_string(),
        });
        let bearer_expected = format!("Bearer {}", token).into_bytes();
        let token_expected = format!("token {}", token).into_bytes();
        Self {
            token,
            bearer_expected,
            token_expected,
            now_iso,
            epoch_sec,
            viewer,
            repos: Vec::with_capacity(64),
            repo_by_name: FxHashMap::default(),
            repo_by_id: FxHashMap::default(),
            issue_by_id: FxHashMap::default(),
            pull_by_id: FxHashMap::default(),
            deleted_repos: FxHashSet::default(),
            next_repo_id: 100,
            next_label_id: 1000,
            next_issue_id: 10000,
            next_pull_id: 20000,
            next_comment_id: 30000,
            seq: 0,
        }
    }

    #[inline]
    pub fn is_authorized(&self, auth: Option<&[u8]>) -> bool {
        let raw = match auth {
            Some(a) => a,
            None => return false,
        };
        if raw == self.bearer_expected.as_slice() || raw == self.token_expected.as_slice() {
            return true;
        }
        let s = match std::str::from_utf8(raw) {
            Ok(v) => v.trim(),
            Err(_) => return false,
        };
        let mut it = s.split_whitespace();
        let scheme = match it.next() {
            Some(v) => v,
            None => return false,
        };
        let tok = match it.next() {
            Some(v) => v,
            None => return false,
        };
        if it.next().is_some() {
            return false;
        }
        (scheme.eq_ignore_ascii_case("bearer") || scheme.eq_ignore_ascii_case("token"))
            && tok == self.token
    }

    #[inline]
    pub fn next_timestamp(&mut self) -> String {
        self.seq += 1;
        format_iso8601(self.epoch_sec + self.seq as i64)
    }

    #[inline]
    pub fn get_repo_idx(&self, owner: &str, name: &str) -> Option<usize> {
        if owner != self.viewer.login {
            return None;
        }
        self.repo_by_name.get(name).copied()
    }

    #[inline]
    pub fn get_repo(&self, owner: &str, name: &str) -> Option<&Repo> {
        let idx = self.get_repo_idx(owner, name)?;
        self.repos.get(idx)
    }

    #[inline]
    pub fn get_repo_mut(&mut self, owner: &str, name: &str) -> Option<&mut Repo> {
        let idx = self.get_repo_idx(owner, name)?;
        self.repos.get_mut(idx)
    }

    pub fn create_repo(
        &mut self,
        name: String,
        description: String,
        private: bool,
        default_branch: String,
        has_issues: bool,
    ) -> usize {
        self.next_repo_id += 1;
        let id = self.next_repo_id;
        let full_name = format!("{}/{}", self.viewer.login, name);
        let repo = Repo {
            id,
            node_id: encode_node_id("R", id),
            name: name.clone(),
            owner: (*self.viewer).clone(),
            full_name,
            description,
            private,
            default_branch: if default_branch.is_empty() {
                "main".to_string()
            } else {
                default_branch
            },
            has_issues,
            created_at: self.now_iso.clone(),
            updated_at: self.now_iso.clone(),
            objects: FxHashMap::default(),
            refs: FxHashMap::default(),
            labels: Vec::new(),
            label_index: FxHashMap::default(),
            issues: Vec::new(),
            pulls: Vec::new(),
            next_issue_number: 1,
            next_pull_number: 1,
            receive_refs_http_cache: None,
            ls_refs_http_cache: FxHashMap::default(),
            fetch_http_cache: FxHashMap::default(),
        };
        let idx = self.repos.len();
        self.repos.push(repo);
        self.repo_by_name.insert(name, idx);
        self.repo_by_id.insert(id, idx);
        idx
    }

    pub fn delete_repo(&mut self, owner: &str, name: &str) -> bool {
        let idx = match self.get_repo_idx(owner, name) {
            Some(i) => i,
            None => return false,
        };
        let id = self.repos[idx].id;
        self.repo_by_name.remove(name);
        self.repo_by_id.remove(&id);
        self.deleted_repos.insert(idx);
        true
    }

    pub fn active_repos(&self) -> Vec<&Repo> {
        self.repos
            .iter()
            .enumerate()
            .filter(|(i, _)| !self.deleted_repos.contains(i))
            .map(|(_, r)| r)
            .collect()
    }

    pub fn put_object(repo: &mut Repo, kind: GitObjectKind, data: Vec<u8>, raw_zlib: Option<Vec<u8>>) -> Oid {
        let oid = hash_object(kind, &data);
        if !repo.objects.contains_key(&oid) {
            let commit_meta = if kind == GitObjectKind::Commit {
                Some(parse_commit_meta(&data))
            } else {
                None
            };
            repo.objects.insert(
                oid,
                GitObject {
                    kind,
                    data,
                    raw_zlib,
                    commit_meta,
                },
            );
        }
        oid
    }

    pub fn set_ref(repo: &mut Repo, ref_name: &str, oid: Option<Oid>) {
        repo.invalidate_git_caches();
        match oid {
            None => {
                repo.refs.remove(ref_name);
            }
            Some(o) if o == ZERO_OID => {
                repo.refs.remove(ref_name);
            }
            Some(o) => {
                repo.refs.insert(ref_name.to_string(), o);
            }
        }
    }

    pub fn resolve_ref(repo: &Repo, ref_or_sha: Option<&str>) -> Option<Oid> {
        let target = match ref_or_sha {
            Some(s) if !s.is_empty() => s,
            _ => repo.default_branch.as_str(),
        };
        if let Some(oid) = hex_to_oid(target.as_bytes()) {
            if repo.objects.contains_key(&oid) {
                return Some(oid);
            }
        }
        if target.starts_with("refs/") {
            return repo.refs.get(target).copied();
        }
        let head_ref = format!("refs/heads/{}", target);
        if let Some(o) = repo.refs.get(&head_ref) {
            return Some(*o);
        }
        let tag_ref = format!("refs/tags/{}", target);
        repo.refs.get(&tag_ref).copied()
    }

    pub fn tree_of_commit(repo: &Repo, commit_sha: Option<Oid>) -> Option<Oid> {
        let sha = commit_sha?;
        let obj = repo.objects.get(&sha)?;
        if obj.kind != GitObjectKind::Commit {
            return None;
        }
        if let Some(meta) = &obj.commit_meta {
            return Some(meta.tree);
        }
        Some(parse_commit(&obj.data).tree)
    }

    pub fn flatten_tree(
        repo: &Repo,
        tree_oid: Option<Oid>,
        prefix: &str,
        out: &mut FxHashMap<String, (Oid, String, Vec<u8>)>,
    ) {
        let oid = match tree_oid {
            Some(o) => o,
            None => return,
        };
        let obj = match repo.objects.get(&oid) {
            Some(o) if o.kind == GitObjectKind::Tree => o,
            _ => return,
        };
        let entries = parse_tree(&obj.data);
        for entry in entries {
            let p = if prefix.is_empty() {
                entry.name.clone()
            } else {
                format!("{}/{}", prefix, entry.name)
            };
            if entry.mode == "40000" || entry.mode == "040000" {
                Self::flatten_tree(repo, Some(entry.oid), &p, out);
            } else if let Some(blob) = repo.objects.get(&entry.oid) {
                if blob.kind == GitObjectKind::Blob {
                    out.insert(p, (entry.oid, entry.mode, blob.data.clone()));
                }
            }
        }
    }

    pub fn build_tree_from_flat_map(
        repo: &mut Repo,
        files: &FxHashMap<String, (Oid, String)>,
    ) -> Oid {
        struct DirNode {
            files: FxHashMap<String, (Oid, String)>,
            dirs: FxHashMap<String, DirNode>,
        }
        impl DirNode {
            fn new() -> Self {
                Self {
                    files: FxHashMap::default(),
                    dirs: FxHashMap::default(),
                }
            }
        }
        fn write_dir(repo: &mut Repo, node: DirNode) -> Oid {
            let mut entries = Vec::with_capacity(node.files.len() + node.dirs.len());
            for (name, (oid, mode)) in node.files {
                entries.push(TreeEntry { mode, name, oid });
            }
            for (name, child) in node.dirs {
                let child_oid = write_dir(repo, child);
                entries.push(TreeEntry {
                    mode: "40000".to_string(),
                    name,
                    oid: child_oid,
                });
            }
            let raw = build_tree(entries);
            ForgeStore::put_object(repo, GitObjectKind::Tree, raw, None)
        }
        let mut root = DirNode::new();
        for (file_path, info) in files {
            let parts: Vec<&str> = file_path.split('/').filter(|s| !s.is_empty()).collect();
            if parts.is_empty() {
                continue;
            }
            let mut cur = &mut root;
            for &part in &parts[..parts.len() - 1] {
                cur = cur
                    .dirs
                    .entry(part.to_string())
                    .or_insert_with(DirNode::new);
            }
            cur.files
                .insert(parts[parts.len() - 1].to_string(), info.clone());
        }
        write_dir(repo, root)
    }

    pub fn commit_file_change(
        &mut self,
        repo_idx: usize,
        branch: &str,
        path: &str,
        content: Option<Vec<u8>>,
        message: &str,
    ) -> (Oid, Option<Oid>) {
        let ref_name = if branch.starts_with("refs/") {
            branch.to_string()
        } else {
            format!("refs/heads/{}", branch)
        };
        let author = format!("{} <{}>", self.viewer.name, self.viewer.email);
        self.seq += 1;
        let ts = self.epoch_sec + self.seq as i64;
        let repo = &mut self.repos[repo_idx];
        let parent_sha = repo.refs.get(&ref_name).copied();
        let mut flat: FxHashMap<String, (Oid, String)> = FxHashMap::default();
        if let Some(psha) = parent_sha {
            let tree = Self::tree_of_commit(repo, Some(psha));
            let mut existing = FxHashMap::default();
            Self::flatten_tree(repo, tree, "", &mut existing);
            for (p, (oid, mode, _)) in existing {
                flat.insert(p, (oid, mode));
            }
        }
        let mut blob_sha = None;
        if let Some(bytes) = content {
            let oid = Self::put_object(repo, GitObjectKind::Blob, bytes, None);
            blob_sha = Some(oid);
            flat.insert(path.to_string(), (oid, "100644".to_string()));
        } else {
            flat.remove(path);
        }
        let tree_oid = Self::build_tree_from_flat_map(repo, &flat);
        let parents = match parent_sha {
            Some(p) => vec![p],
            None => vec![],
        };
        let commit_data = build_commit(&tree_oid, &parents, &author, &author, ts, message);
        let commit_sha = Self::put_object(repo, GitObjectKind::Commit, commit_data, None);
        Self::set_ref(repo, &ref_name, Some(commit_sha));
        (commit_sha, blob_sha)
    }

    #[inline]
    pub fn is_ancestor(repo: &Repo, anc_oid: Oid, desc_oid: Oid) -> bool {
        if anc_oid == desc_oid {
            return true;
        }
        if let Some(obj) = repo.objects.get(&desc_oid) {
            if let Some(meta) = &obj.commit_meta {
                if meta.parents.contains(&anc_oid) {
                    return true;
                }
            }
        }
        let mut seen = FxHashSet::default();
        let mut stack = vec![desc_oid];
        while let Some(cur) = stack.pop() {
            if cur == anc_oid {
                return true;
            }
            if !seen.insert(cur) {
                continue;
            }
            if let Some(obj) = repo.objects.get(&cur) {
                if obj.kind == GitObjectKind::Commit {
                    if let Some(meta) = &obj.commit_meta {
                        for &p in &meta.parents {
                            stack.push(p);
                        }
                    } else {
                        for p in parse_commit(&obj.data).parents {
                            stack.push(p);
                        }
                    }
                }
            }
        }
        false
    }

    fn ancestors_of(repo: &Repo, start_oid: Oid) -> FxHashMap<Oid, usize> {
        let mut dist = FxHashMap::default();
        let mut q = VecDeque::new();
        q.push_back((start_oid, 0usize));
        while let Some((oid, d)) = q.pop_front() {
            if dist.contains_key(&oid) {
                continue;
            }
            dist.insert(oid, d);
            if let Some(obj) = repo.objects.get(&oid) {
                if obj.kind == GitObjectKind::Commit {
                    if let Some(meta) = &obj.commit_meta {
                        for &p in &meta.parents {
                            q.push_back((p, d + 1));
                        }
                    }
                }
            }
        }
        dist
    }

    pub fn find_merge_base(repo: &Repo, oid_a: Oid, oid_b: Oid) -> Option<Oid> {
        let a_dist = Self::ancestors_of(repo, oid_a);
        let b_dist = Self::ancestors_of(repo, oid_b);
        let mut best: Option<Oid> = None;
        let mut best_score = usize::MAX;
        for (oid, da) in a_dist {
            if let Some(&db) = b_dist.get(&oid) {
                if da + db < best_score {
                    best_score = da + db;
                    best = Some(oid);
                }
            }
        }
        best
    }

    pub fn diff_commits(repo: &Repo, base_commit: Oid, head_commit: Oid) -> Vec<DiffFile> {
        let lca = Self::find_merge_base(repo, base_commit, head_commit).unwrap_or(base_commit);
        let mut base_files = FxHashMap::default();
        let mut head_files = FxHashMap::default();
        Self::flatten_tree(repo, Self::tree_of_commit(repo, Some(lca)), "", &mut base_files);
        Self::flatten_tree(
            repo,
            Self::tree_of_commit(repo, Some(head_commit)),
            "",
            &mut head_files,
        );
        let mut all_paths: Vec<String> = base_files
            .keys()
            .chain(head_files.keys())
            .cloned()
            .collect();
        all_paths.sort();
        all_paths.dedup();
        let mut out = Vec::new();
        for p in all_paths {
            let b = base_files.get(&p);
            let h = head_files.get(&p);
            match (b, h) {
                (None, Some((_, _, hdata))) => {
                    let lines = count_lines(hdata);
                    out.push(DiffFile {
                        filename: p,
                        status: "added",
                        additions: lines,
                        deletions: 0,
                        changes: lines,
                    });
                }
                (Some((_, _, bdata)), None) => {
                    let lines = count_lines(bdata);
                    out.push(DiffFile {
                        filename: p,
                        status: "removed",
                        additions: 0,
                        deletions: lines,
                        changes: lines,
                    });
                }
                (Some((boid, _, bdata)), Some((hoid, _, hdata))) if boid != hoid => {
                    let b_lines = count_lines(bdata);
                    let h_lines = count_lines(hdata);
                    let additions = h_lines.max(1);
                    let deletions = b_lines.max(1);
                    out.push(DiffFile {
                        filename: p,
                        status: "modified",
                        additions,
                        deletions,
                        changes: additions + deletions,
                    });
                }
                _ => {}
            }
        }
        out
    }

    pub fn create_label(
        &mut self,
        repo_idx: usize,
        name: String,
        color: String,
        description: String,
    ) -> Label {
        if let Some(&lidx) = self.repos[repo_idx].label_index.get(&name) {
            return self.repos[repo_idx].labels[lidx].clone();
        }
        self.next_label_id += 1;
        let id = self.next_label_id;
        let label = Label {
            id,
            node_id: encode_node_id("LA", id),
            name: name.clone(),
            color,
            description,
        };
        let lidx = self.repos[repo_idx].labels.len();
        self.repos[repo_idx].labels.push(label.clone());
        self.repos[repo_idx].label_index.insert(name, lidx);
        label
    }

    pub fn create_issue(
        &mut self,
        repo_idx: usize,
        title: String,
        body: String,
        label_names: &[String],
    ) -> Result<u64, String> {
        let mut resolved_labels = Vec::with_capacity(label_names.len());
        for lname in label_names {
            match self.repos[repo_idx].label_index.get(lname) {
                Some(&lidx) => resolved_labels.push(self.repos[repo_idx].labels[lidx].clone()),
                None => return Err(lname.clone()),
            }
        }
        self.next_issue_id += 1;
        let id = self.next_issue_id;
        let now = self.next_timestamp();
        self.seq += 1;
        let created_seq = self.seq;
        let user = self.viewer.clone();
        let repo = &mut self.repos[repo_idx];
        let number = repo.next_issue_number;
        repo.next_issue_number += 1;
        let issue = Issue {
            id,
            node_id: encode_node_id("I", id),
            number,
            title,
            body,
            state: "open".to_string(),
            labels: resolved_labels,
            user,
            comments: Vec::new(),
            created_at: now.clone(),
            updated_at: now,
            closed_at: None,
            created_seq,
            cached_http_get: None,
        };
        repo.issues.push(issue);
        self.issue_by_id.insert(id, (repo_idx, number));
        Ok(number)
    }

    pub fn add_issue_comment(
        &mut self,
        repo_idx: usize,
        issue_number: u64,
        body: String,
    ) -> Option<IssueComment> {
        self.next_comment_id += 1;
        let id = self.next_comment_id;
        let now = self.next_timestamp();
        let user = (*self.viewer).clone();
        let issue = self.repos[repo_idx].get_issue_mut(issue_number)?;
        let comment = IssueComment {
            id,
            node_id: encode_node_id("IC", id),
            body,
            user,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        issue.comments.push(comment.clone());
        issue.updated_at = now;
        issue.cached_http_get = None;
        Some(comment)
    }

    pub fn create_pull(
        &mut self,
        repo_idx: usize,
        title: String,
        body: String,
        head: String,
        base: String,
    ) -> Result<u64, String> {
        let head_ref = head.strip_prefix("refs/heads/").unwrap_or(&head).to_string();
        let base_ref = base.strip_prefix("refs/heads/").unwrap_or(&base).to_string();
        let head_sha = match self.repos[repo_idx]
            .refs
            .get(&format!("refs/heads/{}", head_ref))
            .copied()
        {
            Some(s) => s,
            None => return Err(format!("Head ref {} does not exist", head_ref)),
        };
        let base_sha = match self.repos[repo_idx]
            .refs
            .get(&format!("refs/heads/{}", base_ref))
            .copied()
        {
            Some(s) => s,
            None => return Err(format!("Base ref {} does not exist", base_ref)),
        };
        self.next_pull_id += 1;
        let id = self.next_pull_id;
        let now = self.next_timestamp();
        let user = (*self.viewer).clone();
        let repo = &mut self.repos[repo_idx];
        let number = repo.next_pull_number;
        repo.next_pull_number += 1;
        let pull = PullRequest {
            id,
            node_id: encode_node_id("PR", id),
            number,
            title,
            body,
            state: "open".to_string(),
            head_ref,
            head_sha,
            base_ref,
            base_sha,
            merged: false,
            merged_at: None,
            merge_commit_sha: None,
            user,
            created_at: now.clone(),
            updated_at: now,
            closed_at: None,
        };
        repo.pulls.push(pull);
        self.pull_by_id.insert(id, (repo_idx, number));
        Ok(number)
    }

    pub fn squash_merge(
        &mut self,
        repo_idx: usize,
        pull_number: u64,
        custom_message: Option<&str>,
    ) -> Result<Oid, &'static str> {
        let (base_ref_name, base_tip, head_tip, default_msg) = {
            let repo = &self.repos[repo_idx];
            let pull = repo.get_pull(pull_number).ok_or("not_found")?;
            let base_ref = format!("refs/heads/{}", pull.base_ref);
            let head_ref = format!("refs/heads/{}", pull.head_ref);
            let b_tip = repo.refs.get(&base_ref).copied().unwrap_or(pull.base_sha);
            let h_tip = repo.refs.get(&head_ref).copied().unwrap_or(pull.head_sha);
            let msg = format!("{} (#{})", pull.title, pull.number);
            (base_ref, b_tip, h_tip, msg)
        };

        let merged_flat = {
            let repo = &self.repos[repo_idx];
            let lca = Self::find_merge_base(repo, base_tip, head_tip);
            let mut lca_files = FxHashMap::default();
            let mut base_files = FxHashMap::default();
            let mut head_files = FxHashMap::default();
            Self::flatten_tree(repo, Self::tree_of_commit(repo, lca), "", &mut lca_files);
            Self::flatten_tree(repo, Self::tree_of_commit(repo, Some(base_tip)), "", &mut base_files);
            Self::flatten_tree(repo, Self::tree_of_commit(repo, Some(head_tip)), "", &mut head_files);

            let mut all_paths: FxHashSet<String> = FxHashSet::default();
            for k in lca_files.keys().chain(base_files.keys()).chain(head_files.keys()) {
                all_paths.insert(k.clone());
            }
            let mut out: FxHashMap<String, (Oid, String)> = FxHashMap::default();
            for p in all_paths {
                let l_oid = lca_files.get(&p).map(|v| v.0);
                let b_entry = base_files.get(&p);
                let h_entry = head_files.get(&p);
                let b_oid = b_entry.map(|v| v.0);
                let h_oid = h_entry.map(|v| v.0);
                if b_oid == h_oid {
                    if let Some((oid, mode, _)) = b_entry {
                        out.insert(p, (*oid, mode.clone()));
                    }
                } else if b_oid == l_oid {
                    if let Some((oid, mode, _)) = h_entry {
                        out.insert(p, (*oid, mode.clone()));
                    }
                } else if h_oid == l_oid {
                    if let Some((oid, mode, _)) = b_entry {
                        out.insert(p, (*oid, mode.clone()));
                    }
                } else {
                    return Err("conflict");
                }
            }
            out
        };

        let author = format!("{} <{}>", self.viewer.name, self.viewer.email);
        self.seq += 1;
        let ts = self.epoch_sec + self.seq as i64;
        let msg = custom_message.unwrap_or(&default_msg);
        let now = self.next_timestamp();
        let repo = &mut self.repos[repo_idx];
        let tree_oid = Self::build_tree_from_flat_map(repo, &merged_flat);
        let parents = if base_tip != ZERO_OID {
            vec![base_tip]
        } else {
            vec![]
        };
        let commit_data = build_commit(&tree_oid, &parents, &author, &author, ts, msg);
        let sha = Self::put_object(repo, GitObjectKind::Commit, commit_data, None);
        Self::set_ref(repo, &base_ref_name, Some(sha));
        if let Some(pull) = repo.get_pull_mut(pull_number) {
            pull.merged = true;
            pull.state = "closed".to_string();
            pull.merged_at = Some(now.clone());
            pull.closed_at = Some(now.clone());
            pull.updated_at = now;
            pull.merge_commit_sha = Some(sha);
        }
        Ok(sha)
    }
}
