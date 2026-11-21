mod git;
mod graphql;
mod mcp;
mod rest;
mod store;

use crate::git::handle_git_request;
use crate::graphql::handle_graphql_request;
use crate::mcp::handle_mcp_request;
use crate::rest::{handle_rest_request, percent_decode, write_http_json_raw};
use crate::store::ForgeStore;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread;

const WORKER_COUNT: usize = 128;

const HEALTHZ_HTTP: &[u8] = concat!(
    "HTTP/1.1 200 OK\r\n",
    "content-type: application/json; charset=utf-8\r\n",
    "x-github-request-id: health\r\n",
    "content-length: 12\r\n",
    "connection: keep-alive\r\n\r\n",
    "{\"ok\":true}\n"
)
.as_bytes();

struct WorkQueue {
    queue: Mutex<VecDeque<TcpStream>>,
    cv: Condvar,
}

impl WorkQueue {
    fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::with_capacity(128)),
            cv: Condvar::new(),
        }
    }

    fn push(&self, stream: TcpStream) {
        let mut q = self.queue.lock().unwrap();
        q.push_back(stream);
        self.cv.notify_one();
    }

    fn pop(&self) -> TcpStream {
        let mut q = self.queue.lock().unwrap();
        loop {
            if let Some(s) = q.pop_front() {
                return s;
            }
            q = self.cv.wait(q).unwrap();
        }
    }
}

#[inline]
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[inline]
fn parse_usize_ascii(b: &[u8]) -> Option<usize> {
    let s = std::str::from_utf8(b).ok()?.trim();
    s.parse::<usize>().ok()
}

fn decode_chunked(buf: &[u8]) -> Option<(Vec<u8>, usize)> {
    let mut out = Vec::new();
    let mut pos = 0;
    loop {
        let crlf = find_subslice(&buf[pos..], b"\r\n")?;
        let line = std::str::from_utf8(&buf[pos..pos + crlf]).ok()?;
        let size_str = line.split(';').next().unwrap_or("").trim();
        let chunk_size = usize::from_str_radix(size_str, 16).ok()?;
        pos += crlf + 2;
        if chunk_size == 0 {
            if pos + 2 <= buf.len() && &buf[pos..pos + 2] == b"\r\n" {
                pos += 2;
                return Some((out, pos));
            }
            if let Some(end_trail) = find_subslice(&buf[pos..], b"\r\n\r\n") {
                pos += end_trail + 4;
                return Some((out, pos));
            }
            return None;
        }
        if pos + chunk_size + 2 > buf.len() {
            return None;
        }
        out.extend_from_slice(&buf[pos..pos + chunk_size]);
        pos += chunk_size + 2;
    }
}

fn handle_connection(
    mut stream: TcpStream,
    store: &Arc<RwLock<ForgeStore>>,
    read_buf: &mut Vec<u8>,
    resp_buf: &mut Vec<u8>,
) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(15)));
    read_buf.clear();
    let mut tmp = [0u8; 32768];

    loop {
        // 1. Read until full HTTP headers (\r\n\r\n)
        let header_end = loop {
            if let Some(idx) = find_subslice(read_buf, b"\r\n\r\n") {
                break idx;
            }
            match stream.read(&mut tmp) {
                Ok(0) => return,
                Ok(n) => read_buf.extend_from_slice(&tmp[..n]),
                Err(_) => return,
            }
        };

        let body_start = header_end + 4;
        // Parse request line + headers using byte offsets (zero alloc!)
        let header_bytes = &read_buf[..header_end];
        let first_eol = find_subslice(header_bytes, b"\r\n").unwrap_or(header_bytes.len());
        let req_line = &header_bytes[..first_eol];
        let mut req_parts = req_line.split(|&b| b == b' ');
        let method_b = match req_parts.next() {
            Some(m) if !m.is_empty() => m,
            _ => return,
        };
        let target_b = match req_parts.next() {
            Some(t) if !t.is_empty() => t,
            _ => return,
        };
        let version_b = req_parts.next().unwrap_or(b"HTTP/1.1");
        let default_keep_alive = version_b != b"HTTP/1.0";
        let method_len = method_b.len();
        let target_start = method_len + 1;
        let target_len = target_b.len();

        let mut content_length: usize = 0;
        let mut is_chunked = false;
        let mut keep_alive = default_keep_alive;
        let mut auth_range: Option<(usize, usize)> = None;
        let mut git_proto_range: Option<(usize, usize)> = None;
        let mut host_range: Option<(usize, usize)> = None;
        let mut expect_100 = false;

        let mut hpos = first_eol + 2;
        while hpos < header_bytes.len() {
            let rel_eol = find_subslice(&header_bytes[hpos..], b"\r\n")
                .unwrap_or(header_bytes.len() - hpos);
            let line = &header_bytes[hpos..hpos + rel_eol];
            if let Some(colon) = line.iter().position(|&b| b == b':') {
                let name = &line[..colon];
                let mut vstart = hpos + colon + 1;
                let mut vend = hpos + rel_eol;
                while vstart < vend && (header_bytes[vstart] == b' ' || header_bytes[vstart] == b'\t') {
                    vstart += 1;
                }
                while vend > vstart && (header_bytes[vend - 1] == b' ' || header_bytes[vend - 1] == b'\t') {
                    vend -= 1;
                }
                let val = &header_bytes[vstart..vend];
                if name.eq_ignore_ascii_case(b"content-length") {
                    content_length = parse_usize_ascii(val).unwrap_or(0);
                } else if name.eq_ignore_ascii_case(b"authorization") {
                    auth_range = Some((vstart, vend));
                } else if name.eq_ignore_ascii_case(b"git-protocol") {
                    git_proto_range = Some((vstart, vend));
                } else if name.eq_ignore_ascii_case(b"host") {
                    host_range = Some((vstart, vend));
                } else if name.eq_ignore_ascii_case(b"transfer-encoding") {
                    if val.windows(7).any(|w| w.eq_ignore_ascii_case(b"chunked")) {
                        is_chunked = true;
                    }
                } else if name.eq_ignore_ascii_case(b"connection") {
                    if val.eq_ignore_ascii_case(b"close") {
                        keep_alive = false;
                    } else if val.eq_ignore_ascii_case(b"keep-alive") {
                        keep_alive = true;
                    }
                } else if name.eq_ignore_ascii_case(b"expect") {
                    if val.windows(12).any(|w| w.eq_ignore_ascii_case(b"100-continue")) {
                        expect_100 = true;
                    }
                }
            }
            hpos += rel_eol + 2;
        }

        if expect_100 && read_buf.len() == body_start {
            let _ = stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n");
        }

        // 2. Read full request body
        let mut owned_chunked_body: Option<Vec<u8>> = None;
        let consumed_total: usize;
        if is_chunked {
            loop {
                if let Some((decoded, used)) = decode_chunked(&read_buf[body_start..]) {
                    owned_chunked_body = Some(decoded);
                    consumed_total = body_start + used;
                    break;
                }
                match stream.read(&mut tmp) {
                    Ok(0) => return,
                    Ok(n) => read_buf.extend_from_slice(&tmp[..n]),
                    Err(_) => return,
                }
            }
        } else {
            let needed = body_start + content_length;
            while read_buf.len() < needed {
                match stream.read(&mut tmp) {
                    Ok(0) => return,
                    Ok(n) => read_buf.extend_from_slice(&tmp[..n]),
                    Err(_) => return,
                }
            }
            consumed_total = needed;
        }

        // Copy slices/metadata before dispatching
        let method_str = match std::str::from_utf8(&read_buf[..method_len]) {
            Ok(s) => s,
            Err(_) => return,
        };
        let target_str = match std::str::from_utf8(&read_buf[target_start..target_start + target_len]) {
            Ok(s) => s,
            Err(_) => return,
        };
        let (path_str, query_str) = match target_str.split_once('?') {
            Some((p, q)) => (p, q),
            None => (target_str, ""),
        };
        let auth_slice = auth_range.map(|(s, e)| &read_buf[s..e]);
        let git_proto_slice = git_proto_range.map(|(s, e)| &read_buf[s..e]);
        let host_str = host_range
            .and_then(|(s, e)| std::str::from_utf8(&read_buf[s..e]).ok())
            .unwrap_or("127.0.0.1");
        let body_slice: &[u8] = match &owned_chunked_body {
            Some(v) => v.as_slice(),
            None => &read_buf[body_start..body_start + content_length],
        };

        // 3. Dispatch & write single-syscall response!
        resp_buf.clear();
        let cached_arc: Option<Arc<Vec<u8>>> = if method_str == "GET" && path_str == "/healthz" {
            resp_buf.extend_from_slice(HEALTHZ_HTTP);
            None
        } else if path_str == "/api/v3" || path_str.starts_with("/api/v3/") {
            handle_rest_request(
                store,
                method_str,
                path_str,
                query_str,
                host_str,
                auth_slice,
                body_slice,
                resp_buf,
            )
        } else if path_str == "/api/graphql" {
            handle_graphql_request(store, auth_slice, body_slice, resp_buf);
            None
        } else if path_str == "/mcp" {
            handle_mcp_request(store, auth_slice, body_slice, resp_buf);
            None
        } else if let Some((owner, repo_name, rest_path)) = parse_git_path(path_str) {
            handle_git_request(
                store,
                method_str,
                &owner,
                &repo_name,
                rest_path,
                query_str,
                auth_slice,
                git_proto_slice,
                body_slice,
                resp_buf,
            )
        } else {
            write_http_json_raw(resp_buf, "404 Not Found", "", b"{\"message\":\"Not Found\"}\n");
            None
        };

        let write_res = match &cached_arc {
            Some(arc) => stream.write_all(arc.as_slice()),
            None => stream.write_all(resp_buf.as_slice()),
        };
        if write_res.is_err() {
            return;
        }

        // Shift any pipelined bytes in read_buf
        if consumed_total >= read_buf.len() {
            read_buf.clear();
        } else {
            read_buf.drain(..consumed_total);
        }

        if !keep_alive {
            return;
        }
    }
}

#[inline]
fn parse_git_path(path: &str) -> Option<(String, String, &str)> {
    let rest = path.strip_prefix('/')?;
    let slash1 = rest.find('/')?;
    let owner_raw = &rest[..slash1];
    let after_owner = &rest[slash1 + 1..];
    let git_idx = after_owner.find(".git/")?;
    let repo_raw = &after_owner[..git_idx];
    if repo_raw.contains('/') || owner_raw.is_empty() || repo_raw.is_empty() {
        return None;
    }
    let sub = &after_owner[git_idx + 4..]; // starts with '/'
    let owner = if owner_raw.contains('%') {
        percent_decode(owner_raw)
    } else {
        owner_raw.to_string()
    };
    let repo = if repo_raw.contains('%') {
        percent_decode(repo_raw)
    } else {
        repo_raw.to_string()
    };
    Some((owner, repo, sub))
}

fn main() {
    let bind_addr = std::env::var("FORGE_BIND").unwrap_or_else(|_| "0.0.0.0:0".to_string());
    let listener = TcpListener::bind(&bind_addr).expect("failed to bind TCP listener");
    let port = listener.local_addr().expect("no local addr").port();

    let store = Arc::new(RwLock::new(ForgeStore::new()));
    let queue = Arc::new(WorkQueue::new());

    for _ in 0..WORKER_COUNT {
        let q = Arc::clone(&queue);
        let st = Arc::clone(&store);
        thread::Builder::new()
            .stack_size(1024 * 1024)
            .spawn(move || {
                let mut read_buf = Vec::with_capacity(65536);
                let mut resp_buf = Vec::with_capacity(65536);
                loop {
                    let stream = q.pop();
                    handle_connection(stream, &st, &mut read_buf, &mut resp_buf);
                }
            })
            .expect("spawn worker");
    }

    println!("PORT {}", port);
    let _ = std::io::stdout().flush();

    for conn in listener.incoming() {
        if let Ok(stream) = conn {
            queue.push(stream);
        }
    }
}
