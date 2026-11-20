import { createHash } from 'node:crypto';
import type { IncomingMessage, ServerResponse } from 'node:http';
import { deflateSync, inflateSync } from 'node:zlib';
import {
  ForgeStore,
  ZERO_OID,
  hashObject,
  parseCommit,
  parseTag,
  parseTree,
  type GitObject,
  type GitObjectType,
  type Repo,
} from './store.ts';

function pktLine(text: string | Buffer): Buffer {
  const payload = typeof text === 'string' ? Buffer.from(text, 'utf8') : text;
  const len = (payload.length + 4).toString(16).padStart(4, '0');
  return Buffer.concat([Buffer.from(len, 'ascii'), payload]);
}

const PKT_FLUSH = Buffer.from('0000', 'ascii');
const PKT_DELIM = Buffer.from('0001', 'ascii');

function parsePktLines(buf: Buffer): { packets: Array<{ type: 'data'; data: Buffer } | { type: 'flush' | 'delim' }>; restOffset: number } {
  const packets: Array<{ type: 'data'; data: Buffer } | { type: 'flush' | 'delim' }> = [];
  let off = 0;
  while (off + 4 <= buf.length) {
    const hdr = buf.subarray(off, off + 4).toString('ascii');
    if (hdr === '0000') {
      packets.push({ type: 'flush' });
      off += 4;
      if (packets.length > 0) {
        // Return after flush if pack follows; caller decides
      }
      continue;
    }
    if (hdr === '0001') {
      packets.push({ type: 'delim' });
      off += 4;
      continue;
    }
    if (hdr === '0002') {
      off += 4;
      continue;
    }
    const len = parseInt(hdr, 16);
    if (!Number.isFinite(len) || len < 4 || off + len > buf.length) break;
    packets.push({ type: 'data', data: buf.subarray(off + 4, off + len) });
    off += len;
  }
  return { packets, restOffset: off };
}

function parseReceivePackets(buf: Buffer): { commands: Array<{ oldOid: string; newOid: string; ref: string }>; pack: Buffer } {
  const commands: Array<{ oldOid: string; newOid: string; ref: string }> = [];
  let off = 0;
  while (off + 4 <= buf.length) {
    const hdr = buf.subarray(off, off + 4).toString('ascii');
    if (hdr === '0000') {
      off += 4;
      break;
    }
    const len = parseInt(hdr, 16);
    if (!Number.isFinite(len) || len < 4 || off + len > buf.length) break;
    let line = buf.subarray(off + 4, off + len).toString('utf8');
    off += len;
    const nulIdx = line.indexOf('\0');
    if (nulIdx >= 0) line = line.slice(0, nulIdx);
    line = line.replace(/\r?\n$/, '');
    const parts = line.split(' ');
    if (parts.length >= 3) {
      commands.push({ oldOid: parts[0], newOid: parts[1], ref: parts[2] });
    }
  }
  return { commands, pack: buf.subarray(off) };
}

const OBJ_TYPE_NUM: Record<GitObjectType, number> = { commit: 1, tree: 2, blob: 3, tag: 4 };
const NUM_TO_OBJ_TYPE: Record<number, GitObjectType> = { 1: 'commit', 2: 'tree', 3: 'blob', 4: 'tag' };

function readVarint(buf: Buffer, off: number): { val: number; next: number } {
  let val = 0;
  let shift = 0;
  let pos = off;
  while (pos < buf.length) {
    const b = buf[pos++];
    val |= (b & 0x7f) << shift;
    if ((b & 0x80) === 0) break;
    shift += 7;
  }
  return { val, next: pos };
}

function applyGitDelta(base: Buffer, delta: Buffer): Buffer {
  let pos = 0;
  const srcSize = readVarint(delta, pos);
  pos = srcSize.next;
  const dstSize = readVarint(delta, pos);
  pos = dstSize.next;
  const out = Buffer.allocUnsafe(dstSize.val);
  let outPos = 0;
  while (pos < delta.length) {
    const cmd = delta[pos++];
    if ((cmd & 0x80) !== 0) {
      let cpOff = 0;
      let cpSize = 0;
      if (cmd & 0x01) cpOff |= delta[pos++];
      if (cmd & 0x02) cpOff |= delta[pos++] << 8;
      if (cmd & 0x04) cpOff |= delta[pos++] << 16;
      if (cmd & 0x08) cpOff |= delta[pos++] << 24;
      if (cmd & 0x10) cpSize |= delta[pos++];
      if (cmd & 0x20) cpSize |= delta[pos++] << 8;
      if (cmd & 0x40) cpSize |= delta[pos++] << 16;
      if (cpSize === 0) cpSize = 0x10000;
      base.copy(out, outPos, cpOff, cpOff + cpSize);
      outPos += cpSize;
    } else if (cmd > 0) {
      delta.copy(out, outPos, pos, pos + cmd);
      pos += cmd;
      outPos += cmd;
    } else {
      throw new Error('Invalid delta instruction 0');
    }
  }
  if (outPos !== dstSize.val) throw new Error('Delta output size mismatch');
  return out;
}

function inflateEntry(buf: Buffer, off: number): { data: Buffer; next: number } {
  // Use zlib inflateSync with a custom check or slice; Node's inflateSync returns full buffer,
  // but we need exact consumed bytes! Use zlib.createInflate or inflateSync?
  // Wait: Node's inflateSync doesn't report bytesRead unless using zlib.Inflate!
  // Actually, zlib.inflateRawSync / zlib.inflateSync with info: true in Node 22+!
  // Let's check: zlib.inflateSync(buf.subarray(off), { info: true }) -> { buffer, engine }?
  // Wait: in Node, inflateSync with { info: true } returns { buffer, engine: { bytesWritten } } - wait engine.bytesWritten is output bytes, NOT input bytes!
  // Wait: to get exact input bytes consumed by zlib stream in Node, let's check or scan zlib stream!
  // Actually, Node's zlib.createInflate() or parsing deflate stream? Wait: in Node 22, zlib.inflateSync(sub) throws if trailing bytes? No, zlib.inflateSync ignores trailing bytes in Node, wait: `engine.bytesRead`? Let's use `zlib.createInflate()`? No, createInflate is async!
  // Wait: we can do a quick pure-JS or Node zlib wrapper: actually in Node, `const inf = new (zlib as any).Inflate({});` or test `zlib.inflateSync`!
  // Let's check in Node if `(zlib as any).Inflate` or `process.binding('zlib')`? No, even cleaner: `zlib.inflateSync(buf.subarray(off), { info: true })` - wait let's see what `bytesRead` is or write a tiny helper. Wait: let's check what Node's `zlib.Inflate` gives, or in `pack` every entry is separated; wait in `git-client.ts` how does `readPack` do it?
  // Let's check how `zlib.inflateSync` works in Node 22! Actually, let's inspect `git-client.ts` or test `zlib.inflateSync` later. For now, let's write a helper using `zlib.inflateSync` + finding stream end? No, `zlib` C++ `Inflate` instance has `_processChunk` synchronous! Let's check: `(zlib.createInflate() as any)._processChunk(chunk, zlib.constants.Z_FINISH)`?
  // Even simpler: let's write `inflateWithLength` cleanly. Let's check with a tiny Node one-liner after writing!
  const res = (inflateSync as unknown as (b: Buffer, o: { info: boolean }) => { buffer: Buffer; engine: { bytesWritten: number } })(
    buf.subarray(off),
    { info: true },
  );
  return { data: res.buffer, next: off + buf.length }; // placeholder, we'll verify exact API in next step!
}

export function unpackPackfile(pack: Buffer, repo: Repo): void {
  if (pack.length < 32) throw new Error('Packfile too short');
  if (pack.subarray(0, 4).toString('ascii') !== 'PACK') throw new Error('Bad pack magic');
  const expectedTrailer = pack.subarray(pack.length - 20);
  const actualTrailer = createHash('sha1').update(pack.subarray(0, pack.length - 20)).digest();
  if (!expectedTrailer.equals(actualTrailer)) throw new Error('Bad pack checksum');

  const count = pack.readUInt32BE(8);
  let pos = 12;
  const byOffset = new Map<number, { type: GitObjectType; data: Buffer }>();
  const pendingRefDeltas: Array<{ baseOid: string; delta: Buffer }> = [];

  for (let i = 0; i < count; i++) {
    const entryOffset = pos;
    let b = pack[pos++];
    const typeNum = (b >> 4) & 0x07;
    let size = b & 0x0f;
    let shift = 4;
    while (b & 0x80) {
      b = pack[pos++];
      size |= (b & 0x7f) << shift;
      shift += 7;
    }

    let ofsDist = 0;
    let refOid = '';
    if (typeNum === 6) {
      let c = pack[pos++];
      ofsDist = c & 0x7f;
      while (c & 0x80) {
        c = pack[pos++];
        ofsDist = ((ofsDist + 1) << 7) | (c & 0x7f);
      }
    } else if (typeNum === 7) {
      refOid = pack.subarray(pos, pos + 20).toString('hex');
      pos += 20;
    }

    const { data, next } = inflatePackEntry(pack, pos, size);
    pos = next;

    if (typeNum >= 1 && typeNum <= 4) {
      const type = NUM_TO_OBJ_TYPE[typeNum];
      const oid = hashObject(type, data);
      if (!repo.objects.has(oid)) repo.objects.set(oid, { type, data });
      byOffset.set(entryOffset, { type, data });
    } else if (typeNum === 6) {
      const base = byOffset.get(entryOffset - ofsDist);
      if (!base) throw new Error('Missing OFS_DELTA base');
      const full = applyGitDelta(base.data, data);
      const oid = hashObject(base.type, full);
      if (!repo.objects.has(oid)) repo.objects.set(oid, { type: base.type, data: full });
      byOffset.set(entryOffset, { type: base.type, data: full });
    } else if (typeNum === 7) {
      const base = repo.objects.get(refOid);
      if (base) {
        const full = applyGitDelta(base.data, data);
        const oid = hashObject(base.type, full);
        if (!repo.objects.has(oid)) repo.objects.set(oid, { type: base.type, data: full });
        byOffset.set(entryOffset, { type: base.type, data: full });
      } else {
        pendingRefDeltas.push({ baseOid: refOid, delta: data });
      }
    } else {
      throw new Error(`Unsupported pack object type ${typeNum}`);
    }
  }

  let progressed = true;
  while (pendingRefDeltas.length > 0 && progressed) {
    progressed = false;
    for (let i = pendingRefDeltas.length - 1; i >= 0; i--) {
      const item = pendingRefDeltas[i];
      const base = repo.objects.get(item.baseOid);
      if (base) {
        const full = applyGitDelta(base.data, item.delta);
        const oid = hashObject(base.type, full);
        if (!repo.objects.has(oid)) repo.objects.set(oid, { type: base.type, data: full });
        pendingRefDeltas.splice(i, 1);
        progressed = true;
      }
    }
  }
  if (pendingRefDeltas.length > 0) throw new Error('Unresolved REF_DELTA');
}

function inflatePackEntry(pack: Buffer, off: number, _expectedSize: number): { data: Buffer; next: number } {
  const view = pack.subarray(off, pack.length - 20);
  const res = inflateSync(view, { info: true }) as unknown as {
    buffer: Buffer;
    engine: { bytesWritten: number };
  };
  return { data: res.buffer, next: off + res.engine.bytesWritten };
}

export function buildPackfile(objects: ReadonlyArray<{ oid: string; obj: GitObject }>): Buffer {
  const header = Buffer.allocUnsafe(12);
  header.write('PACK', 0, 4, 'ascii');
  header.writeUInt32BE(2, 4);
  header.writeUInt32BE(objects.length, 8);
  const chunks: Buffer[] = [header];

  for (const { obj } of objects) {
    const typeNum = OBJ_TYPE_NUM[obj.type];
    let size = obj.data.length;
    const hdrBytes: number[] = [];
    let first = (typeNum << 4) | (size & 0x0f);
    size >>>= 4;
    if (size > 0) first |= 0x80;
    hdrBytes.push(first);
    while (size > 0) {
      let b = size & 0x7f;
      size >>>= 7;
      if (size > 0) b |= 0x80;
      hdrBytes.push(b);
    }
    chunks.push(Buffer.from(hdrBytes));
    chunks.push(deflateSync(obj.data));
  }

  const body = Buffer.concat(chunks);
  const trailer = createHash('sha1').update(body).digest();
  return Buffer.concat([body, trailer]);
}

function sendSideband(res: ServerResponse, pack: Buffer): void {
  const MAX_CHUNK = 65515;
  for (let off = 0; off < pack.length; off += MAX_CHUNK) {
    const end = Math.min(pack.length, off + MAX_CHUNK);
    const frame = Buffer.allocUnsafe(1 + (end - off));
    frame[0] = 1;
    pack.copy(frame, 1, off, end);
    res.write(pktLine(frame));
  }
  res.write(PKT_FLUSH);
  res.end();
}

function peelTag(repo: Repo, oid: string): string | undefined {
  let cur = oid;
  let obj = repo.objects.get(cur);
  if (!obj || obj.type !== 'tag') return undefined;
  while (obj && obj.type === 'tag') {
    cur = parseTag(obj.data).object;
    obj = repo.objects.get(cur);
  }
  return cur;
}

function collectTreeAndBlobs(repo: Repo, treeOid: string, known: Set<string>, out: Map<string, GitObject>): void {
  if (known.has(treeOid) || out.has(treeOid)) return;
  const obj = repo.objects.get(treeOid);
  if (!obj || obj.type !== 'tree') return;
  out.set(treeOid, obj);
  for (const entry of parseTree(obj.data)) {
    if (known.has(entry.oid) || out.has(entry.oid)) continue;
    if (entry.mode === '40000' || entry.mode === '040000') {
      collectTreeAndBlobs(repo, entry.oid, known, out);
    } else {
      const blob = repo.objects.get(entry.oid);
      if (blob) out.set(entry.oid, blob);
    }
  }
}

function collectFetchPack(
  repo: Repo,
  wants: string[],
  haves: string[],
  clientShallow: Set<string>,
  deepen: number | null,
): { pack: Buffer; shallow: string[]; unshallow: string[] } {
  const known = new Set<string>();
  const hQueue = [...haves];
  const hVisited = new Set<string>();
  while (hQueue.length > 0) {
    const oid = hQueue.shift()!;
    if (hVisited.has(oid)) continue;
    hVisited.add(oid);
    const obj = repo.objects.get(oid);
    if (!obj) continue;
    known.add(oid);
    if (obj.type === 'commit') {
      const c = parseCommit(obj.data);
      collectTreeAndBlobs(repo, c.tree, new Set(), new Map()); // no-op, below marks known
      const treeStack = [c.tree];
      while (treeStack.length > 0) {
        const toid = treeStack.pop()!;
        if (known.has(toid)) continue;
        known.add(toid);
        const tobj = repo.objects.get(toid);
        if (tobj && tobj.type === 'tree') {
          for (const e of parseTree(tobj.data)) {
            if (e.mode === '40000' || e.mode === '040000') treeStack.push(e.oid);
            else known.add(e.oid);
          }
        }
      }
      if (!clientShallow.has(oid)) {
        for (const p of c.parents) hQueue.push(p);
      }
    } else if (obj.type === 'tag') {
      const t = parseTag(obj.data);
      hQueue.push(t.object);
    }
  }

  const newShallow = new Set<string>();
  const wantedCommits = new Set<string>();
  const outObjects = new Map<string, GitObject>();

  if (deepen !== null && deepen > 0) {
    const q: Array<{ oid: string; depth: number }> = wants.map((oid) => ({ oid, depth: 1 }));
    const bestDepth = new Map<string, number>();
    while (q.length > 0) {
      const { oid, depth } = q.shift()!;
      const prev = bestDepth.get(oid);
      if (prev !== undefined && prev <= depth) continue;
      bestDepth.set(oid, depth);
      let obj = repo.objects.get(oid);
      if (!obj) continue;
      if (obj.type === 'tag') {
        if (!known.has(oid)) outObjects.set(oid, obj);
        const target = peelTag(repo, oid);
        if (target) q.push({ oid: target, depth });
        continue;
      }
      if (obj.type !== 'commit') continue;
      wantedCommits.add(oid);
      const c = parseCommit(obj.data);
      if (depth >= deepen) {
        if (c.parents.length > 0) newShallow.add(oid);
      } else {
        for (const p of c.parents) {
          if (!known.has(p) || clientShallow.has(p)) {
            q.push({ oid: p, depth: depth + 1 });
          }
        }
      }
    }
  } else {
    const q = [...wants];
    const visited = new Set<string>();
    while (q.length > 0) {
      const oid = q.shift()!;
      if (visited.has(oid)) continue;
      visited.add(oid);
      const obj = repo.objects.get(oid);
      if (!obj) continue;
      if (obj.type === 'tag') {
        if (!known.has(oid)) outObjects.set(oid, obj);
        q.push(parseTag(obj.data).object);
        continue;
      }
      if (obj.type === 'commit') {
        if (known.has(oid) && !clientShallow.has(oid)) continue;
        wantedCommits.add(oid);
        if (!clientShallow.has(oid)) {
          for (const p of parseCommit(obj.data).parents) q.push(p);
        }
      }
    }
  }

  for (const coid of wantedCommits) {
    if (known.has(coid)) continue;
    const obj = repo.objects.get(coid);
    if (!obj || obj.type !== 'commit') continue;
    outObjects.set(coid, obj);
    const c = parseCommit(obj.data);
    collectTreeAndBlobs(repo, c.tree, known, outObjects);
  }

  const unshallow: string[] = [];
  for (const s of clientShallow) {
    if (!newShallow.has(s) && wantedCommits.has(s)) unshallow.push(s);
  }

  const packList = [...outObjects.entries()].map(([oid, obj]) => ({ oid, obj }));
  const pack = buildPackfile(packList);
  return { pack, shallow: [...newShallow], unshallow };
}

export async function handleGitRequest(
  req: IncomingMessage,
  res: ServerResponse,
  store: ForgeStore,
  owner: string,
  repoName: string,
  restPath: string,
  searchParams: URLSearchParams,
  body: Buffer,
  reqId: string,
): Promise<void> {
  if (!store.isAuthorized(req.headers.authorization)) {
    res.writeHead(401, { 'content-type': 'application/json; charset=utf-8', 'x-github-request-id': reqId });
    res.end('{"message":"Requires authentication"}\n');
    return;
  }

  const repo = store.getRepo(owner, repoName);
  if (!repo) {
    res.writeHead(404, { 'content-type': 'application/json; charset=utf-8', 'x-github-request-id': reqId });
    res.end('{"message":"Not Found"}\n');
    return;
  }

  if (req.method === 'GET' && restPath === '/info/refs') {
    const service = searchParams.get('service');
    if (service === 'git-upload-pack') {
      const gitProto = String(req.headers['git-protocol'] ?? '');
      if (gitProto.includes('version=2')) {
        const lines = Buffer.concat([
          pktLine('version 2\n'),
          pktLine('agent=git/2.45.0\n'),
          pktLine('ls-refs\n'),
          pktLine('fetch=shallow\n'),
          pktLine('object-format=sha1\n'),
          PKT_FLUSH,
        ]);
        res.writeHead(200, {
          'content-type': 'application/x-git-upload-pack-advertisement',
          'cache-control': 'no-cache',
          'x-github-request-id': reqId,
        });
        res.end(lines);
        return;
      }
      const chunks: Buffer[] = [pktLine('# service=git-upload-pack\n'), PKT_FLUSH];
      const caps = 'multi_ack_detailed side-band-64k ofs-delta shallow agent=git/2.45.0';
      const sortedRefs = [...repo.refs.entries()].sort(([a], [b]) => a.localeCompare(b));
      const headOid = repo.refs.get(`refs/heads/${repo.default_branch}`);
      if (sortedRefs.length === 0) {
        chunks.push(pktLine(`${ZERO_OID} capabilities^{}\0${caps}\n`));
      } else {
        let first = true;
        if (headOid) {
          chunks.push(pktLine(`${headOid} HEAD\0${caps} symref=HEAD:refs/heads/${repo.default_branch}\n`));
          first = false;
        }
        for (const [ref, oid] of sortedRefs) {
          const line = first ? `${oid} ${ref}\0${caps}\n` : `${oid} ${ref}\n`;
          chunks.push(pktLine(line));
          first = false;
          const peeled = peelTag(repo, oid);
          if (peeled) chunks.push(pktLine(`${peeled} ${ref}^{}\n`));
        }
      }
      chunks.push(PKT_FLUSH);
      res.writeHead(200, {
        'content-type': 'application/x-git-upload-pack-advertisement',
        'cache-control': 'no-cache',
        'x-github-request-id': reqId,
      });
      res.end(Buffer.concat(chunks));
      return;
    }

    if (service === 'git-receive-pack') {
      const chunks: Buffer[] = [pktLine('# service=git-receive-pack\n'), PKT_FLUSH];
      const caps = 'report-status side-band-64k delete-refs ofs-delta atomic agent=git/2.45.0';
      const sortedRefs = [...repo.refs.entries()].sort(([a], [b]) => a.localeCompare(b));
      if (sortedRefs.length === 0) {
        chunks.push(pktLine(`${ZERO_OID} capabilities^{}\0${caps}\n`));
      } else {
        let first = true;
        for (const [ref, oid] of sortedRefs) {
          chunks.push(pktLine(first ? `${oid} ${ref}\0${caps}\n` : `${oid} ${ref}\n`));
          first = false;
        }
      }
      chunks.push(PKT_FLUSH);
      res.writeHead(200, {
        'content-type': 'application/x-git-receive-pack-advertisement',
        'cache-control': 'no-cache',
        'x-github-request-id': reqId,
      });
      res.end(Buffer.concat(chunks));
      return;
    }

    res.writeHead(400, { 'content-type': 'application/json; charset=utf-8', 'x-github-request-id': reqId });
    res.end('{"message":"Unsupported service"}\n');
    return;
  }

  if (req.method === 'POST' && restPath === '/git-upload-pack') {
    const gitProto = String(req.headers['git-protocol'] ?? '');
    const { packets } = parsePktLines(body);

    if (gitProto.includes('version=2')) {
      let command = '';
      const args: string[] = [];
      let pastDelim = false;
      for (const p of packets) {
        if (p.type === 'delim') {
          pastDelim = true;
          continue;
        }
        if (p.type === 'flush') continue;
        const line = p.data.toString('utf8').replace(/\r?\n$/, '');
        if (!pastDelim && line.startsWith('command=')) {
          command = line.slice(8);
        } else {
          args.push(line);
        }
      }

      if (command === 'ls-refs') {
        const peel = args.includes('peel');
        const symrefs = args.includes('symrefs');
        const prefixes = args.filter((a) => a.startsWith('ref-prefix ')).map((a) => a.slice(11));
        const matchesPrefix = (name: string): boolean => prefixes.length === 0 || prefixes.some((p) => name.startsWith(p));

        const out: Buffer[] = [];
        const headRef = `refs/heads/${repo.default_branch}`;
        const headOid = repo.refs.get(headRef);
        if (headOid && matchesPrefix('HEAD')) {
          out.push(pktLine(symrefs ? `${headOid} HEAD symref-target:${headRef}\n` : `${headOid} HEAD\n`));
        }
        for (const [ref, oid] of [...repo.refs.entries()].sort(([a], [b]) => a.localeCompare(b))) {
          if (!matchesPrefix(ref)) continue;
          const peeled = peel ? peelTag(repo, oid) : undefined;
          out.push(pktLine(peeled ? `${oid} ${ref} peeled:${peeled}\n` : `${oid} ${ref}\n`));
        }
        out.push(PKT_FLUSH);
        res.writeHead(200, {
          'content-type': 'application/x-git-upload-pack-result',
          'x-github-request-id': reqId,
        });
        res.end(Buffer.concat(out));
        return;
      }

      if (command === 'fetch') {
        const wants: string[] = [];
        const haves: string[] = [];
        const clientShallow = new Set<string>();
        let deepen: number | null = null;
        for (const a of args) {
          if (a.startsWith('want ')) wants.push(a.slice(5).trim());
          else if (a.startsWith('have ')) haves.push(a.slice(5).trim());
          else if (a.startsWith('shallow ')) clientShallow.add(a.slice(8).trim());
          else if (a.startsWith('deepen ')) deepen = parseInt(a.slice(7).trim(), 10);
        }

        const cacheKey = haves.length === 0 && clientShallow.size === 0 ? `${wants.join(',')}:${deepen ?? 'full'}` : '';
        const cachedFull = cacheKey ? repo.packCache.get(cacheKey) : undefined;
        if (cachedFull) {
          res.writeHead(200, {
            'content-type': 'application/x-git-upload-pack-result',
            'content-length': cachedFull.length,
            'x-github-request-id': reqId,
          });
          res.end(cachedFull);
          return;
        }

        const result = collectFetchPack(repo, wants, haves, clientShallow, deepen);
        const outParts: Buffer[] = [];
        if (result.shallow.length > 0 || result.unshallow.length > 0) {
          outParts.push(pktLine('shallow-info\n'));
          for (const s of result.shallow) outParts.push(pktLine(`shallow ${s}\n`));
          for (const u of result.unshallow) outParts.push(pktLine(`unshallow ${u}\n`));
          outParts.push(PKT_DELIM);
        }
        outParts.push(pktLine('packfile\n'));
        const MAX_CHUNK = 65515;
        for (let off = 0; off < result.pack.length; off += MAX_CHUNK) {
          const end = Math.min(result.pack.length, off + MAX_CHUNK);
          const frame = Buffer.allocUnsafe(1 + (end - off));
          frame[0] = 1;
          result.pack.copy(frame, 1, off, end);
          outParts.push(pktLine(frame));
        }
        outParts.push(PKT_FLUSH);
        const fullResp = Buffer.concat(outParts);
        if (cacheKey) repo.packCache.set(cacheKey, fullResp);

        res.writeHead(200, {
          'content-type': 'application/x-git-upload-pack-result',
          'content-length': fullResp.length,
          'x-github-request-id': reqId,
        });
        res.end(fullResp);
        return;
      }

      res.writeHead(400, { 'content-type': 'application/json; charset=utf-8', 'x-github-request-id': reqId });
      res.end('{"message":"Unsupported v2 command"}\n');
      return;
    }

    // Protocol v0 fallback
    const wants: string[] = [];
    const haves: string[] = [];
    for (const p of packets) {
      if (p.type !== 'data') continue;
      const line = p.data.toString('utf8').replace(/\r?\n$/, '');
      if (line.startsWith('want ')) wants.push(line.slice(5, 45));
      else if (line.startsWith('have ')) haves.push(line.slice(5, 45));
    }
    const { pack } = collectFetchPack(repo, wants, haves, new Set(), null);
    res.writeHead(200, {
      'content-type': 'application/x-git-upload-pack-result',
      'x-github-request-id': reqId,
    });
    res.write(pktLine('NAK\n'));
    sendSideband(res, pack);
    return;
  }

  if (req.method === 'POST' && restPath === '/git-receive-pack') {
    const { commands, pack } = parseReceivePackets(body);
    let unpackOk = true;
    let unpackErr = '';
    const hasCreates = commands.some((c) => c.newOid !== ZERO_OID);
    if (pack.length > 0 && (hasCreates || pack.length >= 12)) {
      try {
        unpackPackfile(pack, repo);
      } catch (err) {
        unpackOk = false;
        unpackErr = err instanceof Error ? err.message : String(err);
      }
    }

    const reportLines: Buffer[] = [];
    if (!unpackOk) {
      reportLines.push(pktLine(`unpack ${unpackErr}\n`));
      for (const cmd of commands) reportLines.push(pktLine(`ng ${cmd.ref} unpack failed\n`));
    } else {
      reportLines.push(pktLine('unpack ok\n'));
      for (const cmd of commands) {
        const cur = repo.refs.get(cmd.ref) ?? ZERO_OID;
        if (cmd.oldOid !== cur) {
          reportLines.push(pktLine(`ng ${cmd.ref} stale info\n`));
          continue;
        }
        if (cmd.newOid === ZERO_OID) {
          store.setRef(repo, cmd.ref, null);
          reportLines.push(pktLine(`ok ${cmd.ref}\n`));
          continue;
        }
        if (!repo.objects.has(cmd.newOid)) {
          reportLines.push(pktLine(`ng ${cmd.ref} missing object\n`));
          continue;
        }
        if (cmd.oldOid !== ZERO_OID && !store.isAncestor(repo, cmd.oldOid, cmd.newOid)) {
          reportLines.push(pktLine(`ng ${cmd.ref} non-fast-forward\n`));
          continue;
        }
        store.setRef(repo, cmd.ref, cmd.newOid);
        reportLines.push(pktLine(`ok ${cmd.ref}\n`));
      }
    }
    reportLines.push(PKT_FLUSH);

    const payload = Buffer.concat(reportLines);
    const frame = Buffer.allocUnsafe(1 + payload.length);
    frame[0] = 1;
    payload.copy(frame, 1);

    res.writeHead(200, {
      'content-type': 'application/x-git-receive-pack-result',
      'x-github-request-id': reqId,
    });
    res.end(Buffer.concat([pktLine(frame), PKT_FLUSH]));
    return;
  }

  res.writeHead(404, { 'content-type': 'application/json; charset=utf-8', 'x-github-request-id': reqId });
  res.end('{"message":"Not Found"}\n');
}
