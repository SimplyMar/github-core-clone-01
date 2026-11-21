use crate::store::{
    base64_decode, base64_encode, oid_to_hex, parse_commit, parse_tree, ForgeStore, GitObjectKind,
    Issue, IssueComment, Label, PullRequest, Repo, UserObj,
};
use rustc_hash::FxHashMap;
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};

#[inline]
pub fn write_json_str(out: &mut Vec<u8>, s: &str) {
    out.push(b'"');
    let bytes = s.as_bytes();
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        let esc: Option<&[u8]> = match b {
            b'"' => Some(b"\\\""),
            b'\\' => Some(b"\\\\"),
            b'\n' => Some(b"\\n"),
            b'\r' => Some(b"\\r"),
            b'\t' => Some(b"\\t"),
            0x08 => Some(b"\\b"),
            0x0c => Some(b"\\f"),
            0..=0x1f => None,
            _ => continue,
        };
        if i > start {
            out.extend_from_slice(&bytes[start..i]);
        }
        if let Some(e) = esc {
            out.extend_from_slice(e);
        } else {
            let hex = b"0123456789abcdef";
            out.extend_from_slice(&[b'\\', b'u', b'0', b'0', hex[(b >> 4) as usize], hex[(b & 0xf) as usize]]);
        }
        start = i + 1;
    }
    if start < bytes.len() {
        out.extend_from_slice(&bytes[start..]);
    }
    out.push(b'"');
}

#[inline]
pub fn write_u64(out: &mut Vec<u8>, mut n: u64) {
    if n == 0 {
        out.push(b'0');
        return;
    }
    let mut buf = [0u8; 20];
    let mut pos = 20;
    while n > 0 {
        pos -= 1;
        buf[pos] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    out.extend_from_slice(&buf[pos..]);
}

#[inline]
pub fn write_user_json(out: &mut Vec<u8>, u: &UserObj) {
    out.extend_from_slice(b"{\"id\":");
    write_u64(out, u.id);
    out.extend_from_slice(b",\"node_id\":");
    write_json_str(out, &u.node_id);
    out.extend_from_slice(b",\"login\":");
    write_json_str(out, &u.login);
    out.extend_from_slice(b",\"name\":");
    write_json_str(out, &u.name);
    out.extend_from_slice(b",\"email\":");
    write_json_str(out, &u.email);
    out.extend_from_slice(b",\"type\":\"User\"}");
}

#[inline]
pub fn write_label_json(out: &mut Vec<u8>, l: &Label) {
    out.extend_from_slice(b"{\"id\":");
    write_u64(out, l.id);
    out.extend_from_slice(b",\"node_id\":");
    write_json_str(out, &l.node_id);
    out.extend_from_slice(b",\"name\":");
    write_json_str(out, &l.name);
    out.extend_from_slice(b",\"color\":");
    write_json_str(out, &l.color);
    out.extend_from_slice(b",\"description\":");
    write_json_str(out, &l.description);
    out.push(b'}');
}

#[inline]
pub fn write_issue_json(out: &mut Vec<u8>, i: &Issue) {
    out.extend_from_slice(b"{\"id\":");
    write_u64(out, i.id);
    out.extend_from_slice(b",\"node_id\":");
    write_json_str(out, &i.node_id);
    out.extend_from_slice(b",\"number\":");
    write_u64(out, i.number);
    out.extend_from_slice(b",\"title\":");
    write_json_str(out, &i.title);
    out.extend_from_slice(b",\"body\":");
    write_json_str(out, &i.body);
    out.extend_from_slice(b",\"state\":");
    write_json_str(out, &i.state);
    out.extend_from_slice(b",\"labels\":[");
    for (idx, lbl) in i.labels.iter().enumerate() {
        if idx > 0 {
            out.push(b',');
        }
        write_label_json(out, lbl);
    }
    out.extend_from_slice(b"],\"user\":");
    write_user_json(out, &i.user);
    out.extend_from_slice(b",\"comments\":");
    write_u64(out, i.comments.len() as u64);
    out.extend_from_slice(b",\"created_at\":");
    write_json_str(out, &i.created_at);
    out.extend_from_slice(b",\"updated_at\":");
    write_json_str(out, &i.updated_at);
    out.extend_from_slice(b",\"closed_at\":");
    match &i.closed_at {
        Some(c) => write_json_str(out, c),
        None => out.extend_from_slice(b"null"),
    }
    out.push(b'}');
}

pub fn build_issue_http_200(issue: &Issue) -> Vec<u8> {
    let mut body = Vec::with_capacity(512 + issue.title.len() + issue.body.len());
    write_issue_json(&mut body, issue);
    body.push(b'\n');
    let mut resp = Vec::with_capacity(body.len() + 160);
    resp.extend_from_slice(
        b"HTTP/1.1 200 OK\r\ncontent-type: application/json; charset=utf-8\r\nx-github-request-id: r1\r\ncontent-length: ",
    );
    write_u64(&mut resp, body.len() as u64);
    resp.extend_from_slice(b"\r\nconnection: keep-alive\r\n\r\n");
    resp.extend_from_slice(&body);
    resp
}

pub fn format_user_val(u: &UserObj) -> Value {
    json!({
        "id": u.id,
        "node_id": u.node_id,
        "login": u.login,
        "name": u.name,
        "email": u.email,
        "type": "User",
    })
}

pub fn format_label_val(l: &Label) -> Value {
    json!({
        "id": l.id,
        "node_id": l.node_id,
        "name": l.name,
        "color": l.color,
        "description": l.description,
    })
}

pub fn format_comment_val(c: &IssueComment) -> Value {
    json!({
        "id": c.id,
        "node_id": c.node_id,
        "body": c.body,
        "user": format_user_val(&c.user),
        "created_at": c.created_at,
        "updated_at": c.updated_at,
    })
}

pub fn format_repo_val(r: &Repo) -> Value {
    json!({
        "id": r.id,
        "node_id": r.node_id,
        "name": r.name,
        "full_name": r.full_name,
        "private": r.private,
        "owner": format_user_val(&r.owner),
        "description": r.description,
        "default_branch": r.default_branch,
        "has_issues": r.has_issues,
        "created_at": r.created_at,
        "updated_at": r.updated_at,
    })
}

pub fn format_issue_val(i: &Issue) -> Value {
    json!({
        "id": i.id,
        "node_id": i.node_id,
        "number": i.number,
        "title": i.title,
        "body": i.body,
        "state": i.state,
        "labels": i.labels.iter().map(format_label_val).collect::<Vec<_>>(),
        "user": format_user_val(&i.user),
        "comments": i.comments.len(),
        "created_at": i.created_at,
        "updated_at": i.updated_at,
        "closed_at": i.closed_at,
    })
}

pub fn format_pull_val(repo: &Repo, p: &PullRequest) -> Value {
    json!({
        "id": p.id,
        "node_id": p.node_id,
        "number": p.number,
        "state": p.state,
        "title": p.title,
        "body": p.body,
        "head": { "ref": p.head_ref, "sha": oid_to_hex(&p.head_sha), "label": format!("{}:{}", repo.owner.login, p.head_ref) },
        "base": { "ref": p.base_ref, "sha": oid_to_hex(&p.base_sha), "label": format!("{}:{}", repo.owner.login, p.base_ref) },
        "merged": p.merged,
        "merged_at": p.merged_at,
        "merge_commit_sha": p.merge_commit_sha.as_ref().map(oid_to_hex),
        "mergeable": true,
        "user": format_user_val(&p.user),
        "created_at": p.created_at,
        "updated_at": p.updated_at,
        "closed_at": p.closed_at,
    })
}

#[inline]
pub fn write_http_json_raw(out: &mut Vec<u8>, status: &str, extra_headers: &str, body_bytes: &[u8]) {
    out.extend_from_slice(b"HTTP/1.1 ");
    out.extend_from_slice(status.as_bytes());
    out.extend_from_slice(b"\r\ncontent-type: application/json; charset=utf-8\r\nx-github-request-id: r1\r\n");
    if !extra_headers.is_empty() {
        out.extend_from_slice(extra_headers.as_bytes());
    }
    out.extend_from_slice(b"content-length: ");
    write_u64(out, body_bytes.len() as u64);
    out.extend_from_slice(b"\r\nconnection: keep-alive\r\n\r\n");
    out.extend_from_slice(body_bytes);
}

#[inline]
pub fn write_http_json(out: &mut Vec<u8>, status: &str, extra_headers: &str, val: &Value) {
    let mut s = serde_json::to_vec(val).unwrap();
    s.push(b'\n');
    write_http_json_raw(out, status, extra_headers, &s);
}

#[inline]
pub fn write_http_204(out: &mut Vec<u8>) {
    out.extend_from_slice(
        b"HTTP/1.1 204 No Content\r\ncontent-type: application/json; charset=utf-8\r\nx-github-request-id: r1\r\ncontent-length: 0\r\nconnection: keep-alive\r\n\r\n",
    );
}

pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    if !bytes.contains(&b'%') {
        return s.to_string();
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[inline]
fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn query_param<'a>(query: &'a str, key: &str) -> Option<String> {
    for part in query.split('&') {
        if let Some((k, v)) = part.split_once('=') {
            if k == key {
                return Some(percent_decode(v));
            }
        }
    }
    None
}

pub fn handle_rest_request(
    store_lock: &RwLock<ForgeStore>,
    method: &str,
    path: &str,
    query: &str,
    host: &str,
    auth: Option<&[u8]>,
    raw_body: &[u8],
    resp_out: &mut Vec<u8>,
) -> Option<Arc<Vec<u8>>> {
    // ULTRA-FAST PATH 1: GET /api/v3/repos/{owner}/{repo}/issues/{number}
    if method == "GET" && path.starts_with("/api/v3/repos/") {
        let tail = &path[14..];
        let mut parts = tail.split('/');
        if let (Some(o_raw), Some(r_raw), Some("issues"), Some(num_str), None) =
            (parts.next(), parts.next(), parts.next(), parts.next(), parts.next())
        {
            if let Ok(num) = num_str.parse::<u64>() {
                let owner = if o_raw.contains('%') { percent_decode(o_raw) } else { o_raw.to_string() };
                let repo_name = if r_raw.contains('%') { percent_decode(r_raw) } else { r_raw.to_string() };
                {
                    let store = store_lock.read().unwrap();
                    if !store.is_authorized(auth) {
                        write_http_json_raw(resp_out, "401 Unauthorized", "", b"{\"message\":\"Requires authentication\"}\n");
                        return None;
                    }
                    let repo = match store.get_repo(&owner, &repo_name) {
                        Some(r) => r,
                        None => {
                            write_http_json_raw(resp_out, "404 Not Found", "", b"{\"message\":\"Not Found\"}\n");
                            return None;
                        }
                    };
                    let issue = match repo.get_issue(num) {
                        Some(i) => i,
                        None => {
                            write_http_json_raw(resp_out, "404 Not Found", "", b"{\"message\":\"Not Found\"}\n");
                            return None;
                        }
                    };
                    if let Some(ref cached) = issue.cached_http_get {
                        return Some(Arc::clone(cached));
                    }
                }
                // Cache miss: build & store once under write lock!
                let mut store = store_lock.write().unwrap();
                if let Some(repo) = store.get_repo_mut(&owner, &repo_name) {
                    if let Some(issue) = repo.get_issue_mut(num) {
                        let built = Arc::new(build_issue_http_200(issue));
                        issue.cached_http_get = Some(Arc::clone(&built));
                        return Some(built);
                    }
                }
                write_http_json_raw(resp_out, "404 Not Found", "", b"{\"message\":\"Not Found\"}\n");
                return None;
            }
        }
    }

    // ULTRA-FAST PATH 2: POST /api/v3/repos/{owner}/{repo}/issues
    if method == "POST" && path.starts_with("/api/v3/repos/") && path.ends_with("/issues") {
        let tail = &path[14..];
        let mut parts = tail.split('/');
        if let (Some(o_raw), Some(r_raw), Some("issues"), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        {
            let owner = if o_raw.contains('%') { percent_decode(o_raw) } else { o_raw.to_string() };
            let repo_name = if r_raw.contains('%') { percent_decode(r_raw) } else { r_raw.to_string() };
            let mut store = store_lock.write().unwrap();
            if !store.is_authorized(auth) {
                write_http_json_raw(resp_out, "401 Unauthorized", "", b"{\"message\":\"Requires authentication\"}\n");
                return None;
            }
            let val: Value = if raw_body.is_empty() {
                json!({})
            } else {
                match serde_json::from_slice::<Value>(raw_body) {
                    Ok(v) if v.is_object() => v,
                    Ok(_) => {
                        write_http_json_raw(resp_out, "400 Bad Request", "", b"{\"message\":\"Body should be a JSON object\"}\n");
                        return None;
                    }
                    Err(_) => {
                        write_http_json_raw(resp_out, "400 Bad Request", "", b"{\"message\":\"Problems parsing JSON\"}\n");
                        return None;
                    }
                }
            };
            let repo_idx = match store.get_repo_idx(&owner, &repo_name) {
                Some(i) => i,
                None => {
                    write_http_json_raw(resp_out, "404 Not Found", "", b"{\"message\":\"Not Found\"}\n");
                    return None;
                }
            };
            let title = val.get("title").and_then(|v| v.as_str()).unwrap_or("");
            if title.is_empty() {
                write_http_json_raw(resp_out, "422 Unprocessable Entity", "", b"{\"message\":\"Title is required\"}\n");
                return None;
            }
            let body_s = val.get("body").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let labels: Vec<String> = val
                .get("labels")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default();
            match store.create_issue(repo_idx, title.to_string(), body_s, &labels) {
                Ok(num) => {
                    let issue = store.repos[repo_idx].get_issue(num).unwrap();
                    let mut json_body = Vec::with_capacity(384 + issue.title.len() + issue.body.len());
                    write_issue_json(&mut json_body, issue);
                    json_body.push(b'\n');
                    write_http_json_raw(resp_out, "201 Created", "", &json_body);
                }
                Err(bad) => {
                    write_http_json(resp_out, "422 Unprocessable Entity", "", &json!({ "message": format!("Label {} does not exist", bad) }));
                }
            }
            return None;
        }
    }

    if method == "GET" && (path == "/api/v3" || path == "/api/v3/") {
        write_http_json_raw(
            resp_out,
            "200 OK",
            "x-oauth-scopes: repo, read:org\r\n",
            b"{\"current_user_url\":\"/api/v3/user\",\"rate_limit_url\":\"/api/v3/rate_limit\"}\n",
        );
        return None;
    }

    if method == "GET" && path == "/api/v3/meta" {
        write_http_json_raw(
            resp_out,
            "200 OK",
            "",
            b"{\"installed_version\":\"3.15.0\",\"verifiable_password_authentication\":false}\n",
        );
        return None;
    }

    let body: Value = if !raw_body.is_empty() && method != "GET" && method != "HEAD" {
        match serde_json::from_slice::<Value>(raw_body) {
            Ok(v) if v.is_object() => v,
            Ok(_) => {
                let store = store_lock.read().unwrap();
                if !store.is_authorized(auth) {
                    write_http_json_raw(resp_out, "401 Unauthorized", "", b"{\"message\":\"Requires authentication\"}\n");
                    return None;
                }
                write_http_json_raw(resp_out, "400 Bad Request", "", b"{\"message\":\"Body should be a JSON object\"}\n");
                return None;
            }
            Err(_) => {
                let store = store_lock.read().unwrap();
                if !store.is_authorized(auth) {
                    write_http_json_raw(resp_out, "401 Unauthorized", "", b"{\"message\":\"Requires authentication\"}\n");
                    return None;
                }
                write_http_json_raw(resp_out, "400 Bad Request", "", b"{\"message\":\"Problems parsing JSON\"}\n");
                return None;
            }
        }
    } else {
        json!({})
    };

    let mut store = store_lock.write().unwrap();
    if !store.is_authorized(auth) {
        write_http_json_raw(resp_out, "401 Unauthorized", "", b"{\"message\":\"Requires authentication\"}\n");
        return None;
    }

    if method == "GET" && path == "/api/v3/user" {
        write_http_json(resp_out, "200 OK", "", &format_user_val(&store.viewer));
        return None;
    }

    if path == "/api/v3/user/repos" {
        if method == "GET" {
            let all: Vec<Value> = store.active_repos().into_iter().map(format_repo_val).collect();
            let per_page = query_param(query, "per_page")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(30)
                .clamp(1, 100);
            let page = query_param(query, "page")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(1)
                .max(1);
            let start = (page - 1) * per_page;
            let end = (start + per_page).min(all.len());
            let slice = if start < all.len() { &all[start..end] } else { &[] };
            let extra = if start + per_page < all.len() {
                let h = if host.is_empty() { "127.0.0.1" } else { host };
                format!("link: <http://{}/api/v3/user/repos?per_page={}&page={}>; rel=\"next\"\r\n", h, per_page, page + 1)
            } else {
                String::new()
            };
            write_http_json(resp_out, "200 OK", &extra, &Value::Array(slice.to_vec()));
            return None;
        }
        if method == "POST" {
            let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
            if name.is_empty() {
                write_http_json_raw(resp_out, "422 Unprocessable Entity", "", b"{\"message\":\"Repository name is required\"}\n");
                return None;
            }
            if store.repo_by_name.contains_key(&name) {
                write_http_json_raw(resp_out, "422 Unprocessable Entity", "", b"{\"message\":\"Repository already exists\"}\n");
                return None;
            }
            let desc = body.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let priv_flag = body.get("private").and_then(|v| v.as_bool()).unwrap_or(false);
            let def_br = body
                .get("default_branch")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("main")
                .to_string();
            let has_issues = body.get("has_issues").and_then(|v| v.as_bool()).unwrap_or(true);
            let idx = store.create_repo(name, desc, priv_flag, def_br, has_issues);
            write_http_json(resp_out, "201 Created", "", &format_repo_val(&store.repos[idx]));
            return None;
        }
    }

    let rest_after_repos = match path.strip_prefix("/api/v3/repos/") {
        Some(r) => r,
        None => {
            write_http_json_raw(resp_out, "404 Not Found", "", b"{\"message\":\"Not Found\"}\n");
            return None;
        }
    };
    let mut segs = rest_after_repos.splitn(3, '/');
    let owner = match segs.next() {
        Some(s) if !s.is_empty() => percent_decode(s),
        _ => {
            write_http_json_raw(resp_out, "404 Not Found", "", b"{\"message\":\"Not Found\"}\n");
            return None;
        }
    };
    let repo_name = match segs.next() {
        Some(s) if !s.is_empty() => percent_decode(s),
        _ => {
            write_http_json_raw(resp_out, "404 Not Found", "", b"{\"message\":\"Not Found\"}\n");
            return None;
        }
    };
    let sub_path = segs.next().unwrap_or("");
    let repo_idx = match store.get_repo_idx(&owner, &repo_name) {
        Some(i) => i,
        None => {
            write_http_json_raw(resp_out, "404 Not Found", "", b"{\"message\":\"Not Found\"}\n");
            return None;
        }
    };

    if sub_path.is_empty() {
        match method {
            "GET" => {
                write_http_json(resp_out, "200 OK", "", &format_repo_val(&store.repos[repo_idx]));
                return None;
            }
            "PATCH" => {
                let ts = store.next_timestamp();
                let repo = &mut store.repos[repo_idx];
                if let Some(d) = body.get("description").and_then(|v| v.as_str()) {
                    repo.description = d.to_string();
                }
                if let Some(db) = body.get("default_branch").and_then(|v| v.as_str()) {
                    if !db.is_empty() {
                        repo.default_branch = db.to_string();
                    }
                }
                if let Some(p) = body.get("private").and_then(|v| v.as_bool()) {
                    repo.private = p;
                }
                if let Some(hi) = body.get("has_issues").and_then(|v| v.as_bool()) {
                    repo.has_issues = hi;
                }
                repo.updated_at = ts;
                write_http_json(resp_out, "200 OK", "", &format_repo_val(&store.repos[repo_idx]));
                return None;
            }
            "DELETE" => {
                store.delete_repo(&owner, &repo_name);
                write_http_204(resp_out);
                return None;
            }
            _ => {}
        }
    }

    if sub_path == "git/refs" && method == "POST" {
        let r = body.get("ref").and_then(|v| v.as_str()).unwrap_or("");
        let sha_s = body.get("sha").and_then(|v| v.as_str()).unwrap_or("");
        let oid = crate::store::hex_to_oid(sha_s.as_bytes());
        if r.is_empty() || oid.is_none() || !store.repos[repo_idx].objects.contains_key(&oid.unwrap()) {
            write_http_json_raw(resp_out, "422 Unprocessable Entity", "", b"{\"message\":\"Invalid ref or sha\"}\n");
            return None;
        }
        let full_ref = if r.starts_with("refs/") {
            r.to_string()
        } else {
            format!("refs/heads/{}", r)
        };
        let oid = oid.unwrap();
        ForgeStore::set_ref(&mut store.repos[repo_idx], &full_ref, Some(oid));
        write_http_json(
            resp_out,
            "201 Created",
            "",
            &json!({ "ref": full_ref, "object": { "type": "commit", "sha": oid_to_hex(&oid) } }),
        );
        return None;
    }

    if sub_path == "branches" && method == "GET" {
        let repo = &store.repos[repo_idx];
        let mut branches = Vec::new();
        for (ref_name, oid) in &repo.refs {
            if let Some(bname) = ref_name.strip_prefix("refs/heads/") {
                branches.push(json!({ "name": bname, "commit": { "sha": oid_to_hex(oid) }, "protected": false }));
            }
        }
        write_http_json(resp_out, "200 OK", "", &Value::Array(branches));
        return None;
    }

    if sub_path == "commits" && method == "GET" {
        let repo = &store.repos[repo_idx];
        let start_ref = query_param(query, "sha");
        let tip = match ForgeStore::resolve_ref(repo, start_ref.as_deref()) {
            Some(t) => t,
            None => {
                write_http_json_raw(resp_out, "404 Not Found", "", b"{\"message\":\"Not Found\"}\n");
                return None;
            }
        };
        let per_page = query_param(query, "per_page")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(30)
            .clamp(1, 100);
        let mut list = Vec::new();
        let mut cur = Some(tip);
        let mut seen = rustc_hash::FxHashSet::default();
        while let Some(coid) = cur {
            if !seen.insert(coid) || list.len() >= per_page {
                break;
            }
            let obj = match repo.objects.get(&coid) {
                Some(o) if o.kind == GitObjectKind::Commit => o,
                _ => break,
            };
            let parsed = parse_commit(&obj.data);
            let parents: Vec<Value> = parsed.parents.iter().map(|p| json!({ "sha": oid_to_hex(p) })).collect();
            let next_p = parsed.parents.first().copied();
            list.push(json!({
                "sha": oid_to_hex(&coid),
                "commit": {
                    "message": parsed.message.strip_suffix('\n').unwrap_or(&parsed.message),
                    "author": { "name": store.viewer.name, "email": store.viewer.email, "date": store.now_iso },
                    "committer": { "name": store.viewer.name, "email": store.viewer.email, "date": store.now_iso },
                },
                "parents": parents,
            }));
            cur = next_p;
        }
        write_http_json(resp_out, "200 OK", "", &Value::Array(list));
        return None;
    }

    if sub_path == "contents" || sub_path.starts_with("contents/") {
        let file_path = if sub_path == "contents" {
            String::new()
        } else {
            percent_decode(&sub_path[9..]).trim_matches('/').to_string()
        };
        if method == "GET" {
            let repo = &store.repos[repo_idx];
            let qref = query_param(query, "ref");
            let tip_sha = ForgeStore::resolve_ref(repo, qref.as_deref());
            let tree_oid = match ForgeStore::tree_of_commit(repo, tip_sha) {
                Some(t) => t,
                None => {
                    write_http_json_raw(resp_out, "404 Not Found", "", b"{\"message\":\"Not Found\"}\n");
                    return None;
                }
            };
            if file_path.is_empty() {
                let tree_obj = match repo.objects.get(&tree_oid) {
                    Some(o) => o,
                    None => {
                        write_http_json_raw(resp_out, "404 Not Found", "", b"{\"message\":\"Not Found\"}\n");
                        return None;
                    }
                };
                let items: Vec<Value> = parse_tree(&tree_obj.data)
                    .into_iter()
                    .map(|e| {
                        let is_dir = e.mode == "40000" || e.mode == "040000";
                        let sz = if is_dir { 0 } else { repo.objects.get(&e.oid).map(|b| b.data.len()).unwrap_or(0) };
                        json!({
                            "name": e.name,
                            "path": e.name,
                            "sha": oid_to_hex(&e.oid),
                            "size": sz,
                            "type": if is_dir { "dir" } else { "file" },
                        })
                    })
                    .collect();
                write_http_json(resp_out, "200 OK", "", &Value::Array(items));
                return None;
            }
            let parts: Vec<&str> = file_path.split('/').collect();
            let mut cur_tree = tree_oid;
            for (i, &part) in parts.iter().enumerate() {
                let obj = match repo.objects.get(&cur_tree) {
                    Some(o) if o.kind == GitObjectKind::Tree => o,
                    _ => {
                        write_http_json_raw(resp_out, "404 Not Found", "", b"{\"message\":\"Not Found\"}\n");
                        return None;
                    }
                };
                let entry = match parse_tree(&obj.data).into_iter().find(|e| e.name == part) {
                    Some(e) => e,
                    None => {
                        write_http_json_raw(resp_out, "404 Not Found", "", b"{\"message\":\"Not Found\"}\n");
                        return None;
                    }
                };
                let is_dir = entry.mode == "40000" || entry.mode == "040000";
                if i == parts.len() - 1 {
                    if is_dir {
                        let items: Vec<Value> = repo
                            .objects
                            .get(&entry.oid)
                            .map(|st| {
                                parse_tree(&st.data)
                                    .into_iter()
                                    .map(|e| {
                                        let d = e.mode == "40000" || e.mode == "040000";
                                        let sz = if d { 0 } else { repo.objects.get(&e.oid).map(|b| b.data.len()).unwrap_or(0) };
                                        json!({
                                            "name": e.name,
                                            "path": format!("{}/{}", file_path, e.name),
                                            "sha": oid_to_hex(&e.oid),
                                            "size": sz,
                                            "type": if d { "dir" } else { "file" },
                                        })
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        write_http_json(resp_out, "200 OK", "", &Value::Array(items));
                        return None;
                    }
                    let blob = match repo.objects.get(&entry.oid) {
                        Some(b) => b,
                        None => {
                            write_http_json_raw(resp_out, "404 Not Found", "", b"{\"message\":\"Not Found\"}\n");
                            return None;
                        }
                    };
                    write_http_json(
                        resp_out,
                        "200 OK",
                        "",
                        &json!({
                            "name": entry.name,
                            "path": file_path,
                            "sha": oid_to_hex(&entry.oid),
                            "size": blob.data.len(),
                            "type": "file",
                            "encoding": "base64",
                            "content": base64_encode(&blob.data),
                        }),
                    );
                    return None;
                }
                if !is_dir {
                    write_http_json_raw(resp_out, "404 Not Found", "", b"{\"message\":\"Not Found\"}\n");
                    return None;
                }
                cur_tree = entry.oid;
            }
        }
        if method == "PUT" {
            let branch = body
                .get("branch")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from)
                .unwrap_or_else(|| store.repos[repo_idx].default_branch.clone());
            let content_b64 = body.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let message = body
                .get("message")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| format!("Update {}", file_path));
            let provided_sha = body.get("sha").and_then(|v| v.as_str());
            let existing_oid = {
                let repo = &store.repos[repo_idx];
                let tip = ForgeStore::resolve_ref(repo, Some(&branch));
                let mut flat = FxHashMap::default();
                ForgeStore::flatten_tree(repo, ForgeStore::tree_of_commit(repo, tip), "", &mut flat);
                flat.get(&file_path).map(|v| v.0)
            };
            if let Some(eoid) = existing_oid {
                let ehex = oid_to_hex(&eoid);
                if provided_sha != Some(ehex.as_str()) {
                    write_http_json(
                        resp_out,
                        "409 Conflict",
                        "",
                        &json!({ "message": format!("{} does not match {}", file_path, provided_sha.unwrap_or("none")) }),
                    );
                    return None;
                }
            }
            let raw_buf = base64_decode(content_b64.as_bytes());
            let raw_len = raw_buf.len();
            let (commit_sha, blob_sha) =
                store.commit_file_change(repo_idx, &branch, &file_path, Some(raw_buf), &message);
            let file_name = file_path.rsplit('/').next().unwrap_or(&file_path);
            let status = if existing_oid.is_some() { "200 OK" } else { "201 Created" };
            write_http_json(
                resp_out,
                status,
                "",
                &json!({
                    "content": {
                        "name": file_name,
                        "path": file_path,
                        "sha": blob_sha.as_ref().map(oid_to_hex),
                        "size": raw_len,
                        "type": "file",
                        "encoding": "base64",
                    },
                    "commit": { "sha": oid_to_hex(&commit_sha), "message": message },
                }),
            );
            return None;
        }
        if method == "DELETE" {
            let branch = body
                .get("branch")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from)
                .unwrap_or_else(|| store.repos[repo_idx].default_branch.clone());
            let message = body
                .get("message")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| format!("Delete {}", file_path));
            let provided_sha = body.get("sha").and_then(|v| v.as_str());
            let existing_oid = {
                let repo = &store.repos[repo_idx];
                let tip = ForgeStore::resolve_ref(repo, Some(&branch));
                let mut flat = FxHashMap::default();
                ForgeStore::flatten_tree(repo, ForgeStore::tree_of_commit(repo, tip), "", &mut flat);
                flat.get(&file_path).map(|v| v.0)
            };
            let eoid = match existing_oid {
                Some(o) => o,
                None => {
                    write_http_json_raw(resp_out, "404 Not Found", "", b"{\"message\":\"Not Found\"}\n");
                    return None;
                }
            };
            if let Some(ps) = provided_sha {
                if ps != oid_to_hex(&eoid) {
                    write_http_json(
                        resp_out,
                        "409 Conflict",
                        "",
                        &json!({ "message": format!("{} does not match {}", file_path, ps) }),
                    );
                    return None;
                }
            }
            let (commit_sha, _) = store.commit_file_change(repo_idx, &branch, &file_path, None, &message);
            write_http_json(
                resp_out,
                "200 OK",
                "",
                &json!({ "content": null, "commit": { "sha": oid_to_hex(&commit_sha), "message": message } }),
            );
            return None;
        }
    }

    if sub_path == "labels" {
        if method == "GET" {
            let list: Vec<Value> = store.repos[repo_idx].labels.iter().map(format_label_val).collect();
            write_http_json(resp_out, "200 OK", "", &Value::Array(list));
            return None;
        }
        if method == "POST" {
            let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
            if name.is_empty() {
                write_http_json_raw(resp_out, "422 Unprocessable Entity", "", b"{\"message\":\"Label name is required\"}\n");
                return None;
            }
            let color = body.get("color").and_then(|v| v.as_str()).unwrap_or("ededed").to_string();
            let desc = body.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let lbl = store.create_label(repo_idx, name, color, desc);
            write_http_json(resp_out, "201 Created", "", &format_label_val(&lbl));
            return None;
        }
    }

    if sub_path == "issues" && method == "GET" {
        let state_filter = query_param(query, "state").unwrap_or_else(|| "open".to_string());
        let direction = query_param(query, "direction").unwrap_or_else(|| "desc".to_string());
        let per_page = query_param(query, "per_page")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(30)
            .clamp(1, 100);
        let page = query_param(query, "page")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(1)
            .max(1);
        let mut items: Vec<&Issue> = store.repos[repo_idx]
            .issues
            .iter()
            .filter(|i| match state_filter.as_str() {
                "open" | "closed" => i.state == state_filter,
                _ => true,
            })
            .collect();
        if direction == "asc" {
            items.sort_by_key(|i| i.created_seq);
        } else {
            items.sort_by(|a, b| b.created_seq.cmp(&a.created_seq));
        }
        let start = (page - 1) * per_page;
        let end = (start + per_page).min(items.len());
        let slice = if start < items.len() { &items[start..end] } else { &[] };
        let vals: Vec<Value> = slice.iter().map(|i| format_issue_val(i)).collect();
        write_http_json(resp_out, "200 OK", "", &Value::Array(vals));
        return None;
    }

    if let Some(after_issues) = sub_path.strip_prefix("issues/") {
        let mut parts = after_issues.split('/');
        if let Some(num_str) = parts.next() {
            if let Ok(num) = num_str.parse::<u64>() {
                let sub = parts.next();
                if parts.next().is_none() {
                    if store.repos[repo_idx].get_issue(num).is_none() {
                        write_http_json_raw(resp_out, "404 Not Found", "", b"{\"message\":\"Not Found\"}\n");
                        return None;
                    }
                    match sub {
                        Some("comments") => {
                            if method == "GET" {
                                let comments: Vec<Value> = store.repos[repo_idx]
                                    .get_issue(num)
                                    .unwrap()
                                    .comments
                                    .iter()
                                    .map(format_comment_val)
                                    .collect();
                                write_http_json(resp_out, "200 OK", "", &Value::Array(comments));
                                return None;
                            }
                            if method == "POST" {
                                let cbody = body.get("body").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let c = store.add_issue_comment(repo_idx, num, cbody).unwrap();
                                write_http_json(resp_out, "201 Created", "", &format_comment_val(&c));
                                return None;
                            }
                        }
                        None => {
                            if method == "PATCH" {
                                let new_state = body.get("state").and_then(|v| v.as_str()).filter(|&s| s == "open" || s == "closed").map(String::from);
                                let closed_ts = if new_state.as_deref() == Some("closed") {
                                    Some(store.next_timestamp())
                                } else {
                                    None
                                };
                                let upd_ts = store.next_timestamp();
                                let issue = store.repos[repo_idx].get_issue_mut(num).unwrap();
                                if let Some(t) = body.get("title").and_then(|v| v.as_str()) {
                                    issue.title = t.to_string();
                                }
                                if let Some(b) = body.get("body").and_then(|v| v.as_str()) {
                                    issue.body = b.to_string();
                                }
                                if let Some(st) = new_state {
                                    issue.state = st;
                                    issue.closed_at = closed_ts;
                                }
                                issue.updated_at = upd_ts;
                                issue.cached_http_get = None;
                                write_http_json(resp_out, "200 OK", "", &format_issue_val(issue));
                                return None;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    if sub_path == "pulls" {
        if method == "GET" {
            let state_filter = query_param(query, "state").unwrap_or_else(|| "open".to_string());
            let repo = &store.repos[repo_idx];
            let items: Vec<Value> = repo
                .pulls
                .iter()
                .filter(|p| match state_filter.as_str() {
                    "open" | "closed" => p.state == state_filter,
                    _ => true,
                })
                .map(|p| format_pull_val(repo, p))
                .collect();
            write_http_json(resp_out, "200 OK", "", &Value::Array(items));
            return None;
        }
        if method == "POST" {
            let title = body.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let head = body.get("head").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let base = body
                .get("base")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from)
                .unwrap_or_else(|| store.repos[repo_idx].default_branch.clone());
            if title.is_empty() || head.is_empty() {
                write_http_json_raw(resp_out, "422 Unprocessable Entity", "", b"{\"message\":\"title and head are required\"}\n");
                return None;
            }
            let pbody = body.get("body").and_then(|v| v.as_str()).unwrap_or("").to_string();
            match store.create_pull(repo_idx, title, pbody, head, base) {
                Ok(num) => {
                    let repo = &store.repos[repo_idx];
                    let pull = repo.get_pull(num).unwrap();
                    write_http_json(resp_out, "201 Created", "", &format_pull_val(repo, pull));
                }
                Err(e) => {
                    write_http_json(resp_out, "422 Unprocessable Entity", "", &json!({ "message": e }));
                }
            }
            return None;
        }
    }

    if let Some(after_pulls) = sub_path.strip_prefix("pulls/") {
        let mut parts = after_pulls.split('/');
        if let Some(num_str) = parts.next() {
            if let Ok(num) = num_str.parse::<u64>() {
                let action = parts.next();
                if parts.next().is_none() {
                    if store.repos[repo_idx].get_pull(num).is_none() {
                        write_http_json_raw(resp_out, "404 Not Found", "", b"{\"message\":\"Not Found\"}\n");
                        return None;
                    }
                    match (action, method) {
                        (None, "GET") => {
                            let repo = &store.repos[repo_idx];
                            let pull = repo.get_pull(num).unwrap();
                            write_http_json(resp_out, "200 OK", "", &format_pull_val(repo, pull));
                            return None;
                        }
                        (Some("files"), "GET") => {
                            let repo = &store.repos[repo_idx];
                            let pull = repo.get_pull(num).unwrap();
                            let diffs = ForgeStore::diff_commits(repo, pull.base_sha, pull.head_sha);
                            let arr: Vec<Value> = diffs
                                .into_iter()
                                .map(|d| {
                                    json!({
                                        "filename": d.filename,
                                        "status": d.status,
                                        "additions": d.additions,
                                        "deletions": d.deletions,
                                        "changes": d.changes,
                                    })
                                })
                                .collect();
                            write_http_json(resp_out, "200 OK", "", &Value::Array(arr));
                            return None;
                        }
                        (Some("merge"), "PUT") => {
                            if store.repos[repo_idx].get_pull(num).unwrap().merged {
                                write_http_json_raw(resp_out, "405 Method Not Allowed", "", b"{\"message\":\"Pull Request is already merged\"}\n");
                                return None;
                            }
                            let msg = body.get("commit_message").and_then(|v| v.as_str());
                            match store.squash_merge(repo_idx, num, msg) {
                                Ok(sha) => {
                                    write_http_json(
                                        resp_out,
                                        "200 OK",
                                        "",
                                        &json!({ "sha": oid_to_hex(&sha), "merged": true, "message": "Pull Request successfully merged" }),
                                    );
                                }
                                Err(_) => {
                                    write_http_json_raw(resp_out, "405 Method Not Allowed", "", b"{\"message\":\"Pull Request is not mergeable\"}\n");
                                }
                            }
                            return None;
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    write_http_json_raw(resp_out, "404 Not Found", "", b"{\"message\":\"Not Found\"}\n");
    None
}
