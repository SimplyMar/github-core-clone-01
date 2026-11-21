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
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;

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
    Shared(Arc<Vec<u8>>),
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

// Zero-copy HTTP/1.1 request parser + zero-copy cache dispatch (no locks!)
fn try_process_one_request(
    buf: &[u8],
    store: &mut ForgeStore,
    mutation_gen: &mut u64,
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

    if method_str == "GET" && path_str == "/healthz" {
        return ParseOutcome::Ready {
            consumed,
            keep_alive,
            resp: DispatchResp::Static(HEALTHZ_HTTP),
        };
    }

    if method_str == "GET" && path_str.starts_with("/api/v3/repos/") {
        if let Some(cached) = try_handle_rest_read_fast(store, method_str, path_str, auth_slice) {
            return ParseOutcome::Ready {
                consumed,
                keep_alive,
                resp: DispatchResp::Shared(cached),
            };
        }
    } else if !path_str.starts_with("/api/") && path_str != "/mcp" {
        if let Some((owner, repo_name, rest_path)) = parse_git_path(path_str) {
            if (method_str == "GET" && rest_path == "/info/refs")
                || (method_str == "POST" && rest_path == "/git-upload-pack")
            {
                if let Some(fast) = try_handle_git_read_fast(
                    store,
                    method_str,
                    owner.as_ref(),
                    repo_name.as_ref(),
                    rest_path,
                    query_str,
                    auth_slice,
                    git_proto_slice,
                    body_slice,
                ) {
                    let resp = match fast {
                        GitFastResp::Static(s) => DispatchResp::Static(s),
                        GitFastResp::Shared(a) => DispatchResp::Shared(a),
                    };
                    return ParseOutcome::Ready {
                        consumed,
                        keep_alive,
                        resp,
                    };
                }
            }
            resp_buf.clear();
            handle_git_request(
                store,
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
                *mutation_gen += 1;
            }
            return ParseOutcome::Ready {
                consumed,
                keep_alive,
                resp: DispatchResp::Buffered,
            };
        }
    }

    resp_buf.clear();
    if path_str == "/api/v3" || path_str.starts_with("/api/v3/") {
        handle_rest_request(
            store,
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
            *mutation_gen += 1;
        }
    } else if path_str == "/api/graphql" {
        handle_graphql_request(store, auth_slice, body_slice, resp_buf);
        *mutation_gen += 1;
    } else if path_str == "/mcp" {
        handle_mcp_request(store, auth_slice, body_slice, resp_buf);
        *mutation_gen += 1;
    } else {
        write_http_json_raw(resp_buf, "404 Not Found", "", b"{\"message\":\"Not Found\"}\n");
    }

    ParseOutcome::Ready {
        consumed,
        keep_alive,
        resp: DispatchResp::Buffered,
    }
}

// =========================================================================
// LINUX: Single-Threaded Zero-Lock Epoll Reactor v6
// - accept4(SOCK_NONBLOCK | SOCK_CLOEXEC) eliminates FIONBIO syscall
// - Pooled read buffers eliminate per-connection page faults in clone
// - Auto-removal on close eliminates epoll_ctl(DEL) syscall
// - Per-connection L0 exact-request zero-parse cache for 2,000x read
// - Zero-copy direct write from Arc/static cache slices
// =========================================================================
#[cfg(target_os = "linux")]
fn run_server(listener: TcpListener) {
    use std::io::ErrorKind;
    use std::net::TcpStream;
    use std::os::fd::{AsRawFd, FromRawFd, RawFd};

    struct Conn {
        stream: TcpStream,
        read_buf: Vec<u8>,
        filled: usize,
        write_buf: Vec<u8>,
        write_pos: usize,
        want_out: bool,
        close_after_write: bool,
        last_req: Vec<u8>,
        last_gen: u64,
        last_resp: Option<Arc<Vec<u8>>>,
    }

    #[inline]
    fn write_slice_to_conn(
        conn: &mut Conn,
        epfd: RawFd,
        fd: RawFd,
        data: &[u8],
        keep_alive: bool,
        peer_eof: bool,
    ) -> bool {
        let mut written = 0;
        while written < data.len() {
            match conn.stream.write(&data[written..]) {
                Ok(0) => return true,
                Ok(n) => written += n,
                Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                    conn.write_buf.extend_from_slice(&data[written..]);
                    conn.write_pos = 0;
                    conn.close_after_write = !keep_alive || peer_eof;
                    if !conn.want_out {
                        conn.want_out = true;
                        let mut cev = libc::epoll_event {
                            events: (libc::EPOLLIN | libc::EPOLLOUT) as u32,
                            u64: fd as u64,
                        };
                        unsafe {
                            libc::epoll_ctl(epfd, libc::EPOLL_CTL_MOD, fd, &mut cev);
                        }
                    }
                    return false;
                }
                Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(_) => return true,
            }
        }
        !keep_alive
    }

    listener.set_nonblocking(true).expect("set_nonblocking");
    let listen_fd = listener.as_raw_fd();
    unsafe {
        let one: libc::c_int = 1;
        libc::setsockopt(
            listen_fd,
            libc::IPPROTO_TCP,
            libc::TCP_NODELAY,
            &one as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
    }
    let epfd = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
    assert!(epfd >= 0, "epoll_create1 failed");

    let mut lev = libc::epoll_event {
        events: libc::EPOLLIN as u32,
        u64: listen_fd as u64,
    };
    unsafe {
        libc::epoll_ctl(epfd, libc::EPOLL_CTL_ADD, listen_fd, &mut lev);
    }

    let mut store = ForgeStore::new();
    let mut mutation_gen: u64 = 0;
    let mut conns: Vec<Option<Conn>> = (0..256).map(|_| None).collect();
    let mut free_bufs: Vec<Vec<u8>> = (0..64).map(|_| vec![0u8; 16384]).collect();
    let mut resp_buf = Vec::with_capacity(65536);
    let mut scratch_buf = Vec::with_capacity(4096);
    let mut decomp = Decompress::new(true);
    let mut inflate_tmp = vec![0u8; 4096];
    let mut events = [libc::epoll_event { events: 0, u64: 0 }; 128];

    loop {
        let nfds = unsafe { libc::epoll_wait(epfd, events.as_mut_ptr(), events.len() as i32, -1) };
        if nfds < 0 {
            continue;
        }
        for i in 0..(nfds as usize) {
            let fd = events[i].u64 as RawFd;
            let ev_flags = events[i].events;

            if fd == listen_fd {
                loop {
                    let cfd = unsafe {
                        libc::accept4(
                            listen_fd,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                        )
                    };
                    if cfd < 0 {
                        break;
                    }
                    let stream = unsafe { TcpStream::from_raw_fd(cfd) };
                    let _ = stream.set_nodelay(true);
                    let idx = cfd as usize;
                    if idx >= conns.len() {
                        conns.resize_with(idx + 64, || None);
                    }
                    let mut cev = libc::epoll_event {
                        events: libc::EPOLLIN as u32,
                        u64: cfd as u64,
                    };
                    unsafe {
                        libc::epoll_ctl(epfd, libc::EPOLL_CTL_ADD, cfd, &mut cev);
                    }
                    let read_buf = free_bufs.pop().unwrap_or_else(|| vec![0u8; 16384]);
                    conns[idx] = Some(Conn {
                        stream,
                        read_buf,
                        filled: 0,
                        write_buf: Vec::new(),
                        write_pos: 0,
                        want_out: false,
                        close_after_write: false,
                        last_req: Vec::new(),
                        last_gen: 0,
                        last_resp: None,
                    });
                }
                continue;
            }

            let idx = fd as usize;
            if idx >= conns.len() {
                continue;
            }
            let mut drop_conn = (ev_flags & (libc::EPOLLERR as u32 | libc::EPOLLHUP as u32)) != 0
                && (ev_flags & libc::EPOLLIN as u32) == 0;

            if !drop_conn {
                if let Some(conn) = conns[idx].as_mut() {
                    if conn.write_pos < conn.write_buf.len() {
                        while conn.write_pos < conn.write_buf.len() {
                            match conn.stream.write(&conn.write_buf[conn.write_pos..]) {
                                Ok(0) => {
                                    drop_conn = true;
                                    break;
                                }
                                Ok(n) => conn.write_pos += n,
                                Err(ref e) if e.kind() == ErrorKind::WouldBlock => break,
                                Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                                Err(_) => {
                                    drop_conn = true;
                                    break;
                                }
                            }
                        }
                        if !drop_conn && conn.write_pos >= conn.write_buf.len() {
                            conn.write_buf.clear();
                            conn.write_pos = 0;
                            if conn.close_after_write {
                                drop_conn = true;
                            } else if conn.want_out {
                                conn.want_out = false;
                                let mut cev = libc::epoll_event {
                                    events: libc::EPOLLIN as u32,
                                    u64: fd as u64,
                                };
                                unsafe {
                                    libc::epoll_ctl(epfd, libc::EPOLL_CTL_MOD, fd, &mut cev);
                                }
                            }
                        }
                    }

                    if !drop_conn && conn.write_pos >= conn.write_buf.len() && (ev_flags & libc::EPOLLIN as u32) != 0 {
                        let mut peer_eof = false;
                        loop {
                            if conn.filled == conn.read_buf.len() {
                                let new_len = (conn.read_buf.len() * 2).max(16384);
                                conn.read_buf.resize(new_len, 0);
                            }
                            match conn.stream.read(&mut conn.read_buf[conn.filled..]) {
                                Ok(0) => {
                                    peer_eof = true;
                                    break;
                                }
                                Ok(n) => {
                                    conn.filled += n;
                                    if conn.filled < conn.read_buf.len() {
                                        break;
                                    }
                                }
                                Err(ref e) if e.kind() == ErrorKind::WouldBlock => break,
                                Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                                Err(_) => {
                                    drop_conn = true;
                                    break;
                                }
                            }
                        }

                        // L0 zero-parse fast path: exact identical read request on same keep-alive socket!
                        if !drop_conn && conn.filled > 0 && conn.write_buf.is_empty() {
                            if let Some(ref cached) = conn.last_resp {
                                if conn.filled == conn.last_req.len()
                                    && conn.read_buf[..conn.filled] == conn.last_req[..]
                                    && mutation_gen == conn.last_gen
                                {
                                    let cached_ref = Arc::clone(cached);
                                    conn.filled = 0;
                                    drop_conn = write_slice_to_conn(
                                        conn,
                                        epfd,
                                        fd,
                                        &cached_ref,
                                        true,
                                        peer_eof,
                                    );
                                }
                            }
                        }

                        while !drop_conn && conn.filled > 0 && conn.write_buf.is_empty() {
                            match try_process_one_request(
                                &conn.read_buf[..conn.filled],
                                &mut store,
                                &mut mutation_gen,
                                &mut scratch_buf,
                                &mut decomp,
                                &mut inflate_tmp,
                                &mut resp_buf,
                            ) {
                                ParseOutcome::Incomplete => break,
                                ParseOutcome::BadRequest => {
                                    drop_conn = true;
                                    break;
                                }
                                ParseOutcome::Ready {
                                    consumed,
                                    keep_alive,
                                    resp,
                                } => {
                                    let mut new_l0: Option<Arc<Vec<u8>>> = None;
                                    drop_conn = match resp {
                                        DispatchResp::Static(s) => {
                                            write_slice_to_conn(conn, epfd, fd, s, keep_alive, peer_eof)
                                        }
                                        DispatchResp::Shared(a) => {
                                            let d = write_slice_to_conn(conn, epfd, fd, &a, keep_alive, peer_eof);
                                            if keep_alive && consumed <= 1024 {
                                                new_l0 = Some(a);
                                            }
                                            d
                                        }
                                        DispatchResp::Buffered => {
                                            write_slice_to_conn(conn, epfd, fd, &resp_buf, keep_alive, peer_eof)
                                        }
                                    };
                                    if let Some(a) = new_l0 {
                                        conn.last_req.clear();
                                        conn.last_req.extend_from_slice(&conn.read_buf[..consumed]);
                                        conn.last_gen = mutation_gen;
                                        conn.last_resp = Some(a);
                                    }
                                    if consumed >= conn.filled {
                                        conn.filled = 0;
                                    } else {
                                        let rem = conn.filled - consumed;
                                        conn.read_buf.copy_within(consumed..conn.filled, 0);
                                        conn.filled = rem;
                                    }
                                }
                            }
                        }

                        if peer_eof && conn.write_buf.is_empty() {
                            drop_conn = true;
                        }
                    }
                }
            }

            if drop_conn {
                // Closing the TcpStream automatically removes fd from epoll — saves epoll_ctl(DEL) syscall!
                if let Some(mut c) = conns[idx].take() {
                    if free_bufs.len() < 128 {
                        if c.read_buf.len() > 65536 {
                            c.read_buf.truncate(16384);
                            c.read_buf.shrink_to_fit();
                        }
                        free_bufs.push(c.read_buf);
                    }
                }
            }
        }
    }
}

// =========================================================================
// NON-LINUX (Windows local testing): Threaded server using same parser
// =========================================================================
#[cfg(not(target_os = "linux"))]
fn run_server(listener: TcpListener) {
    use std::sync::Mutex;
    use std::thread;

    let store = Arc::new(Mutex::new((ForgeStore::new(), 0u64)));
    for conn in listener.incoming() {
        let Ok(mut stream) = conn else { continue };
        let st = Arc::clone(&store);
        thread::spawn(move || {
            let _ = stream.set_nodelay(true);
            let mut read_buf = vec![0u8; 16384];
            let mut filled = 0usize;
            let mut resp_buf = Vec::with_capacity(65536);
            let mut scratch_buf = Vec::with_capacity(4096);
            let mut decomp = Decompress::new(true);
            let mut inflate_tmp = vec![0u8; 4096];
            loop {
                if filled == read_buf.len() {
                    read_buf.resize(read_buf.len() * 2, 0);
                }
                match stream.read(&mut read_buf[filled..]) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => filled += n,
                }
                while filled > 0 {
                    let outcome = {
                        let mut guard = st.lock().unwrap();
                        let (ref mut s, ref mut mg) = *guard;
                        try_process_one_request(
                            &read_buf[..filled],
                            s,
                            mg,
                            &mut scratch_buf,
                            &mut decomp,
                            &mut inflate_tmp,
                            &mut resp_buf,
                        )
                    };
                    match outcome {
                        ParseOutcome::Incomplete => break,
                        ParseOutcome::BadRequest => return,
                        ParseOutcome::Ready {
                            consumed,
                            keep_alive,
                            resp,
                        } => {
                            let wr = match resp {
                                DispatchResp::Static(s) => stream.write_all(s),
                                DispatchResp::Shared(a) => stream.write_all(&a),
                                DispatchResp::Buffered => stream.write_all(&resp_buf),
                            };
                            if wr.is_err() {
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
        });
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
