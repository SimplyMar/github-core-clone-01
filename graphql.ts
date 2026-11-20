import type { IncomingMessage, ServerResponse } from 'node:http';
import { sendJson } from './rest.ts';
import { ForgeStore, decodeNodeId, type Issue, type PullRequest, type Repo } from './store.ts';

interface FieldNode {
  name: string;
  alias: string | null;
  args: Record<string, unknown>;
  selections: Selection[];
}

type Selection =
  | { kind: 'field'; field: FieldNode }
  | { kind: 'inlineFragment'; typeCondition: string | null; selections: Selection[] }
  | { kind: 'fragmentSpread'; name: string };

interface OperationDef {
  opType: 'query' | 'mutation';
  name: string | null;
  selections: Selection[];
}

interface ParsedDoc {
  operations: OperationDef[];
  fragments: Map<string, Selection[]>;
}

const INTROSPECTION_TYPES: Record<string, { fields?: string[]; enumValues?: string[] }> = {
  Issue: { fields: ['id', 'number', 'title', 'state', 'body', 'createdAt', 'updatedAt', 'closedAt', 'comments'] },
  PullRequest: {
    fields: [
      'id',
      'number',
      'title',
      'body',
      'headRefName',
      'baseRefName',
      'mergeable',
      'state',
      'merged',
      'files',
      'mergeCommit',
    ],
  },
  Repository: {
    fields: [
      'id',
      'databaseId',
      'name',
      'nameWithOwner',
      'owner',
      'defaultBranchRef',
      'viewerPermission',
      'hasIssuesEnabled',
      'issue',
      'issues',
      'pullRequests',
      'pullRequest',
    ],
  },
  SearchType: { enumValues: ['ISSUE', 'REPOSITORY', 'USER', 'DISCUSSION'] },
};

const VALID_FIELDS: Record<string, Set<string>> = {
  Query: new Set(['viewer', 'repository', 'node', '__type']),
  User: new Set(['id', 'login', 'name']),
  Repository: new Set([...(INTROSPECTION_TYPES.Repository.fields ?? []), '__typename']),
  Issue: new Set([...(INTROSPECTION_TYPES.Issue.fields ?? []), '__typename']),
  PullRequest: new Set([...(INTROSPECTION_TYPES.PullRequest.fields ?? []), '__typename']),
  Ref: new Set(['name']),
  IssueConnection: new Set(['nodes', 'pageInfo', 'totalCount']),
  PullRequestConnection: new Set(['nodes', 'pageInfo', 'totalCount']),
  PullRequestFileConnection: new Set(['nodes']),
  PullRequestFile: new Set(['path', 'additions', 'deletions']),
  PageInfo: new Set(['hasNextPage', 'endCursor']),
  __Type: new Set(['name', 'fields', 'enumValues']),
  __Field: new Set(['name']),
  __EnumValue: new Set(['name']),
  Mutation: new Set(['createIssue', 'closeIssue', 'createPullRequest', 'mergePullRequest', 'addComment']),
};

function tokenize(src: string): string[] {
  const tokens: string[] = [];
  let i = 0;
  while (i < src.length) {
    const ch = src[i];
    if (ch === ' ' || ch === '\t' || ch === '\n' || ch === '\r' || ch === ',') {
      i++;
      continue;
    }
    if (ch === '#') {
      while (i < src.length && src[i] !== '\n') i++;
      continue;
    }
    if (ch === '.' && src.slice(i, i + 3) === '...') {
      tokens.push('...');
      i += 3;
      continue;
    }
    if ('{}():[]=!@$'.includes(ch)) {
      tokens.push(ch);
      i++;
      continue;
    }
    if (ch === '"') {
      let j = i + 1;
      let s = '';
      while (j < src.length && src[j] !== '"') {
        if (src[j] === '\\' && j + 1 < src.length) {
          const esc = src[j + 1];
          s += esc === 'n' ? '\n' : esc === 't' ? '\t' : esc;
          j += 2;
        } else {
          s += src[j++];
        }
      }
      tokens.push(JSON.stringify(s));
      i = j + 1;
      continue;
    }
    let j = i;
    while (j < src.length && !' \t\n\r,{}():[]=!@$#"'.includes(src[j])) j++;
    tokens.push(src.slice(i, j));
    i = j;
  }
  return tokens;
}

function parseGraphQL(src: string, vars: Record<string, unknown>): ParsedDoc {
  const tokens = tokenize(src);
  let pos = 0;
  const peek = () => tokens[pos];
  const next = () => tokens[pos++];

  const parseValue = (): unknown => {
    const t = next();
    if (t === '$') {
      const varName = next();
      return vars[varName];
    }
    if (t === '{') {
      const obj: Record<string, unknown> = {};
      while (peek() && peek() !== '}') {
        const k = next();
        if (peek() === ':') next();
        obj[k] = parseValue();
      }
      if (peek() === '}') next();
      return obj;
    }
    if (t === '[') {
      const arr: unknown[] = [];
      while (peek() && peek() !== ']') arr.push(parseValue());
      if (peek() === ']') next();
      return arr;
    }
    if (t.startsWith('"')) return JSON.parse(t);
    if (t === 'true') return true;
    if (t === 'false') return false;
    if (t === 'null') return null;
    if (/^-?\d+(\.\d+)?$/.test(t)) return Number(t);
    return t;
  };

  const parseArgs = (): Record<string, unknown> => {
    const args: Record<string, unknown> = {};
    if (peek() !== '(') return args;
    next();
    while (peek() && peek() !== ')') {
      const name = next();
      if (peek() === ':') next();
      args[name] = parseValue();
    }
    if (peek() === ')') next();
    return args;
  };

  const parseSelections = (): Selection[] => {
    const sels: Selection[] = [];
    if (peek() !== '{') return sels;
    next();
    while (peek() && peek() !== '}') {
      if (peek() === '...') {
        next();
        if (peek() === 'on') {
          next();
          const typeCondition = next();
          sels.push({ kind: 'inlineFragment', typeCondition, selections: parseSelections() });
        } else if (peek() === '{') {
          sels.push({ kind: 'inlineFragment', typeCondition: null, selections: parseSelections() });
        } else {
          const name = next();
          sels.push({ kind: 'fragmentSpread', name });
        }
      } else {
        const first = next();
        let alias: string | null = null;
        let name = first;
        if (peek() === ':') {
          next();
          alias = first;
          name = next();
        }
        const args = parseArgs();
        const selections = peek() === '{' ? parseSelections() : [];
        sels.push({ kind: 'field', field: { name, alias, args, selections } });
      }
    }
    if (peek() === '}') next();
    return sels;
  };

  const operations: OperationDef[] = [];
  const fragments = new Map<string, Selection[]>();

  while (pos < tokens.length) {
    const t = peek();
    if (t === '{') {
      operations.push({ opType: 'query', name: null, selections: parseSelections() });
    } else if (t === 'query' || t === 'mutation') {
      const opType = next() as 'query' | 'mutation';
      let name: string | null = null;
      if (peek() && peek() !== '(' && peek() !== '{') name = next();
      if (peek() === '(') {
        let depth = 0;
        do {
          const tok = next();
          if (tok === '(') depth++;
          else if (tok === ')') depth--;
        } while (pos < tokens.length && depth > 0);
      }
      operations.push({ opType, name, selections: parseSelections() });
    } else if (t === 'fragment') {
      next();
      const name = next();
      if (peek() === 'on') {
        next();
        next();
      }
      fragments.set(name, parseSelections());
    } else {
      next();
    }
  }

  return { operations, fragments };
}

function expandSelections(sels: Selection[], fragments: Map<string, Selection[]>, typeName?: string): FieldNode[] {
  const out: FieldNode[] = [];
  for (const s of sels) {
    if (s.kind === 'field') out.push(s.field);
    else if (s.kind === 'inlineFragment') {
      if (!s.typeCondition || !typeName || s.typeCondition === typeName) {
        out.push(...expandSelections(s.selections, fragments, typeName));
      }
    } else if (s.kind === 'fragmentSpread') {
      const frag = fragments.get(s.name);
      if (frag) out.push(...expandSelections(frag, fragments, typeName));
    }
  }
  return out;
}

function executeDoc(store: ForgeStore, query: string, vars: Record<string, unknown>, operationName?: string) {
  const doc = parseGraphQL(query, vars);
  const op =
    (operationName ? doc.operations.find((o) => o.name === operationName) : undefined) ?? doc.operations[0];
  if (!op) return { data: null, errors: [{ message: 'No operation found' }] };

  const errors: Array<{ message: string }> = [];

  const checkField = (typeName: string, fieldName: string): boolean => {
    const allowed = VALID_FIELDS[typeName];
    if (allowed && !allowed.has(fieldName)) {
      errors.push({ message: `Field '${fieldName}' doesn't exist on type '${typeName}'` });
      return false;
    }
    return true;
  };

  const resolveIssue = (issue: Issue, sels: Selection[]) => {
    const res: Record<string, unknown> = {};
    for (const f of expandSelections(sels, doc.fragments, 'Issue')) {
      const key = f.alias ?? f.name;
      if (!checkField('Issue', f.name)) continue;
      if (f.name === 'id') res[key] = issue.node_id;
      else if (f.name === 'number') res[key] = issue.number;
      else if (f.name === 'title') res[key] = issue.title;
      else if (f.name === 'body') res[key] = issue.body;
      else if (f.name === 'state') res[key] = issue.state.toUpperCase();
      else if (f.name === 'createdAt') res[key] = issue.created_at;
      else if (f.name === 'updatedAt') res[key] = issue.updated_at;
      else if (f.name === 'closedAt') res[key] = issue.closed_at;
      else if (f.name === '__typename') res[key] = 'Issue';
    }
    return res;
  };

  const resolvePull = (repo: Repo, pull: PullRequest, sels: Selection[]) => {
    const res: Record<string, unknown> = {};
    for (const f of expandSelections(sels, doc.fragments, 'PullRequest')) {
      const key = f.alias ?? f.name;
      if (!checkField('PullRequest', f.name)) continue;
      if (f.name === 'id') res[key] = pull.node_id;
      else if (f.name === 'number') res[key] = pull.number;
      else if (f.name === 'title') res[key] = pull.title;
      else if (f.name === 'body') res[key] = pull.body;
      else if (f.name === 'headRefName') res[key] = pull.headRef;
      else if (f.name === 'baseRefName') res[key] = pull.baseRef;
      else if (f.name === 'mergeable') res[key] = 'MERGEABLE';
      else if (f.name === 'state') res[key] = pull.merged ? 'MERGED' : pull.state.toUpperCase();
      else if (f.name === 'merged') res[key] = pull.merged;
      else if (f.name === '__typename') res[key] = 'PullRequest';
      else if (f.name === 'files') {
        const first = typeof f.args.first === 'number' ? f.args.first : 30;
        const diff = store.diffCommits(repo, pull.baseSha, pull.headSha).slice(0, first);
        const conn: Record<string, unknown> = {};
        for (const sf of expandSelections(f.selections, doc.fragments)) {
          const skey = sf.alias ?? sf.name;
          if (sf.name === 'nodes') {
            conn[skey] = diff.map((d) => {
              const item: Record<string, unknown> = {};
              for (const nf of expandSelections(sf.selections, doc.fragments)) {
                const nkey = nf.alias ?? nf.name;
                if (nf.name === 'path') item[nkey] = d.filename;
                else if (nf.name === 'additions') item[nkey] = d.additions;
                else if (nf.name === 'deletions') item[nkey] = d.deletions;
              }
              return item;
            });
          }
        }
        res[key] = conn;
      }
    }
    return res;
  };

  const resolveRepo = (repo: Repo, sels: Selection[]) => {
    const res: Record<string, unknown> = {};
    for (const f of expandSelections(sels, doc.fragments, 'Repository')) {
      const key = f.alias ?? f.name;
      if (!checkField('Repository', f.name)) continue;
      if (f.name === 'id') res[key] = repo.node_id;
      else if (f.name === 'databaseId') res[key] = repo.id;
      else if (f.name === 'name') res[key] = repo.name;
      else if (f.name === 'nameWithOwner') res[key] = repo.full_name;
      else if (f.name === 'viewerPermission') res[key] = 'ADMIN';
      else if (f.name === 'hasIssuesEnabled') res[key] = repo.has_issues;
      else if (f.name === '__typename') res[key] = 'Repository';
      else if (f.name === 'owner') {
        const ownerObj: Record<string, unknown> = {};
        for (const sf of expandSelections(f.selections, doc.fragments)) {
          const skey = sf.alias ?? sf.name;
          if (sf.name === 'login') ownerObj[skey] = repo.owner.login;
          else if (sf.name === 'id') ownerObj[skey] = repo.owner.node_id;
        }
        res[key] = ownerObj;
      } else if (f.name === 'defaultBranchRef') {
        const refObj: Record<string, unknown> = {};
        for (const sf of expandSelections(f.selections, doc.fragments)) {
          const skey = sf.alias ?? sf.name;
          if (sf.name === 'name') refObj[skey] = repo.default_branch;
        }
        res[key] = refObj;
      } else if (f.name === 'issue') {
        const num = Number(f.args.number);
        const issue = repo.issues.get(num);
        res[key] = issue ? resolveIssue(issue, f.selections) : null;
      } else if (f.name === 'issues') {
        let list = [...repo.issues.values()];
        if (Array.isArray(f.args.states)) {
          const states = new Set(f.args.states.map((s) => String(s).toUpperCase()));
          list = list.filter((i) => states.has(i.state.toUpperCase()));
        }
        const orderBy = f.args.orderBy as { field?: string; direction?: string } | undefined;
        if (orderBy?.direction === 'DESC') {
          list.sort((a, b) => b.createdSeq - a.createdSeq || b.number - a.number);
        } else {
          list.sort((a, b) => a.createdSeq - b.createdSeq || a.number - b.number);
        }
        let startIdx = 0;
        if (typeof f.args.after === 'string' && f.args.after) {
          const cursorNum = parseInt(f.args.after.replace('cursor:', ''), 10);
          const idx = list.findIndex((i) => i.number === cursorNum);
          if (idx >= 0) startIdx = idx + 1;
        }
        const first = typeof f.args.first === 'number' ? f.args.first : 30;
        const slice = list.slice(startIdx, startIdx + first);
        const hasNextPage = startIdx + first < list.length;
        const endCursor = slice.length > 0 ? `cursor:${slice[slice.length - 1].number}` : null;
        const conn: Record<string, unknown> = {};
        for (const sf of expandSelections(f.selections, doc.fragments)) {
          const skey = sf.alias ?? sf.name;
          if (sf.name === 'nodes') conn[skey] = slice.map((i) => resolveIssue(i, sf.selections));
          else if (sf.name === 'totalCount') conn[skey] = list.length;
          else if (sf.name === 'pageInfo') {
            const pi: Record<string, unknown> = {};
            for (const pf of expandSelections(sf.selections, doc.fragments)) {
              const pkey = pf.alias ?? pf.name;
              if (pf.name === 'hasNextPage') pi[pkey] = hasNextPage;
              else if (pf.name === 'endCursor') pi[pkey] = endCursor;
            }
            conn[skey] = pi;
          }
        }
        res[key] = conn;
      } else if (f.name === 'pullRequest') {
        const num = Number(f.args.number);
        const pull = repo.pulls.get(num);
        res[key] = pull ? resolvePull(repo, pull, f.selections) : null;
      } else if (f.name === 'pullRequests') {
        let list = [...repo.pulls.values()];
        const first = typeof f.args.first === 'number' ? f.args.first : 30;
        const slice = list.slice(0, first);
        const conn: Record<string, unknown> = {};
        for (const sf of expandSelections(f.selections, doc.fragments)) {
          const skey = sf.alias ?? sf.name;
          if (sf.name === 'nodes') conn[skey] = slice.map((p) => resolvePull(repo, p, sf.selections));
        }
        res[key] = conn;
      }
    }
    return res;
  };

  const data: Record<string, unknown> = {};
  for (const f of expandSelections(op.selections, doc.fragments)) {
    const key = f.alias ?? f.name;
    if (op.opType === 'query') {
      if (!checkField('Query', f.name)) continue;
      if (f.name === 'viewer') {
        const v: Record<string, unknown> = {};
        for (const sf of expandSelections(f.selections, doc.fragments)) {
          const skey = sf.alias ?? sf.name;
          if (!checkField('User', sf.name)) continue;
          if (sf.name === 'login') v[skey] = store.viewer.login;
          else if (sf.name === 'id') v[skey] = store.viewer.node_id;
          else if (sf.name === 'name') v[skey] = store.viewer.name;
        }
        data[key] = v;
      } else if (f.name === 'repository') {
        const repo = store.getRepo(String(f.args.owner ?? ''), String(f.args.name ?? ''));
        data[key] = repo ? resolveRepo(repo, f.selections) : null;
      } else if (f.name === 'node') {
        const decoded = decodeNodeId(String(f.args.id ?? ''));
        if (decoded?.prefix === 'R') {
          const repo = store.reposById.get(decoded.id);
          data[key] = repo ? resolveRepo(repo, f.selections) : null;
        } else {
          data[key] = null;
        }
      } else if (f.name === '__type') {
        const tname = String(f.args.name ?? '');
        const spec = INTROSPECTION_TYPES[tname];
        if (!spec) {
          data[key] = null;
        } else {
          const out: Record<string, unknown> = {};
          for (const sf of expandSelections(f.selections, doc.fragments)) {
            const skey = sf.alias ?? sf.name;
            if (sf.name === 'name') out[skey] = tname;
            else if (sf.name === 'fields') {
              out[skey] = (spec.fields ?? []).map((fn) => ({ name: fn }));
            } else if (sf.name === 'enumValues') {
              out[skey] = (spec.enumValues ?? []).map((ev) => ({ name: ev }));
            }
          }
          data[key] = out;
        }
      }
    } else {
      if (!checkField('Mutation', f.name)) continue;
      const input = (f.args.input ?? {}) as Record<string, unknown>;
      if (f.name === 'createIssue') {
        const decoded = decodeNodeId(String(input.repositoryId ?? ''));
        const repo = decoded?.prefix === 'R' ? store.reposById.get(decoded.id) : undefined;
        if (!repo) {
          errors.push({ message: 'Repository not found' });
          data[key] = null;
          continue;
        }
        const created = store.createIssue(repo, {
          title: String(input.title ?? ''),
          body: typeof input.body === 'string' ? input.body : '',
        });
        if ('badLabel' in created) {
          errors.push({ message: 'Invalid label' });
          data[key] = null;
          continue;
        }
        const payload: Record<string, unknown> = {};
        for (const sf of expandSelections(f.selections, doc.fragments)) {
          if (sf.name === 'issue') payload[sf.alias ?? sf.name] = resolveIssue(created, sf.selections);
        }
        data[key] = payload;
      } else if (f.name === 'closeIssue') {
        const decoded = decodeNodeId(String(input.issueId ?? ''));
        const found = decoded?.prefix === 'I' ? store.issuesById.get(decoded.id) : undefined;
        if (!found) {
          errors.push({ message: 'Issue not found' });
          data[key] = null;
          continue;
        }
        found.issue.state = 'closed';
        found.issue.closed_at = store.nextTimestamp();
        found.issue.updated_at = found.issue.closed_at;
        const payload: Record<string, unknown> = {};
        for (const sf of expandSelections(f.selections, doc.fragments)) {
          if (sf.name === 'issue') payload[sf.alias ?? sf.name] = resolveIssue(found.issue, sf.selections);
        }
        data[key] = payload;
      } else if (f.name === 'createPullRequest') {
        const decoded = decodeNodeId(String(input.repositoryId ?? ''));
        const repo = decoded?.prefix === 'R' ? store.reposById.get(decoded.id) : undefined;
        if (!repo) {
          errors.push({ message: 'Repository not found' });
          data[key] = null;
          continue;
        }
        const created = store.createPull(repo, {
          title: String(input.title ?? ''),
          body: typeof input.body === 'string' ? input.body : '',
          head: String(input.headRefName ?? ''),
          base: String(input.baseRefName ?? repo.default_branch),
        });
        if ('error' in created) {
          errors.push({ message: created.error });
          data[key] = null;
          continue;
        }
        const payload: Record<string, unknown> = {};
        for (const sf of expandSelections(f.selections, doc.fragments)) {
          if (sf.name === 'pullRequest') payload[sf.alias ?? sf.name] = resolvePull(repo, created, sf.selections);
        }
        data[key] = payload;
      } else if (f.name === 'mergePullRequest') {
        const decoded = decodeNodeId(String(input.pullRequestId ?? ''));
        const found = decoded?.prefix === 'PR' ? store.pullsById.get(decoded.id) : undefined;
        if (!found) {
          errors.push({ message: 'Pull request not found' });
          data[key] = null;
          continue;
        }
        const resMerge = store.squashMerge(found.repo, found.pull);
        if ('conflict' in resMerge) {
          errors.push({ message: 'Merge conflict' });
          data[key] = null;
          continue;
        }
        const payload: Record<string, unknown> = {};
        for (const sf of expandSelections(f.selections, doc.fragments)) {
          if (sf.name === 'pullRequest') payload[sf.alias ?? sf.name] = resolvePull(found.repo, found.pull, sf.selections);
        }
        data[key] = payload;
      } else if (f.name === 'addComment') {
        const decoded = decodeNodeId(String(input.subjectId ?? ''));
        const found = decoded?.prefix === 'I' ? store.issuesById.get(decoded.id) : undefined;
        if (!found) {
          errors.push({ message: 'Subject not found' });
          data[key] = null;
          continue;
        }
        const comment = store.addIssueComment(found.issue, String(input.body ?? ''));
        const payload: Record<string, unknown> = {};
        for (const sf of expandSelections(f.selections, doc.fragments)) {
          if (sf.name === 'commentEdge') {
            const edge: Record<string, unknown> = {};
            for (const ef of expandSelections(sf.selections, doc.fragments)) {
              if (ef.name === 'node') {
                const node: Record<string, unknown> = {};
                for (const nf of expandSelections(ef.selections, doc.fragments)) {
                  const nkey = nf.alias ?? nf.name;
                  if (nf.name === 'id') node[nkey] = comment.node_id;
                  else if (nf.name === 'body') node[nkey] = comment.body;
                }
                edge[ef.alias ?? ef.name] = node;
              }
            }
            payload[sf.alias ?? sf.name] = edge;
          }
        }
        data[key] = payload;
      }
    }
  }

  return errors.length > 0 ? { data, errors } : { data };
}

export function handleGraphQLRequest(
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

  let parsed: unknown;
  try {
    parsed = JSON.parse(rawBody.toString('utf8'));
  } catch {
    sendJson(res, 400, { message: 'Problems parsing JSON' }, reqId);
    return;
  }

  if (Array.isArray(parsed)) {
    const results = parsed.map((item) => {
      const rec = (item ?? {}) as Record<string, unknown>;
      return executeDoc(
        store,
        String(rec.query ?? ''),
        (rec.variables as Record<string, unknown>) ?? {},
        typeof rec.operationName === 'string' ? rec.operationName : undefined,
      );
    });
    sendJson(res, 200, results, reqId);
    return;
  }

  if (typeof parsed !== 'object' || parsed === null) {
    sendJson(res, 400, { message: 'Body must be JSON object or array' }, reqId);
    return;
  }

  const rec = parsed as Record<string, unknown>;
  const result = executeDoc(
    store,
    String(rec.query ?? ''),
    (rec.variables as Record<string, unknown>) ?? {},
    typeof rec.operationName === 'string' ? rec.operationName : undefined,
  );
  sendJson(res, 200, result, reqId);
}
