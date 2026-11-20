import { createHash } from 'node:crypto';

export type GitObjectType = 'blob' | 'tree' | 'commit' | 'tag';

export interface GitObject {
  readonly type: GitObjectType;
  readonly data: Buffer;
}

export interface TreeEntry {
  readonly mode: string;
  readonly name: string;
  readonly oid: string;
}

export interface ParsedCommit {
  readonly tree: string;
  readonly parents: string[];
  readonly author: string;
  readonly committer: string;
  readonly message: string;
}

export interface ParsedTag {
  readonly object: string;
  readonly type: GitObjectType;
  readonly tag: string;
  readonly tagger: string;
  readonly message: string;
}

export interface Label {
  readonly id: number;
  readonly node_id: string;
  readonly name: string;
  readonly color: string;
  readonly description: string;
}

export interface IssueComment {
  readonly id: number;
  readonly node_id: string;
  readonly body: string;
  readonly user: UserObj;
  readonly created_at: string;
  readonly updated_at: string;
}

export interface Issue {
  id: number;
  node_id: string;
  number: number;
  title: string;
  body: string;
  state: 'open' | 'closed';
  labels: Label[];
  user: UserObj;
  comments: IssueComment[];
  created_at: string;
  updated_at: string;
  closed_at: string | null;
  createdSeq: number;
}

export interface PullRequest {
  id: number;
  node_id: string;
  number: number;
  title: string;
  body: string;
  state: 'open' | 'closed';
  headRef: string;
  headSha: string;
  baseRef: string;
  baseSha: string;
  merged: boolean;
  merged_at: string | null;
  merge_commit_sha: string | null;
  user: UserObj;
  created_at: string;
  updated_at: string;
  closed_at: string | null;
}

export interface UserObj {
  readonly id: number;
  readonly node_id: string;
  readonly login: string;
  readonly name: string;
  readonly email: string;
  readonly type: 'User';
}

export interface Repo {
  readonly id: number;
  readonly node_id: string;
  readonly name: string;
  readonly owner: UserObj;
  readonly full_name: string;
  description: string;
  private: boolean;
  default_branch: string;
  has_issues: boolean;
  readonly created_at: string;
  updated_at: string;
  readonly objects: Map<string, GitObject>;
  readonly refs: Map<string, string>;
  readonly labels: Map<string, Label>;
  readonly issues: Map<number, Issue>;
  readonly pulls: Map<number, PullRequest>;
  nextIssueNumber: number;
  nextPullNumber: number;
  readonly packCache: Map<string, Buffer>;
}

export const ZERO_OID = '0'.repeat(40);

export function hashObject(type: GitObjectType, data: Buffer): string {
  const header = Buffer.from(`${type} ${data.length}\0`, 'utf8');
  return createHash('sha1').update(header).update(data).digest('hex');
}

export function buildTree(entries: readonly TreeEntry[]): Buffer {
  const sorted = [...entries].sort((a, b) => {
    const an = a.mode === '40000' ? `${a.name}/` : a.name;
    const bn = b.mode === '40000' ? `${b.name}/` : b.name;
    return an < bn ? -1 : an > bn ? 1 : 0;
  });
  const chunks: Buffer[] = [];
  for (const entry of sorted) {
    const mode = entry.mode === '040000' ? '40000' : entry.mode;
    chunks.push(Buffer.from(`${mode} ${entry.name}\0`, 'utf8'));
    chunks.push(Buffer.from(entry.oid, 'hex'));
  }
  return Buffer.concat(chunks);
}

export function parseTree(data: Buffer): TreeEntry[] {
  const entries: TreeEntry[] = [];
  let off = 0;
  while (off < data.length) {
    const sp = data.indexOf(0x20, off);
    if (sp < 0) break;
    const mode = data.subarray(off, sp).toString('utf8');
    const nul = data.indexOf(0x00, sp + 1);
    if (nul < 0) break;
    const name = data.subarray(sp + 1, nul).toString('utf8');
    const oid = data.subarray(nul + 1, nul + 21).toString('hex');
    entries.push({ mode, name, oid });
    off = nul + 21;
  }
  return entries;
}

export function buildCommit(opts: {
  tree: string;
  parents: readonly string[];
  author: string;
  committer: string;
  timestamp: number;
  message: string;
}): Buffer {
  const lines = [`tree ${opts.tree}`];
  for (const p of opts.parents) lines.push(`parent ${p}`);
  lines.push(`author ${opts.author} ${opts.timestamp} +0000`);
  lines.push(`committer ${opts.committer} ${opts.timestamp} +0000`);
  const msg = opts.message.endsWith('\n') ? opts.message : `${opts.message}\n`;
  lines.push('', msg);
  return Buffer.from(lines.join('\n'), 'utf8');
}

export function parseCommit(data: Buffer): ParsedCommit {
  const text = data.toString('utf8');
  const sep = text.indexOf('\n\n');
  const headerPart = sep >= 0 ? text.slice(0, sep) : text;
  const message = sep >= 0 ? text.slice(sep + 2) : '';
  let tree = '';
  const parents: string[] = [];
  let author = '';
  let committer = '';
  for (const line of headerPart.split('\n')) {
    if (line.startsWith('tree ')) tree = line.slice(5);
    else if (line.startsWith('parent ')) parents.push(line.slice(7));
    else if (line.startsWith('author ')) author = line.slice(7);
    else if (line.startsWith('committer ')) committer = line.slice(10);
  }
  return { tree, parents, author, committer, message };
}

export function parseTag(data: Buffer): ParsedTag {
  const text = data.toString('utf8');
  const sep = text.indexOf('\n\n');
  const headerPart = sep >= 0 ? text.slice(0, sep) : text;
  const message = sep >= 0 ? text.slice(sep + 2) : '';
  let object = '';
  let type: GitObjectType = 'commit';
  let tag = '';
  let tagger = '';
  for (const line of headerPart.split('\n')) {
    if (line.startsWith('object ')) object = line.slice(7);
    else if (line.startsWith('type ')) type = line.slice(5) as GitObjectType;
    else if (line.startsWith('tag ')) tag = line.slice(4);
    else if (line.startsWith('tagger ')) tagger = line.slice(7);
  }
  return { object, type, tag, tagger, message };
}

export function encodeNodeId(prefix: string, id: number): string {
  return Buffer.from(`${prefix}:${id}`, 'utf8').toString('base64url');
}

export function decodeNodeId(nodeId: string): { prefix: string; id: number } | null {
  try {
    const raw = Buffer.from(nodeId, 'base64url').toString('utf8');
    const idx = raw.indexOf(':');
    if (idx <= 0) return null;
    const id = Number(raw.slice(idx + 1));
    if (!Number.isSafeInteger(id)) return null;
    return { prefix: raw.slice(0, idx), id };
  } catch {
    return null;
  }
}

export class ForgeStore {
  readonly token: string;
  readonly nowIso: string;
  readonly epochSec: number;
  readonly viewer: UserObj;
  readonly reposByName = new Map<string, Repo>();
  readonly reposById = new Map<number, Repo>();
  readonly issuesById = new Map<number, { repo: Repo; issue: Issue }>();
  readonly pullsById = new Map<number, { repo: Repo; pull: PullRequest }>();
  private nextRepoId = 100;
  private nextLabelId = 1000;
  private nextIssueId = 10000;
  private nextPullId = 20000;
  private nextCommentId = 30000;
  private seq = 0;

  constructor(token?: string, nowIso?: string) {
    this.token = token ?? process.env.MAKEFUN_SHOP_TOKEN ?? 'dev-token';
    const rawNow = nowIso ?? process.env.MAKEFUN_TRIAL_NOW ?? '2026-11-14T00:00:00.000Z';
    const parsed = Date.parse(rawNow);
    this.epochSec = Number.isFinite(parsed) ? Math.floor(parsed / 1000) : 1700000000;
    this.nowIso = new Date(this.epochSec * 1000).toISOString();
    this.viewer = {
      id: 1000,
      node_id: encodeNodeId('U', 1000),
      login: process.env.MAKEFUN_FORGE_LOGIN || 'adaiversen',
      name: 'ada iversen',
      email: 'adaiversen@example.invalid',
      type: 'User',
    };
  }

  nextTimestamp(): string {
    this.seq += 1;
    return new Date((this.epochSec + this.seq) * 1000).toISOString();
  }

  nextRequestId(): string {
    this.seq += 1;
    return `req-${this.seq}`;
  }

  isAuthorized(authHeader: string | undefined): boolean {
    if (!authHeader) return false;
    const parts = authHeader.trim().split(/\s+/);
    if (parts.length !== 2) return false;
    const scheme = parts[0].toLowerCase();
    if (scheme !== 'bearer' && scheme !== 'token') return false;
    return parts[1] === this.token;
  }

  getRepo(owner: string, name: string): Repo | undefined {
    if (owner !== this.viewer.login) return undefined;
    return this.reposByName.get(name);
  }

  createRepo(opts: {
    name: string;
    description?: string;
    private?: boolean;
    default_branch?: string;
    has_issues?: boolean;
  }): Repo {
    const id = ++this.nextRepoId;
    const now = this.nowIso;
    const repo: Repo = {
      id,
      node_id: encodeNodeId('R', id),
      name: opts.name,
      owner: this.viewer,
      full_name: `${this.viewer.login}/${opts.name}`,
      description: opts.description ?? '',
      private: Boolean(opts.private ?? false),
      default_branch: opts.default_branch || 'main',
      has_issues: opts.has_issues ?? true,
      created_at: now,
      updated_at: now,
      objects: new Map(),
      refs: new Map(),
      labels: new Map(),
      issues: new Map(),
      pulls: new Map(),
      nextIssueNumber: 1,
      nextPullNumber: 1,
      packCache: new Map(),
    };
    this.reposByName.set(repo.name, repo);
    this.reposById.set(repo.id, repo);
    return repo;
  }

  deleteRepo(owner: string, name: string): boolean {
    const repo = this.getRepo(owner, name);
    if (!repo) return false;
    this.reposByName.delete(name);
    this.reposById.delete(repo.id);
    return true;
  }

  putObject(repo: Repo, type: GitObjectType, data: Buffer): string {
    const oid = hashObject(type, data);
    if (!repo.objects.has(oid)) repo.objects.set(oid, { type, data });
    return oid;
  }

  setRef(repo: Repo, ref: string, oid: string | null): void {
    repo.packCache.clear();
    if (oid === null || oid === ZERO_OID) repo.refs.delete(ref);
    else repo.refs.set(ref, oid);
  }

  resolveRef(repo: Repo, refOrSha: string | undefined): string | undefined {
    const target = refOrSha || repo.default_branch;
    if (repo.objects.has(target)) return target;
    if (target.startsWith('refs/')) return repo.refs.get(target);
    return repo.refs.get(`refs/heads/${target}`) ?? repo.refs.get(`refs/tags/${target}`);
  }

  flattenTree(repo: Repo, treeOid: string | undefined, prefix = '', out = new Map<string, { oid: string; mode: string; data: Buffer }>()) {
    if (!treeOid) return out;
    const obj = repo.objects.get(treeOid);
    if (!obj || obj.type !== 'tree') return out;
    for (const entry of parseTree(obj.data)) {
      const p = prefix ? `${prefix}/${entry.name}` : entry.name;
      if (entry.mode === '40000' || entry.mode === '040000') {
        this.flattenTree(repo, entry.oid, p, out);
      } else {
        const blob = repo.objects.get(entry.oid);
        if (blob && blob.type === 'blob') {
          out.set(p, { oid: entry.oid, mode: entry.mode, data: blob.data });
        }
      }
    }
    return out;
  }

  buildTreeFromFlatMap(repo: Repo, files: Map<string, { oid: string; mode: string }>): string {
    interface DirNode {
      files: Map<string, { oid: string; mode: string }>;
      dirs: Map<string, DirNode>;
    }
    const make = (): DirNode => ({ files: new Map(), dirs: new Map() });
    const root = make();
    for (const [filePath, info] of files) {
      const parts = filePath.split('/').filter(Boolean);
      if (parts.length === 0) continue;
      let cur = root;
      for (let i = 0; i < parts.length - 1; i++) {
        let next = cur.dirs.get(parts[i]);
        if (!next) {
          next = make();
          cur.dirs.set(parts[i], next);
        }
        cur = next;
      }
      cur.files.set(parts[parts.length - 1], info);
    }
    const writeDir = (node: DirNode): string => {
      const entries: TreeEntry[] = [];
      for (const [name, info] of node.files) entries.push({ mode: info.mode, name, oid: info.oid });
      for (const [name, child] of node.dirs) entries.push({ mode: '40000', name, oid: writeDir(child) });
      return this.putObject(repo, 'tree', buildTree(entries));
    };
    return writeDir(root);
  }

  commitFileChange(
    repo: Repo,
    opts: { branch: string; path: string; content: Buffer | null; message: string },
  ): { commitSha: string; blobSha: string | null } {
    const refName = opts.branch.startsWith('refs/') ? opts.branch : `refs/heads/${opts.branch}`;
    const parentSha = repo.refs.get(refName);
    let flat = new Map<string, { oid: string; mode: string }>();
    if (parentSha) {
      const parentObj = repo.objects.get(parentSha);
      if (parentObj && parentObj.type === 'commit') {
        const parsed = parseCommit(parentObj.data);
        const existing = this.flattenTree(repo, parsed.tree);
        for (const [p, v] of existing) flat.set(p, { oid: v.oid, mode: v.mode });
      }
    }
    let blobSha: string | null = null;
    if (opts.content !== null) {
      blobSha = this.putObject(repo, 'blob', opts.content);
      flat.set(opts.path, { oid: blobSha, mode: '100644' });
    } else {
      flat.delete(opts.path);
    }
    const treeOid = this.buildTreeFromFlatMap(repo, flat);
    const author = `${this.viewer.name} <${this.viewer.email}>`;
    this.seq += 1;
    const commitData = buildCommit({
      tree: treeOid,
      parents: parentSha ? [parentSha] : [],
      author,
      committer: author,
      timestamp: this.epochSec + this.seq,
      message: opts.message,
    });
    const commitSha = this.putObject(repo, 'commit', commitData);
    this.setRef(repo, refName, commitSha);
    return { commitSha, blobSha };
  }

  isAncestor(repo: Repo, ancOid: string, descOid: string): boolean {
    if (ancOid === descOid) return true;
    const seen = new Set<string>();
    const stack = [descOid];
    while (stack.length > 0) {
      const cur = stack.pop()!;
      if (cur === ancOid) return true;
      if (seen.has(cur)) continue;
      seen.add(cur);
      const obj = repo.objects.get(cur);
      if (!obj || obj.type !== 'commit') continue;
      for (const p of parseCommit(obj.data).parents) stack.push(p);
    }
    return false;
  }

  ancestorsOf(repo: Repo, startOid: string): Map<string, number> {
    const dist = new Map<string, number>();
    const q: Array<[string, number]> = [[startOid, 0]];
    while (q.length > 0) {
      const [oid, d] = q.shift()!;
      if (dist.has(oid)) continue;
      dist.set(oid, d);
      const obj = repo.objects.get(oid);
      if (!obj || obj.type !== 'commit') continue;
      for (const p of parseCommit(obj.data).parents) q.push([p, d + 1]);
    }
    return dist;
  }

  findMergeBase(repo: Repo, oidA: string, oidB: string): string | undefined {
    const aDist = this.ancestorsOf(repo, oidA);
    const bDist = this.ancestorsOf(repo, oidB);
    let best: string | undefined;
    let bestScore = Infinity;
    for (const [oid, da] of aDist) {
      const db = bDist.get(oid);
      if (db !== undefined && da + db < bestScore) {
        bestScore = da + db;
        best = oid;
      }
    }
    return best;
  }

  diffCommits(repo: Repo, baseCommitSha: string, headCommitSha: string): Array<{
    filename: string;
    status: 'added' | 'modified' | 'removed';
    additions: number;
    deletions: number;
    changes: number;
  }> {
    const lca = this.findMergeBase(repo, baseCommitSha, headCommitSha) ?? baseCommitSha;
    const baseTree = this.treeOfCommit(repo, lca);
    const headTree = this.treeOfCommit(repo, headCommitSha);
    const baseFiles = this.flattenTree(repo, baseTree);
    const headFiles = this.flattenTree(repo, headTree);
    const allPaths = new Set([...baseFiles.keys(), ...headFiles.keys()]);
    const out: Array<{ filename: string; status: 'added' | 'modified' | 'removed'; additions: number; deletions: number; changes: number }> = [];
    for (const p of [...allPaths].sort()) {
      const b = baseFiles.get(p);
      const h = headFiles.get(p);
      if (!b && h) {
        const lines = countLines(h.data);
        out.push({ filename: p, status: 'added', additions: lines, deletions: 0, changes: lines });
      } else if (b && !h) {
        const lines = countLines(b.data);
        out.push({ filename: p, status: 'removed', additions: 0, deletions: lines, changes: lines });
      } else if (b && h && b.oid !== h.oid) {
        const bLines = countLines(b.data);
        const hLines = countLines(h.data);
        const additions = Math.max(1, hLines);
        const deletions = Math.max(1, bLines);
        out.push({ filename: p, status: 'modified', additions, deletions, changes: additions + deletions });
      }
    }
    return out;
  }

  treeOfCommit(repo: Repo, commitSha: string | undefined): string | undefined {
    if (!commitSha) return undefined;
    const obj = repo.objects.get(commitSha);
    if (!obj || obj.type !== 'commit') return undefined;
    return parseCommit(obj.data).tree;
  }

  squashMerge(repo: Repo, pull: PullRequest, message?: string): { sha: string } | { conflict: true } {
    const baseRef = `refs/heads/${pull.baseRef}`;
    const baseTip = repo.refs.get(baseRef) ?? pull.baseSha;
    const headTip = repo.refs.get(`refs/heads/${pull.headRef}`) ?? pull.headSha;
    const lca = this.findMergeBase(repo, baseTip, headTip);

    const lcaFiles = this.flattenTree(repo, this.treeOfCommit(repo, lca));
    const baseFiles = this.flattenTree(repo, this.treeOfCommit(repo, baseTip));
    const headFiles = this.flattenTree(repo, this.treeOfCommit(repo, headTip));

    const allPaths = new Set([...lcaFiles.keys(), ...baseFiles.keys(), ...headFiles.keys()]);
    const mergedFlat = new Map<string, { oid: string; mode: string }>();

    for (const p of allPaths) {
      const l = lcaFiles.get(p);
      const b = baseFiles.get(p);
      const h = headFiles.get(p);
      const lOid = l?.oid;
      const bOid = b?.oid;
      const hOid = h?.oid;

      if (bOid === hOid) {
        if (b) mergedFlat.set(p, { oid: b.oid, mode: b.mode });
      } else if (bOid === lOid) {
        if (h) mergedFlat.set(p, { oid: h.oid, mode: h.mode });
      } else if (hOid === lOid) {
        if (b) mergedFlat.set(p, { oid: b.oid, mode: b.mode });
      } else {
        return { conflict: true };
      }
    }

    const treeOid = this.buildTreeFromFlatMap(repo, mergedFlat);
    const author = `${this.viewer.name} <${this.viewer.email}>`;
    this.seq += 1;
    const commitData = buildCommit({
      tree: treeOid,
      parents: baseTip ? [baseTip] : [],
      author,
      committer: author,
      timestamp: this.epochSec + this.seq,
      message: message || `${pull.title} (#${pull.number})`,
    });
    const sha = this.putObject(repo, 'commit', commitData);
    this.setRef(repo, baseRef, sha);
    const now = this.nextTimestamp();
    pull.merged = true;
    pull.state = 'closed';
    pull.merged_at = now;
    pull.closed_at = now;
    pull.updated_at = now;
    pull.merge_commit_sha = sha;
    return { sha };
  }

  createLabel(repo: Repo, name: string, color = 'ededed', description = ''): Label {
    const existing = repo.labels.get(name);
    if (existing) return existing;
    const id = ++this.nextLabelId;
    const label: Label = { id, node_id: encodeNodeId('LA', id), name, color, description };
    repo.labels.set(name, label);
    return label;
  }

  createIssue(repo: Repo, opts: { title: string; body?: string; labels?: string[] }): Issue | { badLabel: string } {
    const resolvedLabels: Label[] = [];
    for (const lname of opts.labels ?? []) {
      const lbl = repo.labels.get(lname);
      if (!lbl) return { badLabel: lname };
      resolvedLabels.push(lbl);
    }
    const id = ++this.nextIssueId;
    const number = repo.nextIssueNumber++;
    const now = this.nextTimestamp();
    this.seq += 1;
    const issue: Issue = {
      id,
      node_id: encodeNodeId('I', id),
      number,
      title: opts.title,
      body: opts.body ?? '',
      state: 'open',
      labels: resolvedLabels,
      user: this.viewer,
      comments: [],
      created_at: now,
      updated_at: now,
      closed_at: null,
      createdSeq: this.seq,
    };
    repo.issues.set(number, issue);
    this.issuesById.set(id, { repo, issue });
    return issue;
  }

  addIssueComment(issue: Issue, body: string): IssueComment {
    const id = ++this.nextCommentId;
    const now = this.nextTimestamp();
    const comment: IssueComment = {
      id,
      node_id: encodeNodeId('IC', id),
      body,
      user: this.viewer,
      created_at: now,
      updated_at: now,
    };
    issue.comments.push(comment);
    issue.updated_at = now;
    return comment;
  }

  createPull(repo: Repo, opts: { title: string; body?: string; head: string; base: string }): PullRequest | { error: string } {
    const headSha = this.resolveRef(repo, opts.head);
    const baseSha = this.resolveRef(repo, opts.base);
    if (!headSha || !baseSha) return { error: 'Branch not found' };
    const id = ++this.nextPullId;
    const number = repo.nextPullNumber++;
    const now = this.nextTimestamp();
    const pull: PullRequest = {
      id,
      node_id: encodeNodeId('PR', id),
      number,
      title: opts.title,
      body: opts.body ?? '',
      state: 'open',
      headRef: opts.head.replace(/^refs\/heads\//, ''),
      headSha,
      baseRef: opts.base.replace(/^refs\/heads\//, ''),
      baseSha,
      merged: false,
      merged_at: null,
      merge_commit_sha: null,
      user: this.viewer,
      created_at: now,
      updated_at: now,
      closed_at: null,
    };
    repo.pulls.set(number, pull);
    this.pullsById.set(id, { repo, pull });
    return pull;
  }
}

function countLines(buf: Buffer): number {
  if (buf.length === 0) return 0;
  let count = 0;
  for (let i = 0; i < buf.length; i++) {
    if (buf[i] === 0x0a) count += 1;
  }
  if (buf[buf.length - 1] !== 0x0a) count += 1;
  return count;
}
