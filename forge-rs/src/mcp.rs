use crate::rest::{
    format_issue_val, format_pull_val, format_repo_val, write_http_204, write_http_json,
    write_http_json_raw,
};
use crate::store::{oid_to_hex, ForgeStore};
use serde_json::{json, Map, Value};

fn tools_list_value() -> Value {
    json!([
        {
            "name": "create_repo",
            "description": "Create a new repository for the authenticated user",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "description": { "type": "string" },
                    "private": { "type": "boolean" }
                },
                "required": ["name"]
            }
        },
        {
            "name": "get_repo",
            "description": "Get details of a repository",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string" },
                    "repo": { "type": "string" }
                },
                "required": ["owner", "repo"]
            }
        },
        {
            "name": "create_issue",
            "description": "Create an issue in a repository",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string" },
                    "repo": { "type": "string" },
                    "title": { "type": "string" },
                    "body": { "type": "string" }
                },
                "required": ["owner", "repo", "title"]
            }
        },
        {
            "name": "list_issues",
            "description": "List issues in a repository",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string" },
                    "repo": { "type": "string" },
                    "state": { "type": "string" }
                },
                "required": ["owner", "repo"]
            }
        },
        {
            "name": "create_pull_request",
            "description": "Create a pull request in a repository",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string" },
                    "repo": { "type": "string" },
                    "title": { "type": "string" },
                    "head": { "type": "string" },
                    "base": { "type": "string" },
                    "body": { "type": "string" }
                },
                "required": ["owner", "repo", "title", "head", "base"]
            }
        },
        {
            "name": "merge_pull_request",
            "description": "Squash-merge a pull request",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string" },
                    "repo": { "type": "string" },
                    "pull_number": { "type": "number" }
                },
                "required": ["owner", "repo", "pull_number"]
            }
        }
    ])
}

fn tool_result(data: Value, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": serde_json::to_string(&data).unwrap() }],
        "isError": is_error,
    })
}

fn resolve_repo_args(store: &ForgeStore, args: &Map<String, Value>) -> Option<usize> {
    let mut owner = args.get("owner").and_then(|v| v.as_str()).unwrap_or("");
    let mut repo_name = args.get("repo").and_then(|v| v.as_str()).unwrap_or("");
    if owner.is_empty() {
        if let Some(full) = args.get("full_name").and_then(|v| v.as_str()) {
            if let Some((o, r)) = full.split_once('/') {
                owner = o;
                repo_name = r;
            }
        }
    }
    store.get_repo_idx(owner, repo_name)
}

pub fn handle_mcp_request(
    store: &mut ForgeStore,
    auth: Option<&[u8]>,
    raw_body: &[u8],
    resp_out: &mut Vec<u8>,
) {
    if !store.is_authorized(auth) {
        write_http_json_raw(resp_out, "401 Unauthorized", "", b"{\"message\":\"Requires authentication\"}\n");
        return;
    }

    let msg: Map<String, Value> = match serde_json::from_slice::<Value>(raw_body) {
        Ok(Value::Object(m)) => m,
        Ok(_) => {
            write_http_json(
                resp_out,
                "400 Bad Request",
                "",
                &json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32600, "message": "Invalid Request" } }),
            );
            return;
        }
        Err(_) => {
            write_http_json(
                resp_out,
                "400 Bad Request",
                "",
                &json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "Parse error" } }),
            );
            return;
        }
    };

    let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let has_id = msg.contains_key("id");
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let empty_map = Map::new();
    let params = msg.get("params").and_then(|v| v.as_object()).unwrap_or(&empty_map);

    if method == "notifications/initialized" || !has_id {
        write_http_204(resp_out);
        return;
    }

    let send_ok = |out: &mut Vec<u8>, result: Value| {
        write_http_json(
            out,
            "200 OK",
            "mcp-session-id: session-1\r\n",
            &json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        );
    };
    let send_err = |out: &mut Vec<u8>, code: i32, message: String| {
        write_http_json(
            out,
            "200 OK",
            "",
            &json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
        );
    };

    match method {
        "server/discover" => {
            send_ok(
                resp_out,
                json!({
                    "protocolVersion": "2026-07-28",
                    "supportedVersions": ["2026-07-28", "2025-11-25"],
                    "serverInfo": { "name": "github-core-clone", "version": "1.0.0" },
                    "capabilities": { "tools": { "listChanged": false } },
                }),
            );
        }
        "initialize" => {
            let pv = params
                .get("protocolVersion")
                .and_then(|v| v.as_str())
                .unwrap_or("2025-11-25");
            send_ok(
                resp_out,
                json!({
                    "protocolVersion": pv,
                    "serverInfo": { "name": "github-core-clone", "version": "1.0.0" },
                    "capabilities": { "tools": { "listChanged": false } },
                }),
            );
        }
        "tools/list" => {
            send_ok(resp_out, json!({ "tools": tools_list_value() }));
        }
        "tools/call" => {
            let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params
                .get("arguments")
                .and_then(|v| v.as_object())
                .unwrap_or(&empty_map);

            match tool_name {
                "create_repo" => {
                    let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
                    if name.is_empty() {
                        send_ok(resp_out, tool_result(json!({ "error": "name is required" }), true));
                        return;
                    }
                    if store.repo_by_name.contains_key(&name) {
                        send_ok(resp_out, tool_result(json!({ "error": "Repository already exists" }), true));
                        return;
                    }
                    let desc = args.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let priv_flag = args.get("private").and_then(|v| v.as_bool()).unwrap_or(false);
                    let idx = store.create_repo(name, desc, priv_flag, "main".to_string(), true);
                    send_ok(resp_out, tool_result(format_repo_val(&store.repos[idx]), false));
                }
                "get_repo" => {
                    let owner = args.get("owner").and_then(|v| v.as_str()).unwrap_or("");
                    let repo_name = args.get("repo").and_then(|v| v.as_str()).unwrap_or("");
                    if owner.is_empty() || repo_name.is_empty() {
                        send_ok(resp_out, tool_result(json!({ "error": "owner and repo are required" }), true));
                        return;
                    }
                    match resolve_repo_args(store, args) {
                        Some(idx) => send_ok(resp_out, tool_result(format_repo_val(&store.repos[idx]), false)),
                        None => send_ok(resp_out, tool_result(json!({ "error": "Repository not found" }), true)),
                    }
                }
                "create_issue" => {
                    let owner = args.get("owner").and_then(|v| v.as_str()).unwrap_or("");
                    let repo_name = args.get("repo").and_then(|v| v.as_str()).unwrap_or("");
                    let title = args.get("title").and_then(|v| v.as_str()).unwrap_or("");
                    if owner.is_empty() || repo_name.is_empty() || title.is_empty() {
                        send_ok(resp_out, tool_result(json!({ "error": "owner, repo, and title are required" }), true));
                        return;
                    }
                    let idx = match resolve_repo_args(store, args) {
                        Some(i) => i,
                        None => {
                            send_ok(resp_out, tool_result(json!({ "error": "Repository not found" }), true));
                            return;
                        }
                    };
                    let body = args.get("body").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    match store.create_issue(idx, title.to_string(), body, &[]) {
                        Ok(num) => {
                            let issue = store.repos[idx].get_issue(num).unwrap();
                            send_ok(resp_out, tool_result(format_issue_val(issue), false));
                        }
                        Err(_) => send_ok(resp_out, tool_result(json!({ "error": "Invalid label" }), true)),
                    }
                }
                "list_issues" => {
                    let owner = args.get("owner").and_then(|v| v.as_str()).unwrap_or("");
                    let repo_name = args.get("repo").and_then(|v| v.as_str()).unwrap_or("");
                    if owner.is_empty() || repo_name.is_empty() {
                        send_ok(resp_out, tool_result(json!({ "error": "owner and repo are required" }), true));
                        return;
                    }
                    let idx = match resolve_repo_args(store, args) {
                        Some(i) => i,
                        None => {
                            send_ok(resp_out, tool_result(json!({ "error": "Repository not found" }), true));
                            return;
                        }
                    };
                    let state_filter = args.get("state").and_then(|v| v.as_str()).unwrap_or("open");
                    let list: Vec<Value> = store.repos[idx]
                        .issues
                        .iter()
                        .filter(|i| match state_filter {
                            "open" | "closed" => i.state == state_filter,
                            _ => true,
                        })
                        .map(format_issue_val)
                        .collect();
                    send_ok(resp_out, tool_result(Value::Array(list), false));
                }
                "create_pull_request" => {
                    let owner = args.get("owner").and_then(|v| v.as_str()).unwrap_or("");
                    let repo_name = args.get("repo").and_then(|v| v.as_str()).unwrap_or("");
                    let title = args.get("title").and_then(|v| v.as_str()).unwrap_or("");
                    let head = args.get("head").and_then(|v| v.as_str()).unwrap_or("");
                    let base = args.get("base").and_then(|v| v.as_str()).unwrap_or("");
                    if owner.is_empty() || repo_name.is_empty() || title.is_empty() || head.is_empty() || base.is_empty() {
                        send_ok(resp_out, tool_result(json!({ "error": "owner, repo, title, head, and base are required" }), true));
                        return;
                    }
                    let idx = match resolve_repo_args(store, args) {
                        Some(i) => i,
                        None => {
                            send_ok(resp_out, tool_result(json!({ "error": "Repository not found" }), true));
                            return;
                        }
                    };
                    let body = args.get("body").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    match store.create_pull(idx, title.to_string(), body, head.to_string(), base.to_string()) {
                        Ok(num) => {
                            let repo = &store.repos[idx];
                            let pull = repo.get_pull(num).unwrap();
                            send_ok(resp_out, tool_result(format_pull_val(repo, pull), false));
                        }
                        Err(e) => send_ok(resp_out, tool_result(json!({ "error": e }), true)),
                    }
                }
                "merge_pull_request" => {
                    let owner = args.get("owner").and_then(|v| v.as_str()).unwrap_or("");
                    let repo_name = args.get("repo").and_then(|v| v.as_str()).unwrap_or("");
                    let pull_num = args.get("pull_number").and_then(|v| v.as_u64());
                    if owner.is_empty() || repo_name.is_empty() || pull_num.is_none() {
                        send_ok(resp_out, tool_result(json!({ "error": "owner, repo, and pull_number are required" }), true));
                        return;
                    }
                    let idx = match resolve_repo_args(store, args) {
                        Some(i) => i,
                        None => {
                            send_ok(resp_out, tool_result(json!({ "error": "Pull request not found" }), true));
                            return;
                        }
                    };
                    let num = pull_num.unwrap();
                    if store.repos[idx].get_pull(num).is_none() {
                        send_ok(resp_out, tool_result(json!({ "error": "Pull request not found" }), true));
                        return;
                    }
                    match store.squash_merge(idx, num, None) {
                        Ok(sha) => send_ok(
                            resp_out,
                            tool_result(
                                json!({ "sha": oid_to_hex(&sha), "merged": true, "message": "Pull Request successfully merged" }),
                                false,
                            ),
                        ),
                        Err(_) => send_ok(resp_out, tool_result(json!({ "error": "Merge conflict" }), true)),
                    }
                }
                _ => send_err(resp_out, -32601, format!("Unknown tool: {}", tool_name)),
            }
        }
        _ => send_err(resp_out, -32601, format!("Method not found: {}", method)),
    }
}
