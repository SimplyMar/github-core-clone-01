import type { IncomingMessage, ServerResponse } from 'node:http';
import {
  ForgeStore,
  parseCommit,
  parseTree,
  type Issue,
  type PullRequest,
  type Repo,
} from './store.ts';

const JSON_TYPE = 'application/json; charset=utf-8';

export function sendJson(
  res: ServerResponse,
  status: number,
  body: unknown,
  reqId: string,
  extraHeaders: Readonly<Record<string, string>> = {},
): void {
  if (status === 204) {
    res.writeHead(204, { 'content-type': JSON_TYPE, 'x-github-request-id': reqId, ...extraHeaders });
    res.end();
    return;
  }
  const text = `${JSON.stringify(body)}\n`;
  res.writeHead(status, {
    'content-type': JSON_TYPE,
    'content-length': Buffer.byteLength(text),
    'x-github-request-id': reqId,
    ...extraHeaders,
  });
  res.end(text);
}

export function formatRepo(repo: Repo) {
  return {
    id: repo.id,
    node_id: repo.node_id,
    name: repo.name,
    full_name: repo.full_name,
    private: repo.private,
    owner: repo.owner,
    description: repo.description,
    default_branch: repo.default_branch,
    has_issues: repo.has_issues,
    created_at: repo.created_at,
    updated_at: repo.updated_at,
  };
}

export function formatIssue(issue: Issue) {
  return {
    id: issue.id,
    node_id: issue.node_id,
    number: issue.number,
    title: issue.title,
    body: issue.body,
    state: issue.state,
    labels: issue.labels,
    user: issue.user,
    comments: issue.comments.length,
    created_at: issue.created_at,
    updated_at: issue.updated_at,
    closed_at: issue.closed_at,
  };
}

export function formatPull(repo: Repo, pull: PullRequest) {
  return {
    id: pull.id,
    node_id: pull.node_id,
    number: pull.number,
    state: pull.state,
    title: pull.title,
    body: pull.body,
    head: { ref: pull.headRef, sha: pull.headSha, label: `${repo.owner.login}:${pull.headRef}` },
    base: { ref: pull.baseRef, sha: pull.baseSha, label: `${repo.owner.login}:${pull.baseRef}` },
    merged: pull.merged,
    merged_at: pull.merged_at,
    merge_commit_sha: pull.merge_commit_sha,
    mergeable: true,
    user: pull.user,
    created_at: pull.created_at,
    updated_at: pull.updated_at,
    closed_at: pull.closed_at,
  };
}

export function handleRestRequest(
  req: IncomingMessage,
  res: ServerResponse,
  store: ForgeStore,
  path: string,
  searchParams: URLSearchParams,
  rawBody: Buffer,
  reqId: string,
): void {
  const method = req.method ?? 'GET';

  if (method === 'GET' && (path === '/api/v3' || path === '/api/v3/')) {
    sendJson(
      res,
      200,
      { current_user_url: '/api/v3/user', rate_limit_url: '/api/v3/rate_limit' },
      reqId,
      { 'x-oauth-scopes': 'repo, read:org' },
    );
    return;
  }

  if (method === 'GET' && path === '/api/v3/meta') {
    sendJson(res, 200, { installed_version: '3.15.0', verifiable_password_authentication: false }, reqId);
    return;
  }

  if (!store.isAuthorized(req.headers.authorization)) {
    sendJson(res, 401, { message: 'Requires authentication' }, reqId);
    return;
  }

  let body: Record<string, unknown> = {};
  if (rawBody.length > 0 && method !== 'GET' && method !== 'HEAD') {
    try {
      const parsed = JSON.parse(rawBody.toString('utf8'));
      if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
        sendJson(res, 400, { message: 'Body should be a JSON object' }, reqId);
        return;
      }
      body = parsed as Record<string, unknown>;
    } catch {
      sendJson(res, 400, { message: 'Problems parsing JSON' }, reqId);
      return;
    }
  }

  if (method === 'GET' && path === '/api/v3/user') {
    sendJson(res, 200, store.viewer, reqId);
    return;
  }

  if (path === '/api/v3/user/repos') {
    if (method === 'GET') {
      const all = [...store.reposByName.values()].map(formatRepo);
      const perPage = Math.max(1, Math.min(100, parseInt(searchParams.get('per_page') ?? '30', 10) || 30));
      const page = Math.max(1, parseInt(searchParams.get('page') ?? '1', 10) || 1);
      const start = (page - 1) * perPage;
      const slice = all.slice(start, start + perPage);
      const headers: Record<string, string> = {};
      if (start + perPage < all.length) {
        const host = req.headers.host || '127.0.0.1';
        headers.link = `<http://${host}/api/v3/user/repos?per_page=${perPage}&page=${page + 1}>; rel="next"`;
      }
      sendJson(res, 200, slice, reqId, headers);
      return;
    }
    if (method === 'POST') {
      const name = typeof body.name === 'string' ? body.name.trim() : '';
      if (!name) {
        sendJson(res, 422, { message: 'Repository name is required' }, reqId);
        return;
      }
      if (store.reposByName.has(name)) {
        sendJson(res, 422, { message: 'Repository already exists' }, reqId);
        return;
      }
      const repo = store.createRepo({
        name,
        description: typeof body.description === 'string' ? body.description : '',
        private: Boolean(body.private ?? false),
        default_branch: typeof body.default_branch === 'string' && body.default_branch ? body.default_branch : 'main',
        has_issues: body.has_issues === undefined ? true : Boolean(body.has_issues),
      });
      sendJson(res, 201, formatRepo(repo), reqId);
      return;
    }
  }

  const repoMatch = path.match(/^\/api\/v3\/repos\/([^/]+)\/([^/]+)(?:\/(.*))?$/);
  if (!repoMatch) {
    sendJson(res, 404, { message: 'Not Found' }, reqId);
    return;
  }
  const owner = decodeURIComponent(repoMatch[1]);
  const repoName = decodeURIComponent(repoMatch[2]);
  const subPath = repoMatch[3] ?? '';
  const repo = store.getRepo(owner, repoName);
  if (!repo) {
    sendJson(res, 404, { message: 'Not Found' }, reqId);
    return;
  }

  if (subPath === '') {
    if (method === 'GET') {
      sendJson(res, 200, formatRepo(repo), reqId);
      return;
    }
    if (method === 'PATCH') {
      if (typeof body.description === 'string') repo.description = body.description;
      if (typeof body.default_branch === 'string' && body.default_branch) repo.default_branch = body.default_branch;
      if (typeof body.private === 'boolean') repo.private = body.private;
      if (typeof body.has_issues === 'boolean') repo.has_issues = body.has_issues;
      repo.updated_at = store.nextTimestamp();
      sendJson(res, 200, formatRepo(repo), reqId);
      return;
    }
    if (method === 'DELETE') {
      store.deleteRepo(owner, repoName);
      sendJson(res, 204, null, reqId);
      return;
    }
  }

  if (subPath === 'git/refs' && method === 'POST') {
    const ref = typeof body.ref === 'string' ? body.ref : '';
    const sha = typeof body.sha === 'string' ? body.sha : '';
    if (!ref || !sha || !repo.objects.has(sha)) {
      sendJson(res, 422, { message: 'Invalid ref or sha' }, reqId);
      return;
    }
    const fullRef = ref.startsWith('refs/') ? ref : `refs/heads/${ref}`;
    store.setRef(repo, fullRef, sha);
    sendJson(res, 201, { ref: fullRef, object: { type: 'commit', sha } }, reqId);
    return;
  }

  if (subPath === 'branches' && method === 'GET') {
    const branches = [];
    for (const [ref, sha] of repo.refs) {
      if (ref.startsWith('refs/heads/')) {
        branches.push({ name: ref.slice(11), commit: { sha }, protected: false });
      }
    }
    sendJson(res, 200, branches, reqId);
    return;
  }

  if (subPath === 'commits' && method === 'GET') {
    const startRef = searchParams.get('sha') ?? repo.default_branch;
    const tip = store.resolveRef(repo, startRef);
    if (!tip) {
      sendJson(res, 404, { message: 'Not Found' }, reqId);
      return;
    }
    const perPage = Math.max(1, Math.min(100, parseInt(searchParams.get('per_page') ?? '30', 10) || 30));
    const list = [];
    let cur: string | undefined = tip;
    const seen = new Set<string>();
    while (cur && !seen.has(cur) && list.length < perPage) {
      seen.add(cur);
      const obj = repo.objects.get(cur);
      if (!obj || obj.type !== 'commit') break;
      const parsed = parseCommit(obj.data);
      list.push({
        sha: cur,
        commit: {
          message: parsed.message.replace(/\n$/, ''),
          author: { name: store.viewer.name, email: store.viewer.email, date: store.nowIso },
          committer: { name: store.viewer.name, email: store.viewer.email, date: store.nowIso },
        },
        parents: parsed.parents.map((p) => ({ sha: p })),
      });
      cur = parsed.parents[0];
    }
    sendJson(res, 200, list, reqId);
    return;
  }

  if (subPath === 'contents' || subPath.startsWith('contents/')) {
    const filePath = subPath === 'contents' ? '' : decodeURIComponent(subPath.slice(9)).replace(/^\/+|\/+$/g, '');
    if (method === 'GET') {
      const ref = searchParams.get('ref') ?? repo.default_branch;
      const tipSha = store.resolveRef(repo, ref);
      const treeOid = store.treeOfCommit(repo, tipSha);
      if (!treeOid) {
        sendJson(res, 404, { message: 'Not Found' }, reqId);
        return;
      }
      if (filePath === '') {
        const treeObj = repo.objects.get(treeOid);
        if (!treeObj) {
          sendJson(res, 404, { message: 'Not Found' }, reqId);
          return;
        }
        const items = parseTree(treeObj.data).map((e) => ({
          name: e.name,
          path: e.name,
          sha: e.oid,
          size: e.mode === '40000' ? 0 : (repo.objects.get(e.oid)?.data.length ?? 0),
          type: e.mode === '40000' || e.mode === '040000' ? 'dir' : 'file',
        }));
        sendJson(res, 200, items, reqId);
        return;
      }
      const parts = filePath.split('/');
      let curTree = treeOid;
      for (let i = 0; i < parts.length; i++) {
        const obj = repo.objects.get(curTree);
        if (!obj || obj.type !== 'tree') {
          sendJson(res, 404, { message: 'Not Found' }, reqId);
          return;
        }
        const entry = parseTree(obj.data).find((e) => e.name === parts[i]);
        if (!entry) {
          sendJson(res, 404, { message: 'Not Found' }, reqId);
          return;
        }
        if (i === parts.length - 1) {
          if (entry.mode === '40000' || entry.mode === '040000') {
            const subTree = repo.objects.get(entry.oid);
            const items = subTree
              ? parseTree(subTree.data).map((e) => ({
                  name: e.name,
                  path: `${filePath}/${e.name}`,
                  sha: e.oid,
                  size: e.mode === '40000' ? 0 : (repo.objects.get(e.oid)?.data.length ?? 0),
                  type: e.mode === '40000' || e.mode === '040000' ? 'dir' : 'file',
                }))
              : [];
            sendJson(res, 200, items, reqId);
            return;
          }
          const blob = repo.objects.get(entry.oid);
          if (!blob) {
            sendJson(res, 404, { message: 'Not Found' }, reqId);
            return;
          }
          sendJson(
            res,
            200,
            {
              name: entry.name,
              path: filePath,
              sha: entry.oid,
              size: blob.data.length,
              type: 'file',
              encoding: 'base64',
              content: blob.data.toString('base64'),
            },
            reqId,
          );
          return;
        }
        if (entry.mode !== '40000' && entry.mode !== '040000') {
          sendJson(res, 404, { message: 'Not Found' }, reqId);
          return;
        }
        curTree = entry.oid;
      }
    }

    if (method === 'PUT') {
      const branch = typeof body.branch === 'string' && body.branch ? body.branch : repo.default_branch;
      const contentB64 = typeof body.content === 'string' ? body.content : '';
      const message = typeof body.message === 'string' ? body.message : `Update ${filePath}`;
      const providedSha = typeof body.sha === 'string' ? body.sha : undefined;

      const tipSha = store.resolveRef(repo, branch);
      const flat = store.flattenTree(repo, store.treeOfCommit(repo, tipSha));
      const existing = flat.get(filePath);
      if (existing) {
        if (!providedSha || providedSha !== existing.oid) {
          sendJson(res, 409, { message: `${filePath} does not match ${providedSha ?? 'none'}` }, reqId);
          return;
        }
      }
      const rawBuf = Buffer.from(contentB64, 'base64');
      const { commitSha, blobSha } = store.commitFileChange(repo, {
        branch,
        path: filePath,
        content: rawBuf,
        message,
      });
      const fileName = filePath.split('/').pop() ?? filePath;
      sendJson(
        res,
        existing ? 200 : 201,
        {
          content: {
            name: fileName,
            path: filePath,
            sha: blobSha,
            size: rawBuf.length,
            type: 'file',
            encoding: 'base64',
          },
          commit: { sha: commitSha, message },
        },
        reqId,
      );
      return;
    }

    if (method === 'DELETE') {
      const branch = typeof body.branch === 'string' && body.branch ? body.branch : repo.default_branch;
      const message = typeof body.message === 'string' ? body.message : `Delete ${filePath}`;
      const providedSha = typeof body.sha === 'string' ? body.sha : undefined;

      const tipSha = store.resolveRef(repo, branch);
      const flat = store.flattenTree(repo, store.treeOfCommit(repo, tipSha));
      const existing = flat.get(filePath);
      if (!existing) {
        sendJson(res, 404, { message: 'Not Found' }, reqId);
        return;
      }
      if (providedSha && providedSha !== existing.oid) {
        sendJson(res, 409, { message: `${filePath} does not match ${providedSha}` }, reqId);
        return;
      }
      const { commitSha } = store.commitFileChange(repo, {
        branch,
        path: filePath,
        content: null,
        message,
      });
      sendJson(res, 200, { content: null, commit: { sha: commitSha, message } }, reqId);
      return;
    }
  }

  if (subPath === 'labels') {
    if (method === 'GET') {
      sendJson(res, 200, [...repo.labels.values()], reqId);
      return;
    }
    if (method === 'POST') {
      const name = typeof body.name === 'string' ? body.name.trim() : '';
      if (!name) {
        sendJson(res, 422, { message: 'Label name is required' }, reqId);
        return;
      }
      const label = store.createLabel(
        repo,
        name,
        typeof body.color === 'string' ? body.color : 'ededed',
        typeof body.description === 'string' ? body.description : '',
      );
      sendJson(res, 201, label, reqId);
      return;
    }
  }

  if (subPath === 'issues') {
    if (method === 'GET') {
      const stateFilter = searchParams.get('state') ?? 'open';
      const direction = searchParams.get('direction') ?? 'desc';
      const perPage = Math.max(1, Math.min(100, parseInt(searchParams.get('per_page') ?? '30', 10) || 30));
      const page = Math.max(1, parseInt(searchParams.get('page') ?? '1', 10) || 1);

      let items = [...repo.issues.values()];
      if (stateFilter === 'open' || stateFilter === 'closed') {
        items = items.filter((i) => i.state === stateFilter);
      }
      items.sort((a, b) => (direction === 'asc' ? a.createdSeq - b.createdSeq : b.createdSeq - a.createdSeq));
      const start = (page - 1) * perPage;
      sendJson(res, 200, items.slice(start, start + perPage).map(formatIssue), reqId);
      return;
    }
    if (method === 'POST') {
      const title = typeof body.title === 'string' ? body.title : '';
      if (!title) {
        sendJson(res, 422, { message: 'Title is required' }, reqId);
        return;
      }
      const labels = Array.isArray(body.labels) ? body.labels.filter((l): l is string => typeof l === 'string') : [];
      const created = store.createIssue(repo, {
        title,
        body: typeof body.body === 'string' ? body.body : '',
        labels,
      });
      if ('badLabel' in created) {
        sendJson(res, 422, { message: `Label ${created.badLabel} does not exist` }, reqId);
        return;
      }
      sendJson(res, 201, formatIssue(created), reqId);
      return;
    }
  }

  const issueNumMatch = subPath.match(/^issues\/(\d+)(?:\/(comments))?$/);
  if (issueNumMatch) {
    const num = parseInt(issueNumMatch[1], 10);
    const issue = repo.issues.get(num);
    if (!issue) {
      sendJson(res, 404, { message: 'Not Found' }, reqId);
      return;
    }
    if (issueNumMatch[2] === 'comments') {
      if (method === 'GET') {
        sendJson(res, 200, issue.comments, reqId);
        return;
      }
      if (method === 'POST') {
        const commentBody = typeof body.body === 'string' ? body.body : '';
        const comment = store.addIssueComment(issue, commentBody);
        sendJson(res, 201, comment, reqId);
        return;
      }
    } else {
      if (method === 'GET') {
        sendJson(res, 200, formatIssue(issue), reqId);
        return;
      }
      if (method === 'PATCH') {
        if (typeof body.title === 'string') issue.title = body.title;
        if (typeof body.body === 'string') issue.body = body.body;
        if (body.state === 'open' || body.state === 'closed') {
          issue.state = body.state;
          issue.closed_at = body.state === 'closed' ? store.nextTimestamp() : null;
        }
        issue.updated_at = store.nextTimestamp();
        sendJson(res, 200, formatIssue(issue), reqId);
        return;
      }
    }
  }

  if (subPath === 'pulls') {
    if (method === 'GET') {
      const stateFilter = searchParams.get('state') ?? 'open';
      let items = [...repo.pulls.values()];
      if (stateFilter === 'open' || stateFilter === 'closed') {
        items = items.filter((p) => p.state === stateFilter);
      }
      sendJson(res, 200, items.map((p) => formatPull(repo, p)), reqId);
      return;
    }
    if (method === 'POST') {
      const title = typeof body.title === 'string' ? body.title : '';
      const head = typeof body.head === 'string' ? body.head : '';
      const base = typeof body.base === 'string' ? body.base : repo.default_branch;
      if (!title || !head) {
        sendJson(res, 422, { message: 'title and head are required' }, reqId);
        return;
      }
      const created = store.createPull(repo, {
        title,
        body: typeof body.body === 'string' ? body.body : '',
        head,
        base,
      });
      if ('error' in created) {
        sendJson(res, 422, { message: created.error }, reqId);
        return;
      }
      sendJson(res, 201, formatPull(repo, created), reqId);
      return;
    }
  }

  const pullNumMatch = subPath.match(/^pulls\/(\d+)(?:\/(files|merge))?$/);
  if (pullNumMatch) {
    const num = parseInt(pullNumMatch[1], 10);
    const pull = repo.pulls.get(num);
    if (!pull) {
      sendJson(res, 404, { message: 'Not Found' }, reqId);
      return;
    }
    const action = pullNumMatch[2];
    if (!action && method === 'GET') {
      sendJson(res, 200, formatPull(repo, pull), reqId);
      return;
    }
    if (action === 'files' && method === 'GET') {
      sendJson(res, 200, store.diffCommits(repo, pull.baseSha, pull.headSha), reqId);
      return;
    }
    if (action === 'merge' && method === 'PUT') {
      if (pull.merged) {
        sendJson(res, 405, { message: 'Pull Request is already merged' }, reqId);
        return;
      }
      const resMerge = store.squashMerge(
        repo,
        pull,
        typeof body.commit_message === 'string' ? body.commit_message : undefined,
      );
      if ('conflict' in resMerge) {
        sendJson(res, 405, { message: 'Pull Request is not mergeable' }, reqId);
        return;
      }
      sendJson(res, 200, { sha: resMerge.sha, merged: true, message: 'Pull Request successfully merged' }, reqId);
      return;
    }
  }

  sendJson(res, 404, { message: 'Not Found' }, reqId);
}
