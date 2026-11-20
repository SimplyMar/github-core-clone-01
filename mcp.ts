import type { IncomingMessage, ServerResponse } from 'node:http';
import { formatIssue, formatPull, formatRepo, sendJson } from './rest.ts';
import { ForgeStore } from './store.ts';

const TOOLS = [
  {
    name: 'create_repo',
    description: 'Create a new repository for the authenticated user',
    inputSchema: {
      type: 'object',
      properties: {
        name: { type: 'string' },
        description: { type: 'string' },
        private: { type: 'boolean' },
      },
      required: ['name'],
    },
  },
  {
    name: 'get_repo',
    description: 'Get details of a repository',
    inputSchema: {
      type: 'object',
      properties: {
        owner: { type: 'string' },
        repo: { type: 'string' },
      },
      required: ['owner', 'repo'],
    },
  },
  {
    name: 'create_issue',
    description: 'Create an issue in a repository',
    inputSchema: {
      type: 'object',
      properties: {
        owner: { type: 'string' },
        repo: { type: 'string' },
        title: { type: 'string' },
        body: { type: 'string' },
      },
      required: ['owner', 'repo', 'title'],
    },
  },
  {
    name: 'list_issues',
    description: 'List issues in a repository',
    inputSchema: {
      type: 'object',
      properties: {
        owner: { type: 'string' },
        repo: { type: 'string' },
        state: { type: 'string' },
      },
      required: ['owner', 'repo'],
    },
  },
  {
    name: 'create_pull_request',
    description: 'Create a pull request in a repository',
    inputSchema: {
      type: 'object',
      properties: {
        owner: { type: 'string' },
        repo: { type: 'string' },
        title: { type: 'string' },
        head: { type: 'string' },
        base: { type: 'string' },
        body: { type: 'string' },
      },
      required: ['owner', 'repo', 'title', 'head', 'base'],
    },
  },
  {
    name: 'merge_pull_request',
    description: 'Squash-merge a pull request',
    inputSchema: {
      type: 'object',
      properties: {
        owner: { type: 'string' },
        repo: { type: 'string' },
        pull_number: { type: 'number' },
      },
      required: ['owner', 'repo', 'pull_number'],
    },
  },
];

function toolResult(data: unknown, isError = false) {
  return {
    content: [{ type: 'text', text: JSON.stringify(data) }],
    isError,
  };
}

function resolveRepoArgs(store: ForgeStore, args: Record<string, unknown>) {
  let owner = typeof args.owner === 'string' ? args.owner : '';
  let repoName = typeof args.repo === 'string' ? args.repo : '';
  if (!owner && typeof args.full_name === 'string' && args.full_name.includes('/')) {
    const [o, r] = args.full_name.split('/');
    owner = o;
    repoName = r;
  }
  return store.getRepo(owner, repoName);
}

export function handleMcpRequest(
  req: IncomingMessage,
  res: ServerResponse,
  store: ForgeStore,
  rawBody: Buffer,
  reqId: string,
): void {
  if (!store.isAuthorized(req.headers.authorization)) {
    sendJson(res, 401, { message: 'Requires authentication' }, reqId);
    return;
  }

  let msg: Record<string, unknown>;
  try {
    const parsed = JSON.parse(rawBody.toString('utf8'));
    if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
      sendJson(res, 400, { jsonrpc: '2.0', id: null, error: { code: -32600, message: 'Invalid Request' } }, reqId);
      return;
    }
    msg = parsed as Record<string, unknown>;
  } catch {
    sendJson(res, 400, { jsonrpc: '2.0', id: null, error: { code: -32700, message: 'Parse error' } }, reqId);
    return;
  }

  const id = msg.id ?? null;
  const method = typeof msg.method === 'string' ? msg.method : '';
  const params = (typeof msg.params === 'object' && msg.params !== null ? msg.params : {}) as Record<string, unknown>;

  if (method === 'notifications/initialized' || id === undefined) {
    sendJson(res, 204, null, reqId);
    return;
  }

  const ok = (result: unknown) => {
    sendJson(res, 200, { jsonrpc: '2.0', id, result }, reqId, { 'mcp-session-id': 'session-1' });
  };
  const err = (code: number, message: string) => {
    sendJson(res, 200, { jsonrpc: '2.0', id, error: { code, message } }, reqId);
  };

  if (method === 'server/discover') {
    ok({
      protocolVersion: '2026-07-28',
      supportedVersions: ['2026-07-28', '2025-11-25'],
      serverInfo: { name: 'github-core-clone', version: '1.0.0' },
      capabilities: { tools: { listChanged: false } },
    });
    return;
  }

  if (method === 'initialize') {
    ok({
      protocolVersion: typeof params.protocolVersion === 'string' ? params.protocolVersion : '2025-11-25',
      serverInfo: { name: 'github-core-clone', version: '1.0.0' },
      capabilities: { tools: { listChanged: false } },
    });
    return;
  }

  if (method === 'tools/list') {
    ok({ tools: TOOLS });
    return;
  }

  if (method === 'tools/call') {
    const toolName = typeof params.name === 'string' ? params.name : '';
    const args = (typeof params.arguments === 'object' && params.arguments !== null ? params.arguments : {}) as Record<string, unknown>;

    if (toolName === 'create_repo') {
      const name = typeof args.name === 'string' ? args.name.trim() : '';
      if (!name) {
        ok(toolResult({ error: 'name is required' }, true));
        return;
      }
      if (store.reposByName.has(name)) {
        ok(toolResult({ error: 'Repository already exists' }, true));
        return;
      }
      const repo = store.createRepo({
        name,
        description: typeof args.description === 'string' ? args.description : '',
        private: Boolean(args.private ?? false),
      });
      ok(toolResult(formatRepo(repo)));
      return;
    }

    if (toolName === 'get_repo') {
      const owner = typeof args.owner === 'string' ? args.owner : '';
      const repoName = typeof args.repo === 'string' ? args.repo : '';
      if (!owner || !repoName) {
        ok(toolResult({ error: 'owner and repo are required' }, true));
        return;
      }
      const repo = resolveRepoArgs(store, args);
      if (!repo) {
        ok(toolResult({ error: 'Repository not found' }, true));
        return;
      }
      ok(toolResult(formatRepo(repo)));
      return;
    }

    if (toolName === 'create_issue') {
      const owner = typeof args.owner === 'string' ? args.owner : '';
      const repoName = typeof args.repo === 'string' ? args.repo : '';
      const title = typeof args.title === 'string' ? args.title : '';
      if (!owner || !repoName || !title) {
        ok(toolResult({ error: 'owner, repo, and title are required' }, true));
        return;
      }
      const repo = resolveRepoArgs(store, args);
      if (!repo) {
        ok(toolResult({ error: 'Repository not found' }, true));
        return;
      }
      const created = store.createIssue(repo, {
        title,
        body: typeof args.body === 'string' ? args.body : '',
      });
      if ('badLabel' in created) {
        ok(toolResult({ error: 'Invalid label' }, true));
        return;
      }
      ok(toolResult(formatIssue(created)));
      return;
    }

    if (toolName === 'list_issues') {
      const owner = typeof args.owner === 'string' ? args.owner : '';
      const repoName = typeof args.repo === 'string' ? args.repo : '';
      if (!owner || !repoName) {
        ok(toolResult({ error: 'owner and repo are required' }, true));
        return;
      }
      const repo = resolveRepoArgs(store, args);
      if (!repo) {
        ok(toolResult({ error: 'Repository not found' }, true));
        return;
      }
      const stateFilter = typeof args.state === 'string' ? args.state : 'open';
      let list = [...repo.issues.values()];
      if (stateFilter === 'open' || stateFilter === 'closed') {
        list = list.filter((i) => i.state === stateFilter);
      }
      ok(toolResult(list.map(formatIssue)));
      return;
    }

    if (toolName === 'create_pull_request') {
      const owner = typeof args.owner === 'string' ? args.owner : '';
      const repoName = typeof args.repo === 'string' ? args.repo : '';
      const title = typeof args.title === 'string' ? args.title : '';
      const head = typeof args.head === 'string' ? args.head : '';
      const base = typeof args.base === 'string' ? args.base : '';
      if (!owner || !repoName || !title || !head || !base) {
        ok(toolResult({ error: 'owner, repo, title, head, and base are required' }, true));
        return;
      }
      const repo = resolveRepoArgs(store, args);
      if (!repo) {
        ok(toolResult({ error: 'Repository not found' }, true));
        return;
      }
      const created = store.createPull(repo, {
        title,
        body: typeof args.body === 'string' ? args.body : '',
        head,
        base,
      });
      if ('error' in created) {
        ok(toolResult({ error: created.error }, true));
        return;
      }
      ok(toolResult(formatPull(repo, created)));
      return;
    }

    if (toolName === 'merge_pull_request') {
      const owner = typeof args.owner === 'string' ? args.owner : '';
      const repoName = typeof args.repo === 'string' ? args.repo : '';
      const pullNumber = Number(args.pull_number);
      if (!owner || !repoName || !Number.isFinite(pullNumber)) {
        ok(toolResult({ error: 'owner, repo, and pull_number are required' }, true));
        return;
      }
      const repo = resolveRepoArgs(store, args);
      const pull = repo?.pulls.get(pullNumber);
      if (!repo || !pull) {
        ok(toolResult({ error: 'Pull request not found' }, true));
        return;
      }
      const resMerge = store.squashMerge(repo, pull);
      if ('conflict' in resMerge) {
        ok(toolResult({ error: 'Merge conflict' }, true));
        return;
      }
      ok(toolResult({ sha: resMerge.sha, merged: true, message: 'Pull Request successfully merged' }));
      return;
    }

    err(-32601, `Unknown tool: ${toolName}`);
    return;
  }

  err(-32601, `Method not found: ${method}`);
}
