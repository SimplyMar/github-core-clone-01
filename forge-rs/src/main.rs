mod git;
mod graphql;
mod mcp;
mod rest;
mod store;

use crate::git::{handle_git_request, try_handle_git_read_fast, GitFastResp};
use crate::graphql::handle_graphql_request;
use crate::mcp::handle_mcp_request;
use crate::rest::{
    handle_rest_request, percent_decode_cow, try_handle_rest_read_fast, write_http_json_raw,
};
use crate::store::ForgeStore;
use flate2::Decompress;
use std::borrow::Cow;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread;
use std::time::Duration;

const HEALTHZ_HTTP: &[u8] = concat!(
    "HTTP/1.1 200 OK\r\n",
    "content-type: application/json\r\n",
    "x-github-request-id: 1\r\n",
    "content-length: 11\r\n\r\n",
    "{\"ok\":true}"
)
.as_bytes();

#[inline]
fn find_header_end(buf: &[u8]) -> Option<usize> {
    let len = buf.len();
    if len < 4 {
        return None;
    }
    let mut i = 3;
    while i < len {
        let b = buf[i];
        if b == b'\n' {
            if buf[i - 1] == b'\r' && buf[i - 2] == b'\n' && buf[i - 3] == b'\r' {
                return Some(i - 3);
            }
            i += 2;
        } else if b == b'\r' {
            i += 1;
        } else {
            i += 4;
        }
    }
    None
}

#[inline]
fn find_crlf(buf: &[u8]) -> Option<usize> {
    let len = buf.len();
    if len < 2 {
        return None;
    }
    let mut i = 1;
    while i < len {
        let b = buf[i];
        if b == b'\n' {
            if buf[i - 1] == b'\r' {
                return Some(i - 1);
            }
            i += 2;
        } else if b == b'\r' {
            i += 1;
        } else {
            i += 2;
        }
    }
    None
}

#[inline]
fn parse_usize_ascii(b: &[u8]) -> Option<usize> {
    let mut val = 0usize;
    let mut any = false;
    for &c in b {
        if c == b' ' || c == b'\t' {
            continue;
        }
        if !c.is_ascii_digit() {
            return None;
        }
        val = val.checked_mul(10)?.checked_add((c - b'0') as usize)?;
        any = true;
    }
    if any { Some(val) } else { None }
}

fn decode_chunked(buf: &[u8]) -> Option<(Vec<u8>, usize)> {
    let mut out = Vec::new();
    let mut pos = 0;
    loop {
        let crlf = find_crlf(&buf[pos..])?;
        let line = std::str::from_utf8(&buf[pos..pos + crlf]).ok()?;
        let size_str = line.split(';').next().unwrap_or("").trim();
        let chunk_size = usize::from_str_radix(size_str, 16).ok()?;
        pos += crlf + 2;
        if chunk_size == 0 {
            if pos + 2 <= buf.len() && &buf[pos..pos + 2] == b"\r\n" {
                pos += 2;
                return Some((out, pos));
            }
            if let Some(end_trail) = find_header_end(&buf[pos..]) {
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

#[inline]
fn parse_git_path(path: &str) -> Option<(Cow<'_, str>, Cow<'_, str>, &str)> {
    let rest = path.strip_prefix('/')?;
    let slash1 = rest.find('/')?;
    let owner_raw = &rest[..slash1];
    let after_owner = &rest[slash1 + 1..];
    let git_idx = after_owner.find(".git/")?;
    let repo_raw = &after_owner[..git_idx];
    if repo_raw.contains('/') || owner_raw.is_empty() || repo_raw.is_empty() {
        return None;
    }
    let sub = &after_owner[git_idx + 4..];
    Some((percent_decode_cow(owner_raw), percent_decode_cow(repo_raw), sub))
}

enum DispatchResp {
    Static(&'static [u8]),
    Shared(Arc<Vec<u8>>, u64),
    Buffered,
}

enum ParseOutcome {
    Incomplete,
    BadRequest,
    Ready {
        consumed: usize,
        keep_alive: bool,
        resp: DispatchResp,
    },
}

// Zero-copy HTTP/1.1 parser + shared-read L1 fast path + write-path fallback.
fn try_process_one_request(
    buf: &[u8],
    store_lock: &RwLock<ForgeStore>,
    mutation_gen: &AtomicU64,
    scratch_buf: &mut Vec<u8>,
    decomp: &mut Decompress,
    inflate_tmp: &mut [u8],
    resp_buf: &mut Vec<u8>,
) -> ParseOutcome {
    let header_end = match find_header_end(buf) {
        Some(idx) => idx,
        None => return ParseOutcome::Incomplete,
    };
    let body_start = header_end + 4;
    let header_bytes = &buf[..header_end];
    let first_eol = find_crlf(header_bytes).unwrap_or(header_bytes.len());
    let req_line = &header_bytes[..first_eol];
    let sp1 = match req_line.iter().position(|&b| b == b' ') {
        Some(p) if p > 0 => p,
        _ => return ParseOutcome::BadRequest,
    };
    let after_sp1 = &req_line[sp1 + 1..];
    let sp2 = match after_sp1.iter().position(|&b| b == b' ') {
        Some(p) if p > 0 => p,
        _ => return ParseOutcome::BadRequest,
    };
    let method_str = match std::str::from_utf8(&req_line[..sp1]) {
        Ok(s) => s,
        Err(_) => return ParseOutcome::BadRequest,
    };
    let target_str = match std::str::from_utf8(&after_sp1[..sp2]) {
        Ok(s) => s,
        Err(_) => return ParseOutcome::BadRequest,
    };
    let version_b = &after_sp1[sp2 + 1..];
    let mut keep_alive = version_b != b"HTTP/1.0";

    let mut content_length: usize = 0;
    let mut is_chunked = false;
    let mut auth_slice: Option<&[u8]> = None;
    let mut git_proto_slice: Option<&[u8]> = None;
    let mut host_str: &str = "127.0.0.1";

    let mut hpos = first_eol + 2;
    while hpos < header_bytes.len() {
        let rel_eol = find_crlf(&header_bytes[hpos..]).unwrap_or(header_bytes.len() - hpos);
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
            match name.len() {
                4 if name.eq_ignore_ascii_case(b"host") => {
                    if let Ok(h) = std::str::from_utf8(val) {
                        host_str = h;
                    }
                }
                10 if name.eq_ignore_ascii_case(b"connection") => {
                    if val.eq_ignore_ascii_case(b"close") {
                        keep_alive = false;
                    } else if val.eq_ignore_ascii_case(b"keep-alive") {
                        keep_alive = true;
                    }
                }
                12 if name.eq_ignore_ascii_case(b"git-protocol") => {
                    git_proto_slice = Some(val);
                }
                13 if name.eq_ignore_ascii_case(b"authorization") => {
                    auth_slice = Some(val);
                }
                14 if name.eq_ignore_ascii_case(b"content-length") => {
                    content_length = parse_usize_ascii(val).unwrap_or(0);
                }
                17 if name.eq_ignore_ascii_case(b"transfer-encoding") => {
                    if val.windows(7).any(|w| w.eq_ignore_ascii_case(b"chunked")) {
                        is_chunked = true;
                    }
                }
                _ => {}
            }
        }
        hpos += rel_eol + 2;
    }

    let mut owned_chunked: Option<Vec<u8>> = None;
    let consumed: usize;
    if is_chunked {
        match decode_chunked(&buf[body_start..]) {
            Some((decoded, used)) => {
                owned_chunked = Some(decoded);
                consumed = body_start + used;
            }
            None => return ParseOutcome::Incomplete,
        }
    } else {
        let needed = body_start + content_length;
        if buf.len() < needed {
            return ParseOutcome::Incomplete;
        }
        consumed = needed;
    }

    let (path_str, query_str) = match target_str.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target_str, ""),
    };
    let body_slice: &[u8] = match &owned_chunked {
        Some(v) => v.as_slice(),
        None => &buf[body_start..body_start + content_length],
    };

    // LOCK-FREE FAST PATH: /healthz
    if method_str == "GET" && path_str == "/healthz" {
        return ParseOutcome::Ready {
            consumed,
            keep_alive,
            resp: DispatchResp::Static(HEALTHZ_HTTP),
        };
    }

    // SHARED-READ L1 FAST PATHS (RwLock::read only, zero byte copy, full 2-CPU parallelism!)
    if method_str == "GET" && path_str.starts_with("/api/v3/repos/") {
        let guard = store_lock.read().unwrap();
        if let Some(cached) = try_handle_rest_read_fast(&guard, method_str, path_str, auth_slice) {
            let gen_now = mutation_gen.load(Ordering::Acquire);
            return ParseOutcome::Ready {
                consumed,
                keep_alive,
                resp: DispatchResp::Shared(cached, gen_now),
            };
        }
    } else if !path_str.starts_with("/api/") && path_str != "/mcp" {
        if let Some((owner, repo_name, rest_path)) = parse_git_path(path_str) {
            if (method_str == "GET" && rest_path == "/info/refs")
                || (method_str == "POST" && rest_path == "/git-upload-pack")
            {
                let guard = store_lock.read().unwrap();
                if let Some(fast) = try_handle_git_read_fast(
                    &guard,
                    method_str,
                    owner.as_ref(),
                    repo_name.as_ref(),
                    rest_path,
                    query_str,
                    auth_slice,
                    git_proto_slice,
                    body_slice,
                ) {
                    let gen_now = mutation_gen.load(Ordering::Acquire);
                    let resp = match fast {
                        GitFastResp::Static(s) => DispatchResp::Static(s),
                        GitFastResp::Shared(a) => DispatchResp::Shared(a, gen_now),
                    };
                    return ParseOutcome::Ready {
                        consumed,
                        keep_alive,
                        resp,
                    };
                }
            }
            // Write-lock fallback for git request (already parsed git path)
            resp_buf.clear();
            {
                let mut guard = store_lock.write().unwrap();
                handle_git_request(
                    &mut guard,
                    method_str,
                    owner.as_ref(),
                    repo_name.as_ref(),
                    rest_path,
                    query_str,
                    auth_slice,
                    git_proto_slice,
                    body_slice,
                    decomp,
                    inflate_tmp,
                    resp_buf,
                );
                if rest_path == "/git-receive-pack" {
                    mutation_gen.fetch_add(1, Ordering::Release);
                }
            }
            return ParseOutcome::Ready {
                consumed,
                keep_alive,
                resp: DispatchResp::Buffered,
            };
        }
    }

    // WRITE-LOCK PATH for REST / GraphQL / MCP / cache-miss / 404
    resp_buf.clear();
    {
        let mut guard = store_lock.write().unwrap();
        if path_str == "/api/v3" || path_str.starts_with("/api/v3/") {
            handle_rest_request(
                &mut guard,
                method_str,
                path_str,
                query_str,
                host_str,
                auth_slice,
                body_slice,
                scratch_buf,
                resp_buf,
            );
            if method_str != "GET" && method_str != "HEAD" {
                mutation_gen.fetch_add(1, Ordering::Release);
            }
        } else if path_str == "/api/graphql" {
            handle_graphql_request(&mut guard, auth_slice, body_slice, resp_buf);
            mutation_gen.fetch_add(1, Ordering::Release);
        } else if path_str == "/mcp" {
            handle_mcp_request(&mut guard, auth_slice, body_slice, resp_buf);
            mutation_gen.fetch_add(1, Ordering::Release);
        } else {
            write_http_json_raw(resp_buf, "404 Not Found", "", b"{\"message\":\"Not Found\"}\n");
        }
    }

    ParseOutcome::Ready {
        consumed,
        keep_alive,
        resp: DispatchResp::Buffered,
    }
}

fn handle_connection(
    mut stream: TcpStream,
    store: &RwLock<ForgeStore>,
    mutation_gen: &AtomicU64,
    read_buf: &mut Vec<u8>,
    resp_buf: &mut Vec<u8>,
    scratch_buf: &mut Vec<u8>,
    decomp: &mut Decompress,
    inflate_tmp: &mut [u8],
) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(1200)));
    let mut filled = 0usize;
    // Per-connection L0 exact-request read cache (invalidated by global mutation_gen)
    let mut last_req: Vec<u8> = Vec::new();
    let mut last_gen: u64 = 0;
    let mut last_resp: Option<Arc<Vec<u8>>> = None;

    loop {
        if filled == read_buf.len() {
            read_buf.resize((read_buf.len() * 2).max(16384), 0);
        }
        match stream.read(&mut read_buf[filled..]) {
            Ok(0) | Err(_) => return,
            Ok(n) => filled += n,
        }
        // L0 ultra-fast path: identical read-only request on keep-alive connection with zero mutations
        if let Some(ref cached) = last_resp {
            if filled == last_req.len()
                && read_buf[..filled] == last_req[..]
                && mutation_gen.load(Ordering::Acquire) == last_gen
            {
                if stream.write_all(cached).is_err() {
                    return;
                }
                filled = 0;
                continue;
            }
        }
        while filled > 0 {
            match try_process_one_request(
                &read_buf[..filled],
                store,
                mutation_gen,
                scratch_buf,
                decomp,
                inflate_tmp,
                resp_buf,
            ) {
                ParseOutcome::Incomplete => break,
                ParseOutcome::BadRequest => return,
                ParseOutcome::Ready {
                    consumed,
                    keep_alive,
                    resp,
                } => {
                    let write_res = match resp {
                        DispatchResp::Static(s) => stream.write_all(s),
                        DispatchResp::Shared(a, gen_now) => {
                            let r = stream.write_all(&a);
                            if keep_alive && consumed <= 1024 {
                                last_req.clear();
                                last_req.extend_from_slice(&read_buf[..consumed]);
                                last_gen = gen_now;
                                last_resp = Some(a);
                            }
                            r
                        }
                        DispatchResp::Buffered => stream.write_all(resp_buf),
                    };
                    if write_res.is_err() {
                        return;
                    }
                    if consumed >= filled {
                        filled = 0;
                    } else {
                        let rem = filled - consumed;
                        read_buf.copy_within(consumed..filled, 0);
                        filled = rem;
                    }
                    if !keep_alive {
                        return;
                    }
                }
            }
        }
    }
}

// =========================================================================
// Pre-spawned Blocking Worker Pool + RwLock Shared-Read Fast Paths + L0 Cache
// (2 syscalls/req on gVisor — no epoll_wait/epoll_ctl/FIONBIO overhead,
//  plus full 2-CPU parallel execution for 50 concurrent clones!)
// =========================================================================
fn run_server(listener: TcpListener) {
    const WORKER_COUNT: usize = 128;
    let store = Arc::new(RwLock::new(ForgeStore::new()));
    let mutation_gen = Arc::new(AtomicU64::new(0));
    let queue: Arc<(Mutex<VecDeque<TcpStream>>, Condvar)> =
        Arc::new((Mutex::new(VecDeque::with_capacity(256)), Condvar::new()));

    for _ in 0..WORKER_COUNT {
        let st = Arc::clone(&store);
        let mg = Arc::clone(&mutation_gen);
        let q = Arc::clone(&queue);
        let _ = thread::Builder::new().stack_size(128 * 1024).spawn(move || {
            let mut read_buf = vec![0u8; 16384];
            let mut resp_buf = Vec::with_capacity(65536);
            let mut scratch_buf = Vec::with_capacity(4096);
            let mut decomp = Decompress::new(true);
            let mut inflate_tmp = vec![0u8; 4096];
            loop {
                let stream = {
                    let (lock, cvar) = &*q;
                    let mut guard = lock.lock().unwrap();
                    while guard.is_empty() {
                        guard = cvar.wait(guard).unwrap();
                    }
                    guard.pop_front().unwrap()
                };
                handle_connection(
                    stream,
                    &st,
                    &mg,
                    &mut read_buf,
                    &mut resp_buf,
                    &mut scratch_buf,
                    &mut decomp,
                    &mut inflate_tmp,
                );
                if read_buf.len() > 262_144 {
                    read_buf.truncate(16384);
                    read_buf.shrink_to_fit();
                }
            }
        });
    }

    for conn in listener.incoming() {
        let Ok(stream) = conn else { continue };
        let (lock, cvar) = &*queue;
        let mut guard = lock.lock().unwrap();
        guard.push_back(stream);
        drop(guard);
        cvar.notify_one();
    }
}

fn main() {
    let bind_addr = std::env::var("FORGE_BIND").unwrap_or_else(|_| "0.0.0.0:0".to_string());
    let listener = TcpListener::bind(&bind_addr).expect("failed to bind TCP listener");
    let port = listener.local_addr().expect("no local addr").port();

    println!("PORT {}", port);
    let _ = std::io::stdout().flush();

    run_server(listener);
}
