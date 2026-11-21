use crate::rest::{write_http_json, write_http_json_raw};
use crate::store::{decode_node_id, oid_to_hex, ForgeStore, Issue, PullRequest, Repo};
use rustc_hash::FxHashMap;
use serde_json::{json, Map, Value};

#[derive(Clone, Debug)]
struct FieldNode {
    name: String,
    alias: Option<String>,
    args: FxHashMap<String, Value>,
    selections: Vec<Selection>,
}

#[derive(Clone, Debug)]
enum Selection {
    Field(FieldNode),
    InlineFragment {
        type_condition: Option<String>,
        selections: Vec<Selection>,
    },
    FragmentSpread(String),
}

#[derive(Clone, Debug)]
struct OperationDef {
    op_type: String,
    name: Option<String>,
    selections: Vec<Selection>,
}

struct ParsedDoc {
    operations: Vec<OperationDef>,
    fragments: FxHashMap<String, Vec<Selection>>,
}

fn introspection_fields(type_name: &str) -> Option<&'static [&'static str]> {
    match type_name {
        "Issue" => Some(&["id", "number", "title", "state", "body", "createdAt", "updatedAt", "closedAt", "comments"]),
        "PullRequest" => Some(&[
            "id", "number", "title", "body", "headRefName", "baseRefName", "mergeable", "state", "merged", "files", "mergeCommit",
        ]),
        "Repository" => Some(&[
            "id", "databaseId", "name", "nameWithOwner", "owner", "defaultBranchRef", "viewerPermission",
            "hasIssuesEnabled", "issue", "issues", "pullRequests", "pullRequest",
        ]),
        _ => None,
    }
}

fn introspection_enums(type_name: &str) -> Option<&'static [&'static str]> {
    match type_name {
        "SearchType" => Some(&["ISSUE", "REPOSITORY", "USER", "DISCUSSION"]),
        _ => None,
    }
}

fn is_valid_field(type_name: &str, field_name: &str) -> bool {
    if field_name == "__typename" {
        return true;
    }
    match type_name {
        "Query" => matches!(field_name, "viewer" | "repository" | "node" | "__type"),
        "User" => matches!(field_name, "id" | "login" | "name"),
        "Repository" => introspection_fields("Repository").unwrap().contains(&field_name),
        "Issue" => introspection_fields("Issue").unwrap().contains(&field_name),
        "PullRequest" => introspection_fields("PullRequest").unwrap().contains(&field_name),
        "Ref" => field_name == "name",
        "IssueConnection" | "PullRequestConnection" => matches!(field_name, "nodes" | "pageInfo" | "totalCount"),
        "PullRequestFileConnection" => field_name == "nodes",
        "PullRequestFile" => matches!(field_name, "path" | "additions" | "deletions"),
        "PageInfo" => matches!(field_name, "hasNextPage" | "endCursor"),
        "__Type" => matches!(field_name, "name" | "fields" | "enumValues"),
        "__Field" | "__EnumValue" => field_name == "name",
        "Mutation" => matches!(
            field_name,
            "createIssue" | "closeIssue" | "createPullRequest" | "mergePullRequest" | "addComment"
        ),
        _ => true,
    }
}

fn tokenize(src: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i];
        if matches!(ch, b' ' | b'\t' | b'\n' | b'\r' | b',') {
            i += 1;
            continue;
        }
        if ch == b'#' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if ch == b'.' && i + 2 < bytes.len() && &bytes[i..i + 3] == b"..." {
            tokens.push("...".to_string());
            i += 3;
            continue;
        }
        if b"{}():[]=!@$".contains(&ch) {
            tokens.push((ch as char).to_string());
            i += 1;
            continue;
        }
        if ch == b'"' {
            let mut j = i + 1;
            let mut s = String::new();
            while j < bytes.len() && bytes[j] != b'"' {
                if bytes[j] == b'\\' && j + 1 < bytes.len() {
                    let esc = bytes[j + 1];
                    match esc {
                        b'n' => s.push('\n'),
                        b't' => s.push('\t'),
                        b'r' => s.push('\r'),
                        other => s.push(other as char),
                    }
                    j += 2;
                } else {
                    let ch_str = &src[j..];
                    let c = ch_str.chars().next().unwrap();
                    s.push(c);
                    j += c.len_utf8();
                }
            }
            tokens.push(serde_json::to_string(&s).unwrap());
            i = (j + 1).min(bytes.len());
            continue;
        }
        let start = i;
        while i < bytes.len() && !b" \t\n\r,{}():[]=!@#$\"".contains(&bytes[i]) {
            i += 1;
        }
        tokens.push(src[start..i].to_string());
    }
    tokens
}

struct Parser<'a> {
    tokens: Vec<String>,
    pos: usize,
    vars: &'a Map<String, Value>,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.pos).map(String::as_str)
    }

    fn next(&mut self) -> Option<String> {
        if self.pos < self.tokens.len() {
            let t = self.tokens[self.pos].clone();
            self.pos += 1;
            Some(t)
        } else {
            None
        }
    }

    fn parse_value(&mut self) -> Value {
        let t = match self.next() {
            Some(v) => v,
            None => return Value::Null,
        };
        if t == "$" {
            let var_name = self.next().unwrap_or_default();
            return self.vars.get(&var_name).cloned().unwrap_or(Value::Null);
        }
        if t == "{" {
            let mut obj = Map::new();
            while let Some(pk) = self.peek() {
                if pk == "}" {
                    self.next();
                    break;
                }
                let k = self.next().unwrap_or_default();
                if self.peek() == Some(":") {
                    self.next();
                }
                let v = self.parse_value();
                obj.insert(k, v);
            }
            return Value::Object(obj);
        }
        if t == "[" {
            let mut arr = Vec::new();
            while let Some(pk) = self.peek() {
                if pk == "]" {
                    self.next();
                    break;
                }
                arr.push(self.parse_value());
            }
            return Value::Array(arr);
        }
        if t.starts_with('"') {
            return serde_json::from_str(&t).unwrap_or(Value::String(t));
        }
        if t == "true" {
            return Value::Bool(true);
        }
        if t == "false" {
            return Value::Bool(false);
        }
        if t == "null" {
            return Value::Null;
        }
        if let Ok(n) = t.parse::<i64>() {
            return Value::Number(n.into());
        }
        if let Ok(f) = t.parse::<f64>() {
            if let Some(num) = serde_json::Number::from_f64(f) {
                return Value::Number(num);
            }
        }
        Value::String(t)
    }

    fn parse_args(&mut self) -> FxHashMap<String, Value> {
        let mut args = FxHashMap::default();
        if self.peek() != Some("(") {
            return args;
        }
        self.next();
        while let Some(pk) = self.peek() {
            if pk == ")" {
                self.next();
                break;
            }
            let k = self.next().unwrap_or_default();
            if self.peek() == Some(":") {
                self.next();
            }
            let v = self.parse_value();
            args.insert(k, v);
        }
        args
    }

    fn skip_directives(&mut self) {
        while self.peek() == Some("@") {
            self.next();
            self.next();
            if self.peek() == Some("(") {
                let _ = self.parse_args();
            }
        }
    }

    fn parse_selection_set(&mut self) -> Vec<Selection> {
        let mut sels = Vec::new();
        if self.peek() != Some("{") {
            return sels;
        }
        self.next();
        while let Some(pk) = self.peek() {
            if pk == "}" {
                self.next();
                break;
            }
            if pk == "..." {
                self.next();
                if self.peek() == Some("on") {
                    self.next();
                    let type_cond = self.next();
                    self.skip_directives();
                    let sub = self.parse_selection_set();
                    sels.push(Selection::InlineFragment {
                        type_condition: type_cond,
                        selections: sub,
                    });
                } else if self.peek() == Some("{") {
                    let sub = self.parse_selection_set();
                    sels.push(Selection::InlineFragment {
                        type_condition: None,
                        selections: sub,
                    });
                } else {
                    let fname = self.next().unwrap_or_default();
                    self.skip_directives();
                    sels.push(Selection::FragmentSpread(fname));
                }
                continue;
            }
            let first = self.next().unwrap_or_default();
            let (alias, name) = if self.peek() == Some(":") {
                self.next();
                let real = self.next().unwrap_or_default();
                (Some(first), real)
            } else {
                (None, first)
            };
            let args = self.parse_args();
            self.skip_directives();
            let sub = if self.peek() == Some("{") {
                self.parse_selection_set()
            } else {
                Vec::new()
            };
            sels.push(Selection::Field(FieldNode {
                name,
                alias,
                args,
                selections: sub,
            }));
        }
        sels
    }

    fn skip_var_defs(&mut self) {
        if self.peek() != Some("(") {
            return;
        }
        self.next();
        let mut depth = 1;
        while depth > 0 {
            match self.next().as_deref() {
                Some("(") => depth += 1,
                Some(")") => depth -= 1,
                None => break,
                _ => {}
            }
        }
    }

    fn parse_doc(&mut self) -> ParsedDoc {
        let mut operations = Vec::new();
        let mut fragments = FxHashMap::default();
        while let Some(pk) = self.peek() {
            if pk == "fragment" {
                self.next();
                let fname = self.next().unwrap_or_default();
                if self.peek() == Some("on") {
                    self.next();
                    self.next();
                }
                self.skip_directives();
                let sels = self.parse_selection_set();
                fragments.insert(fname, sels);
            } else if pk == "query" || pk == "mutation" {
                let op_type = self.next().unwrap();
                let name = match self.peek() {
                    Some(t) if t != "(" && t != "{" && t != "@" => self.next(),
                    _ => None,
                };
                self.skip_var_defs();
                self.skip_directives();
                let sels = self.parse_selection_set();
                operations.push(OperationDef {
                    op_type,
                    name,
                    selections: sels,
                });
            } else if pk == "{" {
                let sels = self.parse_selection_set();
                operations.push(OperationDef {
                    op_type: "query".to_string(),
                    name: None,
                    selections: sels,
                });
            } else {
                self.next();
            }
        }
        ParsedDoc {
            operations,
            fragments,
        }
    }
}

fn expand_selections(
    sels: &[Selection],
    fragments: &FxHashMap<String, Vec<Selection>>,
    type_name: Option<&str>,
) -> Vec<FieldNode> {
    let mut out = Vec::new();
    for s in sels {
        match s {
            Selection::Field(f) => out.push(f.clone()),
            Selection::InlineFragment {
                type_condition,
                selections,
            } => {
                if type_condition.is_none() || type_name.is_none() || type_condition.as_deref() == type_name {
                    out.extend(expand_selections(selections, fragments, type_name));
                }
            }
            Selection::FragmentSpread(name) => {
                if let Some(f_sels) = fragments.get(name) {
                    out.extend(expand_selections(f_sels, fragments, type_name));
                }
            }
        }
    }
    out
}

fn resolve_issue(
    issue: &Issue,
    sels: &[Selection],
    fragments: &FxHashMap<String, Vec<Selection>>,
    errors: &mut Vec<Value>,
) -> Value {
    let mut res = Map::new();
    for f in expand_selections(sels, fragments, Some("Issue")) {
        let key = f.alias.unwrap_or_else(|| f.name.clone());
        if !is_valid_field("Issue", &f.name) {
            errors.push(json!({ "message": format!("Field '{}' doesn't exist on type 'Issue'", f.name) }));
            continue;
        }
        match f.name.as_str() {
            "__typename" => { res.insert(key, json!("Issue")); }
            "id" => { res.insert(key, json!(issue.node_id)); }
            "number" => { res.insert(key, json!(issue.number)); }
            "title" => { res.insert(key, json!(issue.title)); }
            "body" => { res.insert(key, json!(issue.body)); }
            "state" => { res.insert(key, json!(issue.state.to_uppercase())); }
            "createdAt" => { res.insert(key, json!(issue.created_at)); }
            "updatedAt" => { res.insert(key, json!(issue.updated_at)); }
            "closedAt" => { res.insert(key, json!(issue.closed_at)); }
            "comments" => {
                let mut conn = Map::new();
                for sf in expand_selections(&f.selections, fragments, None) {
                    let skey = sf.alias.unwrap_or_else(|| sf.name.clone());
                    if sf.name == "nodes" {
                        let nodes: Vec<Value> = issue
                            .comments
                            .iter()
                            .map(|c| {
                                let mut obj = Map::new();
                                for cf in expand_selections(&sf.selections, fragments, None) {
                                    let ckey = cf.alias.unwrap_or_else(|| cf.name.clone());
                                    match cf.name.as_str() {
                                        "id" => { obj.insert(ckey, json!(c.node_id)); }
                                        "body" => { obj.insert(ckey, json!(c.body)); }
                                        "createdAt" => { obj.insert(ckey, json!(c.created_at)); }
                                        _ => {}
                                    }
                                }
                                Value::Object(obj)
                            })
                            .collect();
                        conn.insert(skey, Value::Array(nodes));
                    } else if sf.name == "totalCount" {
                        conn.insert(skey, json!(issue.comments.len()));
                    }
                }
                res.insert(key, Value::Object(conn));
            }
            _ => {}
        }
    }
    Value::Object(res)
}

fn resolve_pull(
    repo: &Repo,
    pull: &PullRequest,
    sels: &[Selection],
    fragments: &FxHashMap<String, Vec<Selection>>,
    errors: &mut Vec<Value>,
) -> Value {
    let mut res = Map::new();
    for f in expand_selections(sels, fragments, Some("PullRequest")) {
        let key = f.alias.unwrap_or_else(|| f.name.clone());
        if !is_valid_field("PullRequest", &f.name) {
            errors.push(json!({ "message": format!("Field '{}' doesn't exist on type 'PullRequest'", f.name) }));
            continue;
        }
        match f.name.as_str() {
            "__typename" => { res.insert(key, json!("PullRequest")); }
            "id" => { res.insert(key, json!(pull.node_id)); }
            "number" => { res.insert(key, json!(pull.number)); }
            "title" => { res.insert(key, json!(pull.title)); }
            "body" => { res.insert(key, json!(pull.body)); }
            "headRefName" => { res.insert(key, json!(pull.head_ref)); }
            "baseRefName" => { res.insert(key, json!(pull.base_ref)); }
            "mergeable" => { res.insert(key, json!("MERGEABLE")); }
            "merged" => { res.insert(key, json!(pull.merged)); }
            "state" => {
                let st = if pull.merged { "MERGED" } else if pull.state == "closed" { "CLOSED" } else { "OPEN" };
                res.insert(key, json!(st));
            }
            "mergeCommit" => {
                if let Some(ref oid) = pull.merge_commit_sha {
                    let mut mc = Map::new();
                    let hex = oid_to_hex(oid);
                    for sf in expand_selections(&f.selections, fragments, None) {
                        let skey = sf.alias.unwrap_or_else(|| sf.name.clone());
                        if sf.name == "oid" {
                            mc.insert(skey, json!(hex));
                        }
                    }
                    res.insert(key, Value::Object(mc));
                } else {
                    res.insert(key, Value::Null);
                }
            }
            "files" => {
                let diffs = ForgeStore::diff_commits(repo, pull.base_sha, pull.head_sha);
                let mut conn = Map::new();
                for sf in expand_selections(&f.selections, fragments, None) {
                    let skey = sf.alias.unwrap_or_else(|| sf.name.clone());
                    if sf.name == "nodes" {
                        let nodes: Vec<Value> = diffs
                            .iter()
                            .map(|d| {
                                let mut fo = Map::new();
                                for ff in expand_selections(&sf.selections, fragments, None) {
                                    let fkey = ff.alias.unwrap_or_else(|| ff.name.clone());
                                    match ff.name.as_str() {
                                        "path" => { fo.insert(fkey, json!(d.filename)); }
                                        "additions" => { fo.insert(fkey, json!(d.additions)); }
                                        "deletions" => { fo.insert(fkey, json!(d.deletions)); }
                                        _ => {}
                                    }
                                }
                                Value::Object(fo)
                            })
                            .collect();
                        conn.insert(skey, Value::Array(nodes));
                    }
                }
                res.insert(key, Value::Object(conn));
            }
            _ => {}
        }
    }
    Value::Object(res)
}

fn resolve_repo(
    repo: &Repo,
    sels: &[Selection],
    fragments: &FxHashMap<String, Vec<Selection>>,
    errors: &mut Vec<Value>,
) -> Value {
    let mut res = Map::new();
    for f in expand_selections(sels, fragments, Some("Repository")) {
        let key = f.alias.unwrap_or_else(|| f.name.clone());
        if !is_valid_field("Repository", &f.name) {
            errors.push(json!({ "message": format!("Field '{}' doesn't exist on type 'Repository'", f.name) }));
            continue;
        }
        match f.name.as_str() {
            "__typename" => { res.insert(key, json!("Repository")); }
            "id" => { res.insert(key, json!(repo.node_id)); }
            "databaseId" => { res.insert(key, json!(repo.id)); }
            "name" => { res.insert(key, json!(repo.name)); }
            "nameWithOwner" => { res.insert(key, json!(repo.full_name)); }
            "viewerPermission" => { res.insert(key, json!("ADMIN")); }
            "hasIssuesEnabled" => { res.insert(key, json!(repo.has_issues)); }
            "owner" => {
                let mut o = Map::new();
                for sf in expand_selections(&f.selections, fragments, None) {
                    let skey = sf.alias.unwrap_or_else(|| sf.name.clone());
                    match sf.name.as_str() {
                        "login" => { o.insert(skey, json!(repo.owner.login)); }
                        "id" => { o.insert(skey, json!(repo.owner.node_id)); }
                        _ => {}
                    }
                }
                res.insert(key, Value::Object(o));
            }
            "defaultBranchRef" => {
                let mut r = Map::new();
                for sf in expand_selections(&f.selections, fragments, None) {
                    let skey = sf.alias.unwrap_or_else(|| sf.name.clone());
                    if sf.name == "name" {
                        r.insert(skey, json!(repo.default_branch));
                    }
                }
                res.insert(key, Value::Object(r));
            }
            "issue" => {
                let num = f.args.get("number").and_then(|v| v.as_u64()).unwrap_or(0);
                let val = repo
                    .get_issue(num)
                    .map(|i| resolve_issue(i, &f.selections, fragments, errors))
                    .unwrap_or(Value::Null);
                res.insert(key, val);
            }
            "issues" => {
                let states: Vec<String> = f
                    .args
                    .get("states")
                    .and_then(|v| match v {
                        Value::Array(arr) => Some(arr.iter().filter_map(|x| x.as_str().map(|s| s.to_lowercase())).collect()),
                        Value::String(s) => Some(vec![s.to_lowercase()]),
                        _ => None,
                    })
                    .unwrap_or_default();
                let mut list: Vec<&Issue> = repo
                    .issues
                    .iter()
                    .filter(|i| states.is_empty() || states.iter().any(|s| s == &i.state))
                    .collect();
                let desc = f
                    .args
                    .get("orderBy")
                    .and_then(|v| v.get("direction"))
                    .and_then(|v| v.as_str())
                    .map(|s| s == "DESC")
                    .unwrap_or(false);
                if desc {
                    list.sort_by(|a, b| b.created_seq.cmp(&a.created_seq).then_with(|| b.number.cmp(&a.number)));
                } else {
                    list.sort_by(|a, b| a.created_seq.cmp(&b.created_seq).then_with(|| a.number.cmp(&b.number)));
                }
                let mut start_idx = 0usize;
                if let Some(after) = f.args.get("after").and_then(|v| v.as_str()) {
                    if let Ok(cnum) = after.trim_start_matches("cursor:").parse::<u64>() {
                        if let Some(pos) = list.iter().position(|i| i.number == cnum) {
                            start_idx = pos + 1;
                        }
                    }
                }
                let first = f.args.get("first").and_then(|v| v.as_u64()).unwrap_or(30) as usize;
                let end = (start_idx + first).min(list.len());
                let slice = if start_idx < list.len() { &list[start_idx..end] } else { &[] };
                let has_next = start_idx + first < list.len();
                let end_cursor = slice.last().map(|i| format!("cursor:{}", i.number));
                let mut conn = Map::new();
                for sf in expand_selections(&f.selections, fragments, None) {
                    let skey = sf.alias.unwrap_or_else(|| sf.name.clone());
                    match sf.name.as_str() {
                        "nodes" => {
                            let nodes: Vec<Value> = slice
                                .iter()
                                .map(|i| resolve_issue(i, &sf.selections, fragments, errors))
                                .collect();
                            conn.insert(skey, Value::Array(nodes));
                        }
                        "totalCount" => {
                            conn.insert(skey, json!(list.len()));
                        }
                        "pageInfo" => {
                            let mut pi = Map::new();
                            for pf in expand_selections(&sf.selections, fragments, None) {
                                let pkey = pf.alias.unwrap_or_else(|| pf.name.clone());
                                if pf.name == "hasNextPage" {
                                    pi.insert(pkey, json!(has_next));
                                } else if pf.name == "endCursor" {
                                    pi.insert(pkey, json!(end_cursor));
                                }
                            }
                            conn.insert(skey, Value::Object(pi));
                        }
                        _ => {}
                    }
                }
                res.insert(key, Value::Object(conn));
            }
            "pullRequest" => {
                let num = f.args.get("number").and_then(|v| v.as_u64()).unwrap_or(0);
                let val = repo
                    .get_pull(num)
                    .map(|p| resolve_pull(repo, p, &f.selections, fragments, errors))
                    .unwrap_or(Value::Null);
                res.insert(key, val);
            }
            "pullRequests" => {
                let first = f.args.get("first").and_then(|v| v.as_u64()).unwrap_or(30) as usize;
                let end = first.min(repo.pulls.len());
                let slice = &repo.pulls[..end];
                let mut conn = Map::new();
                for sf in expand_selections(&f.selections, fragments, None) {
                    let skey = sf.alias.unwrap_or_else(|| sf.name.clone());
                    if sf.name == "nodes" {
                        let nodes: Vec<Value> = slice
                            .iter()
                            .map(|p| resolve_pull(repo, p, &sf.selections, fragments, errors))
                            .collect();
                        conn.insert(skey, Value::Array(nodes));
                    }
                }
                res.insert(key, Value::Object(conn));
            }
            _ => {}
        }
    }
    Value::Object(res)
}

fn execute_doc(
    store: &mut ForgeStore,
    query: &str,
    vars: &Map<String, Value>,
    op_name: Option<&str>,
) -> Value {
    let mut parser = Parser {
        tokens: tokenize(query),
        pos: 0,
        vars,
    };
    let doc = parser.parse_doc();
    let op = match op_name {
        Some(n) => doc.operations.iter().find(|o| o.name.as_deref() == Some(n)),
        None => doc.operations.first(),
    };
    let op = match op {
        Some(o) => o,
        None => return json!({ "data": null, "errors": [{ "message": "No operation found" }] }),
    };

    let mut errors: Vec<Value> = Vec::new();
    let mut data = Map::new();

    for f in expand_selections(&op.selections, &doc.fragments, None) {
        let key = f.alias.unwrap_or_else(|| f.name.clone());
        if op.op_type == "query" {
            if !is_valid_field("Query", &f.name) {
                errors.push(json!({ "message": format!("Field '{}' doesn't exist on type 'Query'", f.name) }));
                continue;
            }
            match f.name.as_str() {
                "viewer" => {
                    let mut v = Map::new();
                    for sf in expand_selections(&f.selections, &doc.fragments, None) {
                        let skey = sf.alias.unwrap_or_else(|| sf.name.clone());
                        if !is_valid_field("User", &sf.name) {
                            errors.push(json!({ "message": format!("Field '{}' doesn't exist on type 'User'", sf.name) }));
                            continue;
                        }
                        match sf.name.as_str() {
                            "login" => { v.insert(skey, json!(store.viewer.login)); }
                            "id" => { v.insert(skey, json!(store.viewer.node_id)); }
                            "name" => { v.insert(skey, json!(store.viewer.name)); }
                            _ => {}
                        }
                    }
                    data.insert(key, Value::Object(v));
                }
                "repository" => {
                    let owner = f.args.get("owner").and_then(|v| v.as_str()).unwrap_or("");
                    let name = f.args.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let val = store
                        .get_repo(owner, name)
                        .map(|r| resolve_repo(r, &f.selections, &doc.fragments, &mut errors))
                        .unwrap_or(Value::Null);
                    data.insert(key, val);
                }
                "node" => {
                    let id_str = f.args.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let val = match decode_node_id(id_str) {
                        Some((prefix, id)) if prefix == "R" => store
                            .repo_by_id
                            .get(&id)
                            .and_then(|&idx| store.repos.get(idx))
                            .map(|r| resolve_repo(r, &f.selections, &doc.fragments, &mut errors))
                            .unwrap_or(Value::Null),
                        _ => Value::Null,
                    };
                    data.insert(key, val);
                }
                "__type" => {
                    let tname = f.args.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let fields = introspection_fields(tname);
                    let enums = introspection_enums(tname);
                    if fields.is_none() && enums.is_none() {
                        data.insert(key, Value::Null);
                    } else {
                        let mut out = Map::new();
                        for sf in expand_selections(&f.selections, &doc.fragments, None) {
                            let skey = sf.alias.unwrap_or_else(|| sf.name.clone());
                            match sf.name.as_str() {
                                "name" => { out.insert(skey, json!(tname)); }
                                "fields" => {
                                    let arr: Vec<Value> = fields.unwrap_or(&[]).iter().map(|fnm| json!({ "name": fnm })).collect();
                                    out.insert(skey, Value::Array(arr));
                                }
                                "enumValues" => {
                                    let arr: Vec<Value> = enums.unwrap_or(&[]).iter().map(|ev| json!({ "name": ev })).collect();
                                    out.insert(skey, Value::Array(arr));
                                }
                                _ => {}
                            }
                        }
                        data.insert(key, Value::Object(out));
                    }
                }
                _ => {}
            }
        } else {
            if !is_valid_field("Mutation", &f.name) {
                errors.push(json!({ "message": format!("Field '{}' doesn't exist on type 'Mutation'", f.name) }));
                continue;
            }
            let input = f.args.get("input").and_then(|v| v.as_object());
            match f.name.as_str() {
                "createIssue" => {
                    let repo_id_str = input.and_then(|m| m.get("repositoryId")).and_then(|v| v.as_str()).unwrap_or("");
                    let repo_idx = decode_node_id(repo_id_str)
                        .filter(|(p, _)| p == "R")
                        .and_then(|(_, id)| store.repo_by_id.get(&id).copied());
                    let repo_idx = match repo_idx {
                        Some(i) => i,
                        None => {
                            errors.push(json!({ "message": "Repository not found" }));
                            data.insert(key, Value::Null);
                            continue;
                        }
                    };
                    let title = input.and_then(|m| m.get("title")).and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let body = input.and_then(|m| m.get("body")).and_then(|v| v.as_str()).unwrap_or("").to_string();
                    match store.create_issue(repo_idx, title, body, &[]) {
                        Ok(num) => {
                            let issue = store.repos[repo_idx].get_issue(num).unwrap();
                            let mut payload = Map::new();
                            for sf in expand_selections(&f.selections, &doc.fragments, None) {
                                if sf.name == "issue" {
                                    let skey = sf.alias.unwrap_or_else(|| sf.name.clone());
                                    payload.insert(skey, resolve_issue(issue, &sf.selections, &doc.fragments, &mut errors));
                                }
                            }
                            data.insert(key, Value::Object(payload));
                        }
                        Err(e) => {
                            errors.push(json!({ "message": e }));
                            data.insert(key, Value::Null);
                        }
                    }
                }
                "closeIssue" => {
                    let issue_id_str = input.and_then(|m| m.get("issueId")).and_then(|v| v.as_str()).unwrap_or("");
                    let found = decode_node_id(issue_id_str)
                        .filter(|(p, _)| p == "I")
                        .and_then(|(_, id)| store.issue_by_id.get(&id).copied());
                    let (repo_idx, num) = match found {
                        Some(v) => v,
                        None => {
                            errors.push(json!({ "message": "Issue not found" }));
                            data.insert(key, Value::Null);
                            continue;
                        }
                    };
                    let now = store.next_timestamp();
                    if let Some(issue) = store.repos[repo_idx].get_issue_mut(num) {
                        issue.state = "closed".to_string();
                        issue.closed_at = Some(now.clone());
                        issue.updated_at = now;
                        issue.cached_http_get = None;
                    }
                    let issue = store.repos[repo_idx].get_issue(num).unwrap();
                    let mut payload = Map::new();
                    for sf in expand_selections(&f.selections, &doc.fragments, None) {
                        if sf.name == "issue" {
                            let skey = sf.alias.unwrap_or_else(|| sf.name.clone());
                            payload.insert(skey, resolve_issue(issue, &sf.selections, &doc.fragments, &mut errors));
                        }
                    }
                    data.insert(key, Value::Object(payload));
                }
                "createPullRequest" => {
                    let repo_id_str = input.and_then(|m| m.get("repositoryId")).and_then(|v| v.as_str()).unwrap_or("");
                    let repo_idx = decode_node_id(repo_id_str)
                        .filter(|(p, _)| p == "R")
                        .and_then(|(_, id)| store.repo_by_id.get(&id).copied());
                    let repo_idx = match repo_idx {
                        Some(i) => i,
                        None => {
                            errors.push(json!({ "message": "Repository not found" }));
                            data.insert(key, Value::Null);
                            continue;
                        }
                    };
                    let title = input.and_then(|m| m.get("title")).and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let body = input.and_then(|m| m.get("body")).and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let head = input.and_then(|m| m.get("headRefName")).and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let base = input
                        .and_then(|m| m.get("baseRefName"))
                        .and_then(|v| v.as_str())
                        .map(String::from)
                        .unwrap_or_else(|| store.repos[repo_idx].default_branch.clone());
                    match store.create_pull(repo_idx, title, body, head, base) {
                        Ok(num) => {
                            let repo = &store.repos[repo_idx];
                            let pull = repo.get_pull(num).unwrap();
                            let mut payload = Map::new();
                            for sf in expand_selections(&f.selections, &doc.fragments, None) {
                                if sf.name == "pullRequest" {
                                    let skey = sf.alias.unwrap_or_else(|| sf.name.clone());
                                    payload.insert(skey, resolve_pull(repo, pull, &sf.selections, &doc.fragments, &mut errors));
                                }
                            }
                            data.insert(key, Value::Object(payload));
                        }
                        Err(e) => {
                            errors.push(json!({ "message": e }));
                            data.insert(key, Value::Null);
                        }
                    }
                }
                "mergePullRequest" => {
                    let pr_id_str = input.and_then(|m| m.get("pullRequestId")).and_then(|v| v.as_str()).unwrap_or("");
                    let found = decode_node_id(pr_id_str)
                        .filter(|(p, _)| p == "PR")
                        .and_then(|(_, id)| store.pull_by_id.get(&id).copied());
                    let (repo_idx, num) = match found {
                        Some(v) => v,
                        None => {
                            errors.push(json!({ "message": "Pull request not found" }));
                            data.insert(key, Value::Null);
                            continue;
                        }
                    };
                    match store.squash_merge(repo_idx, num, None) {
                        Ok(_) => {
                            let repo = &store.repos[repo_idx];
                            let pull = repo.get_pull(num).unwrap();
                            let mut payload = Map::new();
                            for sf in expand_selections(&f.selections, &doc.fragments, None) {
                                if sf.name == "pullRequest" {
                                    let skey = sf.alias.unwrap_or_else(|| sf.name.clone());
                                    payload.insert(skey, resolve_pull(repo, pull, &sf.selections, &doc.fragments, &mut errors));
                                }
                            }
                            data.insert(key, Value::Object(payload));
                        }
                        Err(_) => {
                            errors.push(json!({ "message": "Merge conflict" }));
                            data.insert(key, Value::Null);
                        }
                    }
                }
                "addComment" => {
                    let subj = input.and_then(|m| m.get("subjectId")).and_then(|v| v.as_str()).unwrap_or("");
                    let found = decode_node_id(subj)
                        .filter(|(p, _)| p == "I")
                        .and_then(|(_, id)| store.issue_by_id.get(&id).copied());
                    let (repo_idx, num) = match found {
                        Some(v) => v,
                        None => {
                            errors.push(json!({ "message": "Subject not found" }));
                            data.insert(key, Value::Null);
                            continue;
                        }
                    };
                    let body = input.and_then(|m| m.get("body")).and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let comment = store.add_issue_comment(repo_idx, num, body).unwrap();
                    let mut payload = Map::new();
                    for sf in expand_selections(&f.selections, &doc.fragments, None) {
                        if sf.name == "commentEdge" {
                            let mut edge = Map::new();
                            for ef in expand_selections(&sf.selections, &doc.fragments, None) {
                                if ef.name == "node" {
                                    let mut node = Map::new();
                                    for nf in expand_selections(&ef.selections, &doc.fragments, None) {
                                        let nkey = nf.alias.unwrap_or_else(|| nf.name.clone());
                                        if nf.name == "id" {
                                            node.insert(nkey, json!(comment.node_id));
                                        } else if nf.name == "body" {
                                            node.insert(nkey, json!(comment.body));
                                        }
                                    }
                                    let ekey = ef.alias.unwrap_or_else(|| ef.name.clone());
                                    edge.insert(ekey, Value::Object(node));
                                }
                            }
                            let skey = sf.alias.unwrap_or_else(|| sf.name.clone());
                            payload.insert(skey, Value::Object(edge));
                        }
                    }
                    data.insert(key, Value::Object(payload));
                }
                _ => {}
            }
        }
    }

    if errors.is_empty() {
        json!({ "data": Value::Object(data) })
    } else {
        json!({ "data": Value::Object(data), "errors": errors })
    }
}

pub fn handle_graphql_request(
    store: &mut ForgeStore,
    auth: Option<&[u8]>,
    raw_body: &[u8],
    resp_out: &mut Vec<u8>,
) {
    if !store.is_authorized(auth) {
        write_http_json_raw(resp_out, "401 Unauthorized", "", b"{\"message\":\"Requires authentication\"}\n");
        return;
    }
    let parsed: Value = match serde_json::from_slice(raw_body) {
        Ok(v) => v,
        Err(_) => {
            write_http_json_raw(resp_out, "400 Bad Request", "", b"{\"message\":\"Problems parsing JSON\"}\n");
            return;
        }
    };
    let empty_vars = Map::new();
    if let Some(arr) = parsed.as_array() {
        let mut results = Vec::with_capacity(arr.len());
        for item in arr {
            let q = item.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let vars = item.get("variables").and_then(|v| v.as_object()).unwrap_or(&empty_vars);
            let op = item.get("operationName").and_then(|v| v.as_str());
            results.push(execute_doc(store, q, vars, op));
        }
        write_http_json(resp_out, "200 OK", "", &Value::Array(results));
        return;
    }
    if !parsed.is_object() {
        write_http_json_raw(resp_out, "400 Bad Request", "", b"{\"message\":\"Body must be JSON object or array\"}\n");
        return;
    }
    let q = parsed.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let vars = parsed.get("variables").and_then(|v| v.as_object()).unwrap_or(&empty_vars);
    let op = parsed.get("operationName").and_then(|v| v.as_str());
    let res = execute_doc(store, q, vars, op);
    write_http_json(resp_out, "200 OK", "", &res);
}
