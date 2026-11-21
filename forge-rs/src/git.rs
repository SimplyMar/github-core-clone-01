use crate::store::{
    hex_to_oid, oid_to_hex, parse_commit, parse_tag, parse_tree, ForgeStore, GitObject,
    GitObjectKind, Oid, Repo, ZERO_OID, ZERO_OID_HEX,
};
use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};
use rustc_hash::{FxHashMap, FxHashSet};
use sha1::{Digest, Sha1};
use std::collections::VecDeque;
use std::sync::{Arc, RwLock};

pub const UPLOAD_PACK_V2_HTTP: &[u8] = concat!(
    "HTTP/1.1 200 OK\r\n",
    "content-type: application/x-git-upload-pack-advertisement\r\n",
    "cache-control: no-cache\r\n",
    "x-github-request-id: git-v2\r\n",
    "content-length: 92\r\n",
    "connection: keep-alive\r\n\r\n",
    "000eversion 2\n0015agent=git/2.45.0\n000cls-refs\n0012fetch=shallow\n0017object-format=sha1\n0000"
)
.as_bytes();

#[inline]
fn push_pkt_line(out: &mut Vec<u8>, payload: &[u8]) {
    let total = payload.len() + 4;
    let hex = b"0123456789abcdef";
    out.push(hex[(total >> 12) & 0xf]);
    out.push(hex[(total >> 8) & 0xf]);
    out.push(hex[(total >> 4) & 0xf]);
    out.push(hex[total & 0xf]);
    out.extend_from_slice(payload);
}

#[inline]
fn push_http_200(out: &mut Vec<u8>, content_type: &str, extra_headers: &str, body: &[u8]) {
    out.extend_from_slice(b"HTTP/1.1 200 OK\r\ncontent-type: ");
    out.extend_from_slice(content_type.as_bytes());
    out.extend_from_slice(b"\r\n");
    if !extra_headers.is_empty() {
        out.extend_from_slice(extra_headers.as_bytes());
    }
    out.extend_from_slice(b"x-github-request-id: git-1\r\ncontent-length: ");
    out.extend_from_slice(body.len().to_string().as_bytes());
    out.extend_from_slice(b"\r\nconnection: keep-alive\r\n\r\n");
    out.extend_from_slice(body);
}

#[inline]
fn push_http_err(out: &mut Vec<u8>, status: &str, msg: &str) {
    let body = format!("{{\"message\":\"{}\"}}\n", msg);
    out.extend_from_slice(b"HTTP/1.1 ");
    out.extend_from_slice(status.as_bytes());
    out.extend_from_slice(b"\r\ncontent-type: application/json; charset=utf-8\r\nx-github-request-id: git-err\r\ncontent-length: ");
    out.extend_from_slice(body.len().to_string().as_bytes());
    out.extend_from_slice(b"\r\nconnection: keep-alive\r\n\r\n");
    out.extend_from_slice(body.as_bytes());
}

enum Pkt<'a> {
    Flush,
    Delim,
    Data(&'a [u8]),
}

fn parse_pkt_lines<'a>(buf: &'a [u8]) -> Vec<Pkt<'a>> {
    let mut out = Vec::with_capacity(16);
    let mut off = 0;
    while off + 4 <= buf.len() {
        let hdr = &buf[off..off + 4];
        if hdr == b"0000" {
            out.push(Pkt::Flush);
            off += 4;
            continue;
        }
        if hdr == b"0001" {
            out.push(Pkt::Delim);
            off += 4;
            continue;
        }
        if hdr == b"0002" {
            off += 4;
            continue;
        }
        let len = match parse_hex4(hdr) {
            Some(l) if l >= 4 && off + l <= buf.len() => l,
            _ => break,
        };
        out.push(Pkt::Data(&buf[off + 4..off + len]));
        off += len;
    }
    out
}

#[inline]
fn parse_hex4(b: &[u8]) -> Option<usize> {
    let mut v = 0usize;
    for &c in b {
        let d = match c {
            b'0'..=b'9' => (c - b'0') as usize,
            b'a'..=b'f' => (c - b'a' + 10) as usize,
            b'A'..=b'F' => (c - b'A' + 10) as usize,
            _ => return None,
        };
        v = (v << 4) | d;
    }
    Some(v)
}

struct ReceiveCmd<'a> {
    old_oid: Oid,
    new_oid: Oid,
    ref_name: &'a str,
}

fn parse_receive_packets<'a>(buf: &'a [u8]) -> (Vec<ReceiveCmd<'a>>, &'a [u8]) {
    let mut cmds = Vec::with_capacity(4);
    let mut off = 0;
    while off + 4 <= buf.len() {
        let hdr = &buf[off..off + 4];
        if hdr == b"0000" {
            off += 4;
            break;
        }
        let len = match parse_hex4(hdr) {
            Some(l) if l >= 4 && off + l <= buf.len() => l,
            _ => break,
        };
        let mut line = &buf[off + 4..off + len];
        off += len;
        if let Some(nul) = line.iter().position(|&b| b == 0) {
            line = &line[..nul];
        }
        while line.ends_with(b"\n") || line.ends_with(b"\r") {
            line = &line[..line.len() - 1];
        }
        if line.len() >= 83 && line[40] == b' ' && line[81] == b' ' {
            if let (Some(old_oid), Some(new_oid)) = (hex_to_oid(&line[..40]), hex_to_oid(&line[41..81])) {
                if let Ok(ref_name) = std::str::from_utf8(&line[82..]) {
                    cmds.push(ReceiveCmd {
                        old_oid,
                        new_oid,
                        ref_name,
                    });
                }
            }
        }
    }
    (cmds, &buf[off..])
}

#[inline]
fn read_varint(buf: &[u8], mut pos: usize) -> Result<(usize, usize), &'static str> {
    let mut val = 0usize;
    let mut shift = 0u32;
    while pos < buf.len() {
        let b = buf[pos];
        pos += 1;
        val |= ((b & 0x7f) as usize) << shift;
        if (b & 0x80) == 0 {
            return Ok((val, pos));
        }
        shift += 7;
    }
    Err("truncated varint")
}

fn apply_git_delta(base: &[u8], delta: &[u8]) -> Result<Vec<u8>, &'static str> {
    let (_src_size, pos) = read_varint(delta, 0)?;
    let (dst_size, mut pos) = read_varint(delta, pos)?;
    let mut out = Vec::with_capacity(dst_size);
    while pos < delta.len() {
        let cmd = delta[pos];
        pos += 1;
        if (cmd & 0x80) != 0 {
            let mut cp_off = 0usize;
            let mut cp_size = 0usize;
            if (cmd & 0x01) != 0 { cp_off |= delta[pos] as usize; pos += 1; }
            if (cmd & 0x02) != 0 { cp_off |= (delta[pos] as usize) << 8; pos += 1; }
            if (cmd & 0x04) != 0 { cp_off |= (delta[pos] as usize) << 16; pos += 1; }
            if (cmd & 0x08) != 0 { cp_off |= (delta[pos] as usize) << 24; pos += 1; }
            if (cmd & 0x10) != 0 { cp_size |= delta[pos] as usize; pos += 1; }
            if (cmd & 0x20) != 0 { cp_size |= (delta[pos] as usize) << 8; pos += 1; }
            if (cmd & 0x40) != 0 { cp_size |= (delta[pos] as usize) << 16; pos += 1; }
            if cp_size == 0 {
                cp_size = 0x10000;
            }
            if cp_off + cp_size > base.len() {
                return Err("delta copy out of bounds");
            }
            out.extend_from_slice(&base[cp_off..cp_off + cp_size]);
        } else if cmd > 0 {
            let n = cmd as usize;
            if pos + n > delta.len() {
                return Err("delta literal out of bounds");
            }
            out.extend_from_slice(&delta[pos..pos + n]);
            pos += n;
        } else {
            return Err("invalid delta opcode 0");
        }
    }
    if out.len() != dst_size {
        return Err("delta size mismatch");
    }
    Ok(out)
}

fn inflate_pack_entry(
    decomp: &mut Decompress,
    pack: &[u8],
    off: usize,
    expected_size: usize,
) -> Result<(Vec<u8>, usize), &'static str> {
    let limit = pack.len() - 20;
    if off >= limit {
        return Err("truncated pack entry");
    }
    let slice = &pack[off..limit];
    decomp.reset(true);
    let mut out = Vec::with_capacity(expected_size.max(64));
    let mut tmp = [0u8; 8192];
    loop {
        let before_in = decomp.total_in() as usize;
        let before_out = decomp.total_out() as usize;
        let status = decomp
            .decompress(&slice[before_in..], &mut tmp, FlushDecompress::None)
            .map_err(|_| "zlib inflate error")?;
        let produced = (decomp.total_out() as usize) - before_out;
        if produced > 0 {
            out.extend_from_slice(&tmp[..produced]);
        }
        match status {
            Status::StreamEnd => {
                let consumed = decomp.total_in() as usize;
                return Ok((out, off + consumed));
            }
            Status::Ok | Status::BufError => {
                if produced == 0 && (decomp.total_in() as usize) == before_in {
                    return Err("zlib stream stalled");
                }
            }
        }
    }
}

pub fn unpack_packfile(pack: &[u8], repo: &mut Repo) -> Result<(), &'static str> {
    if pack.len() < 32 {
        return Err("Packfile too short");
    }
    if &pack[0..4] != b"PACK" {
        return Err("Bad pack magic");
    }
    let expected_trailer = &pack[pack.len() - 20..];
    let actual_trailer = Sha1::digest(&pack[..pack.len() - 20]);
    if actual_trailer.as_slice() != expected_trailer {
        return Err("Bad pack checksum");
    }

    let count = u32::from_be_bytes([pack[8], pack[9], pack[10], pack[11]]) as usize;
    let mut pos = 12usize;
    let mut offset_oids: Vec<(usize, Oid)> = Vec::with_capacity(count);
    let mut pending_ref_deltas: Vec<(Oid, Vec<u8>)> = Vec::new();
    let mut decomp = Decompress::new(true);

    for _ in 0..count {
        if pos >= pack.len() - 20 {
            return Err("Unexpected end of packfile");
        }
        let entry_offset = pos;
        let mut b = pack[pos];
        pos += 1;
        let type_num = (b >> 4) & 0x07;
        let mut size = (b & 0x0f) as usize;
        let mut shift = 4u32;
        while (b & 0x80) != 0 {
            if pos >= pack.len() - 20 {
                return Err("Truncated object header");
            }
            b = pack[pos];
            pos += 1;
            size |= ((b & 0x7f) as usize) << shift;
            shift += 7;
        }

        let mut ofs_dist = 0usize;
        let mut ref_oid = ZERO_OID;
        if type_num == 6 {
            let mut c = pack[pos];
            pos += 1;
            ofs_dist = (c & 0x7f) as usize;
            while (c & 0x80) != 0 {
                c = pack[pos];
                pos += 1;
                ofs_dist = ((ofs_dist + 1) << 7) | ((c & 0x7f) as usize);
            }
        } else if type_num == 7 {
            if pos + 20 > pack.len() - 20 {
                return Err("Truncated REF_DELTA oid");
            }
            ref_oid.copy_from_slice(&pack[pos..pos + 20]);
            pos += 20;
        }

        let zlib_start = pos;
        let (data, next_pos) = inflate_pack_entry(&mut decomp, pack, pos, size)?;
        pos = next_pos;

        if let Some(kind) = GitObjectKind::from_num(type_num) {
            let raw_zlib = Some(pack[zlib_start..next_pos].to_vec());
            let oid = ForgeStore::put_object(repo, kind, data, raw_zlib);
            offset_oids.push((entry_offset, oid));
        } else if type_num == 6 {
            let base_off = entry_offset.checked_sub(ofs_dist).ok_or("Invalid OFS_DELTA offset")?;
            let base_oid = offset_oids
                .binary_search_by_key(&base_off, |&(off, _)| off)
                .map(|i| offset_oids[i].1)
                .map_err(|_| "Missing OFS_DELTA base")?;
            let (kind, full) = {
                let base = repo.objects.get(&base_oid).ok_or("Missing OFS_DELTA base")?;
                (base.kind, apply_git_delta(&base.data, &data)?)
            };
            let oid = ForgeStore::put_object(repo, kind, full, None);
            offset_oids.push((entry_offset, oid));
        } else if type_num == 7 {
            if let Some((kind, full)) = repo.objects.get(&ref_oid).map(|b| (b.kind, apply_git_delta(&b.data, &data))) {
                let full = full?;
                let oid = ForgeStore::put_object(repo, kind, full, None);
                offset_oids.push((entry_offset, oid));
            } else {
                pending_ref_deltas.push((ref_oid, data));
            }
        } else {
            return Err("Unknown pack object type");
        }
    }

    let mut progressed = true;
    while !pending_ref_deltas.is_empty() && progressed {
        progressed = false;
        let mut i = pending_ref_deltas.len();
        while i > 0 {
            i -= 1;
            let base_oid = pending_ref_deltas[i].0;
            if let Some(base) = repo.objects.get(&base_oid) {
                let kind = base.kind;
                let full = apply_git_delta(&base.data, &pending_ref_deltas[i].1)?;
                ForgeStore::put_object(repo, kind, full, None);
                pending_ref_deltas.swap_remove(i);
                progressed = true;
            }
        }
    }
    if !pending_ref_deltas.is_empty() {
        return Err("Unresolved REF_DELTA");
    }
    Ok(())
}

fn deflate_bytes(data: &[u8]) -> Vec<u8> {
    let mut comp = Compress::new(Compression::fast(), true);
    let mut out = Vec::with_capacity(data.len() + 32);
    let mut tmp = [0u8; 8192];
    loop {
        let before_in = comp.total_in() as usize;
        let before_out = comp.total_out() as usize;
        let status = comp
            .compress(&data[before_in..], &mut tmp, FlushCompress::Finish)
            .unwrap();
        let produced = (comp.total_out() as usize) - before_out;
        if produced > 0 {
            out.extend_from_slice(&tmp[..produced]);
        }
        if status == Status::StreamEnd {
            break;
        }
    }
    out
}

pub fn build_packfile(objects: &[&GitObject]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(12 + objects.len() * 128 + 20);
    buf.extend_from_slice(b"PACK");
    buf.extend_from_slice(&2u32.to_be_bytes());
    buf.extend_from_slice(&(objects.len() as u32).to_be_bytes());

    for obj in objects {
        let type_num = obj.kind as u8;
        let mut size = obj.data.len();
        let mut first = (type_num << 4) | ((size & 0x0f) as u8);
        size >>= 4;
        if size > 0 {
            first |= 0x80;
        }
        buf.push(first);
        while size > 0 {
            let mut b = (size & 0x7f) as u8;
            size >>= 7;
            if size > 0 {
                b |= 0x80;
            }
            buf.push(b);
        }
        if let Some(ref z) = obj.raw_zlib {
            buf.extend_from_slice(z);
        } else {
            let z = deflate_bytes(&obj.data);
            buf.extend_from_slice(&z);
        }
    }

    let trailer = Sha1::digest(&buf);
    buf.extend_from_slice(&trailer);
    buf
}

fn push_sideband_pack(out: &mut Vec<u8>, pack: &[u8]) {
    const MAX_CHUNK: usize = 65515;
    let mut off = 0;
    let hex = b"0123456789abcdef";
    while off < pack.len() {
        let end = (off + MAX_CHUNK).min(pack.len());
        let chunk = &pack[off..end];
        let total = 4 + 1 + chunk.len();
        out.push(hex[(total >> 12) & 0xf]);
        out.push(hex[(total >> 8) & 0xf]);
        out.push(hex[(total >> 4) & 0xf]);
        out.push(hex[total & 0xf]);
        out.push(1u8);
        out.extend_from_slice(chunk);
        off = end;
    }
    out.extend_from_slice(b"0000");
}

fn peel_tag(repo: &Repo, oid: Oid) -> Option<Oid> {
    let mut cur = oid;
    let mut obj = repo.objects.get(&cur)?;
    if obj.kind != GitObjectKind::Tag {
        return None;
    }
    while obj.kind == GitObjectKind::Tag {
        cur = parse_tag(&obj.data).object;
        match repo.objects.get(&cur) {
            Some(o) => obj = o,
            None => break,
        }
    }
    Some(cur)
}

fn collect_tree_and_blobs(
    repo: &Repo,
    tree_oid: Oid,
    known: &FxHashSet<Oid>,
    out_order: &mut Vec<Oid>,
    out_seen: &mut FxHashSet<Oid>,
) {
    if known.contains(&tree_oid) || !out_seen.insert(tree_oid) {
        return;
    }
    let obj = match repo.objects.get(&tree_oid) {
        Some(o) if o.kind == GitObjectKind::Tree => o,
        _ => return,
    };
    out_order.push(tree_oid);
    for entry in parse_tree(&obj.data) {
        if known.contains(&entry.oid) || out_seen.contains(&entry.oid) {
            continue;
        }
        if entry.mode == "40000" || entry.mode == "040000" {
            collect_tree_and_blobs(repo, entry.oid, known, out_order, out_seen);
        } else if repo.objects.contains_key(&entry.oid) {
            if out_seen.insert(entry.oid) {
                out_order.push(entry.oid);
            }
        }
    }
}

fn collect_fetch_pack(
    repo: &Repo,
    wants: &[Oid],
    haves: &[Oid],
    client_shallow: &FxHashSet<Oid>,
    deepen: Option<usize>,
) -> (Vec<u8>, Vec<Oid>, Vec<Oid>) {
    let mut known: FxHashSet<Oid> = FxHashSet::default();
    let mut h_queue: VecDeque<Oid> = haves.iter().copied().collect();
    let mut h_visited: FxHashSet<Oid> = FxHashSet::default();

    while let Some(oid) = h_queue.pop_front() {
        if !h_visited.insert(oid) {
            continue;
        }
        let obj = match repo.objects.get(&oid) {
            Some(o) => o,
            None => continue,
        };
        known.insert(oid);
        if obj.kind == GitObjectKind::Commit {
            let (tree, parents) = if let Some(ref m) = obj.commit_meta {
                (m.tree, m.parents.clone())
            } else {
                let c = parse_commit(&obj.data);
                (c.tree, c.parents)
            };
            let mut tree_stack = vec![tree];
            while let Some(toid) = tree_stack.pop() {
                if !known.insert(toid) {
                    continue;
                }
                if let Some(tobj) = repo.objects.get(&toid) {
                    if tobj.kind == GitObjectKind::Tree {
                        for e in parse_tree(&tobj.data) {
                            if e.mode == "40000" || e.mode == "040000" {
                                tree_stack.push(e.oid);
                            } else {
                                known.insert(e.oid);
                            }
                        }
                    }
                }
            }
            if !client_shallow.contains(&oid) {
                for p in parents {
                    h_queue.push_back(p);
                }
            }
        } else if obj.kind == GitObjectKind::Tag {
            h_queue.push_back(parse_tag(&obj.data).object);
        }
    }

    let mut new_shallow: Vec<Oid> = Vec::new();
    let mut new_shallow_set: FxHashSet<Oid> = FxHashSet::default();
    let mut wanted_commits: Vec<Oid> = Vec::new();
    let mut wanted_commit_set: FxHashSet<Oid> = FxHashSet::default();
    let mut out_order: Vec<Oid> = Vec::new();
    let mut out_seen: FxHashSet<Oid> = FxHashSet::default();

    if let Some(d_limit) = deepen.filter(|&d| d > 0) {
        let mut q: VecDeque<(Oid, usize)> = wants.iter().map(|&o| (o, 1)).collect();
        let mut best_depth: FxHashMap<Oid, usize> = FxHashMap::default();
        while let Some((oid, depth)) = q.pop_front() {
            if let Some(&prev) = best_depth.get(&oid) {
                if prev <= depth {
                    continue;
                }
            }
            best_depth.insert(oid, depth);
            let obj = match repo.objects.get(&oid) {
                Some(o) => o,
                None => continue,
            };
            if obj.kind == GitObjectKind::Tag {
                if !known.contains(&oid) && out_seen.insert(oid) {
                    out_order.push(oid);
                }
                if let Some(target) = peel_tag(repo, oid) {
                    q.push_back((target, depth));
                }
                continue;
            }
            if obj.kind != GitObjectKind::Commit {
                continue;
            }
            if wanted_commit_set.insert(oid) {
                wanted_commits.push(oid);
            }
            let parents: &[Oid] = match &obj.commit_meta {
                Some(m) => &m.parents,
                None => &[],
            };
            if depth >= d_limit {
                if !parents.is_empty() && new_shallow_set.insert(oid) {
                    new_shallow.push(oid);
                }
            } else {
                for &p in parents {
                    if !known.contains(&p) || client_shallow.contains(&p) {
                        q.push_back((p, depth + 1));
                    }
                }
            }
        }
    } else {
        let mut q: VecDeque<Oid> = wants.iter().copied().collect();
        let mut visited: FxHashSet<Oid> = FxHashSet::default();
        while let Some(oid) = q.pop_front() {
            if !visited.insert(oid) {
                continue;
            }
            let obj = match repo.objects.get(&oid) {
                Some(o) => o,
                None => continue,
            };
            if obj.kind == GitObjectKind::Tag {
                if !known.contains(&oid) && out_seen.insert(oid) {
                    out_order.push(oid);
                }
                q.push_back(parse_tag(&obj.data).object);
                continue;
            }
            if obj.kind == GitObjectKind::Commit {
                if known.contains(&oid) && !client_shallow.contains(&oid) {
                    continue;
                }
                if wanted_commit_set.insert(oid) {
                    wanted_commits.push(oid);
                }
                if !client_shallow.contains(&oid) {
                    if let Some(ref m) = obj.commit_meta {
                        for &p in &m.parents {
                            q.push_back(p);
                        }
                    }
                }
            }
        }
    }

    for coid in &wanted_commits {
        if known.contains(coid) {
            continue;
        }
        let obj = match repo.objects.get(coid) {
            Some(o) if o.kind == GitObjectKind::Commit => o,
            _ => continue,
        };
        if out_seen.insert(*coid) {
            out_order.push(*coid);
        }
        if let Some(ref m) = obj.commit_meta {
            collect_tree_and_blobs(repo, m.tree, &known, &mut out_order, &mut out_seen);
        }
    }

    let mut unshallow = Vec::new();
    for &s in client_shallow {
        if !new_shallow_set.contains(&s) && wanted_commit_set.contains(&s) {
            unshallow.push(s);
        }
    }

    let objs: Vec<&GitObject> = out_order
        .iter()
        .filter_map(|o| repo.objects.get(o))
        .collect();
    let pack = build_packfile(&objs);
    (pack, new_shallow, unshallow)
}

fn build_receive_refs_http(repo: &Repo) -> Vec<u8> {
    let mut body = Vec::with_capacity(256);
    push_pkt_line(&mut body, b"# service=git-receive-pack\n");
    body.extend_from_slice(b"0000");
    let caps = "report-status side-band-64k delete-refs ofs-delta atomic agent=git/2.45.0";
    let mut sorted: Vec<(&String, &Oid)> = repo.refs.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    if sorted.is_empty() {
        let line = format!("{} capabilities^{{}}\0{}\n", ZERO_OID_HEX, caps);
        push_pkt_line(&mut body, line.as_bytes());
    } else {
        for (i, (ref_name, oid)) in sorted.into_iter().enumerate() {
            let line = if i == 0 {
                format!("{} {}\0{}\n", oid_to_hex(oid), ref_name, caps)
            } else {
                format!("{} {}\n", oid_to_hex(oid), ref_name)
            };
            push_pkt_line(&mut body, line.as_bytes());
        }
    }
    body.extend_from_slice(b"0000");
    let mut resp = Vec::with_capacity(body.len() + 192);
    push_http_200(
        &mut resp,
        "application/x-git-receive-pack-advertisement",
        "cache-control: no-cache\r\n",
        &body,
    );
    resp
}

pub fn handle_git_request(
    store_lock: &RwLock<ForgeStore>,
    method: &str,
    owner: &str,
    repo_name: &str,
    rest_path: &str,
    query: &str,
    auth: Option<&[u8]>,
    git_proto: Option<&[u8]>,
    body: &[u8],
    resp_out: &mut Vec<u8>,
) -> Option<Arc<Vec<u8>>> {
    let is_v2 = git_proto
        .map(|g| g.windows(9).any(|w| w == b"version=2"))
        .unwrap_or(false);

    if method == "GET" && rest_path == "/info/refs" {
        let service = query_param(query, "service").unwrap_or("");
        let store = store_lock.read().unwrap();
        if !store.is_authorized(auth) {
            push_http_err(resp_out, "401 Unauthorized", "Requires authentication");
            return None;
        }
        let repo = match store.get_repo(owner, repo_name) {
            Some(r) => r,
            None => {
                push_http_err(resp_out, "404 Not Found", "Not Found");
                return None;
            }
        };
        if service == "git-upload-pack" {
            if is_v2 {
                resp_out.extend_from_slice(UPLOAD_PACK_V2_HTTP);
                return None;
            }
            let mut pkt_body = Vec::with_capacity(256);
            push_pkt_line(&mut pkt_body, b"# service=git-upload-pack\n");
            pkt_body.extend_from_slice(b"0000");
            let caps = "multi_ack_detailed side-band-64k ofs-delta shallow agent=git/2.45.0";
            let mut sorted: Vec<(&String, &Oid)> = repo.refs.iter().collect();
            sorted.sort_by(|a, b| a.0.cmp(b.0));
            let head_ref = format!("refs/heads/{}", repo.default_branch);
            let head_oid = repo.refs.get(&head_ref).copied();
            if sorted.is_empty() {
                let line = format!("{} capabilities^{{}}\0{}\n", ZERO_OID_HEX, caps);
                push_pkt_line(&mut pkt_body, line.as_bytes());
            } else {
                let mut first = true;
                if let Some(hoid) = head_oid {
                    let line = format!("{} HEAD\0{} symref=HEAD:{}\n", oid_to_hex(&hoid), caps, head_ref);
                    push_pkt_line(&mut pkt_body, line.as_bytes());
                    first = false;
                }
                for (ref_name, oid) in sorted {
                    let line = if first {
                        first = false;
                        format!("{} {}\0{}\n", oid_to_hex(oid), ref_name, caps)
                    } else {
                        format!("{} {}\n", oid_to_hex(oid), ref_name)
                    };
                    push_pkt_line(&mut pkt_body, line.as_bytes());
                    if let Some(peeled) = peel_tag(repo, *oid) {
                        let pline = format!("{} {}^{{}}\n", oid_to_hex(&peeled), ref_name);
                        push_pkt_line(&mut pkt_body, pline.as_bytes());
                    }
                }
            }
            pkt_body.extend_from_slice(b"0000");
            push_http_200(
                resp_out,
                "application/x-git-upload-pack-advertisement",
                "cache-control: no-cache\r\n",
                &pkt_body,
            );
            return None;
        }
        if service == "git-receive-pack" {
            if let Some(ref cached) = repo.receive_refs_http_cache {
                return Some(Arc::clone(cached));
            }
            let built = build_receive_refs_http(repo);
            resp_out.extend_from_slice(&built);
            return None;
        }
        push_http_err(resp_out, "400 Bad Request", "Unsupported service");
        return None;
    }

    if method == "POST" && rest_path == "/git-upload-pack" {
        {
            let store = store_lock.read().unwrap();
            if !store.is_authorized(auth) {
                push_http_err(resp_out, "401 Unauthorized", "Requires authentication");
                return None;
            }
            let repo = match store.get_repo(owner, repo_name) {
                Some(r) => r,
                None => {
                    push_http_err(resp_out, "404 Not Found", "Not Found");
                    return None;
                }
            };
            if is_v2 {
                if let Some(cached) = repo.fetch_http_cache.get(body) {
                    return Some(Arc::clone(cached));
                }
                if let Some(cached) = repo.ls_refs_http_cache.get(body) {
                    return Some(Arc::clone(cached));
                }
            }
        }

        let (built_http, cache_kind) = {
            let store = store_lock.read().unwrap();
            let repo = match store.get_repo(owner, repo_name) {
                Some(r) => r,
                None => {
                    push_http_err(resp_out, "404 Not Found", "Not Found");
                    return None;
                }
            };
            let packets = parse_pkt_lines(body);
            if is_v2 {
                let mut command = "";
                let mut args: Vec<&str> = Vec::with_capacity(16);
                let mut past_delim = false;
                for p in packets {
                    match p {
                        Pkt::Delim => past_delim = true,
                        Pkt::Flush => {}
                        Pkt::Data(d) => {
                            let s = std::str::from_utf8(d).unwrap_or("").trim_end_matches(&['\r', '\n'][..]);
                            if !past_delim && s.starts_with("command=") {
                                command = &s[8..];
                            } else {
                                args.push(s);
                            }
                        }
                    }
                }
                if command == "ls-refs" {
                    let peel = args.iter().any(|&a| a == "peel");
                    let symrefs = args.iter().any(|&a| a == "symrefs");
                    let prefixes: Vec<&str> = args
                        .iter()
                        .filter_map(|a| a.strip_prefix("ref-prefix "))
                        .collect();
                    let matches = |name: &str| prefixes.is_empty() || prefixes.iter().any(|p| name.starts_with(p));
                    let mut pkt_body = Vec::with_capacity(256);
                    let head_ref = format!("refs/heads/{}", repo.default_branch);
                    if let Some(hoid) = repo.refs.get(&head_ref) {
                        if matches("HEAD") {
                            let line = if symrefs {
                                format!("{} HEAD symref-target:{}\n", oid_to_hex(hoid), head_ref)
                            } else {
                                format!("{} HEAD\n", oid_to_hex(hoid))
                            };
                            push_pkt_line(&mut pkt_body, line.as_bytes());
                        }
                    }
                    let mut sorted: Vec<(&String, &Oid)> = repo.refs.iter().collect();
                    sorted.sort_by(|a, b| a.0.cmp(b.0));
                    for (ref_name, oid) in sorted {
                        if !matches(ref_name) {
                            continue;
                        }
                        let peeled = if peel { peel_tag(repo, *oid) } else { None };
                        let line = match peeled {
                            Some(p) => format!("{} {} peeled:{}\n", oid_to_hex(oid), ref_name, oid_to_hex(&p)),
                            None => format!("{} {}\n", oid_to_hex(oid), ref_name),
                        };
                        push_pkt_line(&mut pkt_body, line.as_bytes());
                    }
                    pkt_body.extend_from_slice(b"0000");
                    let mut http = Vec::with_capacity(pkt_body.len() + 160);
                    push_http_200(&mut http, "application/x-git-upload-pack-result", "", &pkt_body);
                    (http, 1u8)
                } else if command == "fetch" {
                    let mut wants = Vec::new();
                    let mut haves = Vec::new();
                    let mut client_shallow = FxHashSet::default();
                    let mut deepen: Option<usize> = None;
                    for a in args {
                        if let Some(rest) = a.strip_prefix("want ") {
                            if let Some(o) = hex_to_oid(rest.trim().as_bytes()) {
                                wants.push(o);
                            }
                        } else if let Some(rest) = a.strip_prefix("have ") {
                            if let Some(o) = hex_to_oid(rest.trim().as_bytes()) {
                                haves.push(o);
                            }
                        } else if let Some(rest) = a.strip_prefix("shallow ") {
                            if let Some(o) = hex_to_oid(rest.trim().as_bytes()) {
                                client_shallow.insert(o);
                            }
                        } else if let Some(rest) = a.strip_prefix("deepen ") {
                            deepen = rest.trim().parse().ok();
                        }
                    }
                    let (pack, shallow, unshallow) =
                        collect_fetch_pack(repo, &wants, &haves, &client_shallow, deepen);
                    let mut pkt_body = Vec::with_capacity(pack.len() + 256);
                    if !shallow.is_empty() || !unshallow.is_empty() {
                        push_pkt_line(&mut pkt_body, b"shallow-info\n");
                        for s in &shallow {
                            let line = format!("shallow {}\n", oid_to_hex(s));
                            push_pkt_line(&mut pkt_body, line.as_bytes());
                        }
                        for u in &unshallow {
                            let line = format!("unshallow {}\n", oid_to_hex(u));
                            push_pkt_line(&mut pkt_body, line.as_bytes());
                        }
                        pkt_body.extend_from_slice(b"0001");
                    }
                    push_pkt_line(&mut pkt_body, b"packfile\n");
                    push_sideband_pack(&mut pkt_body, &pack);
                    let mut http = Vec::with_capacity(pkt_body.len() + 160);
                    push_http_200(&mut http, "application/x-git-upload-pack-result", "", &pkt_body);
                    let can_cache = haves.is_empty() && client_shallow.is_empty();
                    (http, if can_cache { 2u8 } else { 0u8 })
                } else {
                    push_http_err(resp_out, "400 Bad Request", "Unsupported git v2 command");
                    return None;
                }
            } else {
                let mut wants = Vec::new();
                let mut haves = Vec::new();
                let mut client_shallow = FxHashSet::default();
                let mut deepen: Option<usize> = None;
                for p in packets {
                    if let Pkt::Data(d) = p {
                        let s = std::str::from_utf8(d).unwrap_or("");
                        let s = s.split('\0').next().unwrap_or(s).trim_end_matches(&['\r', '\n'][..]);
                        if let Some(rest) = s.strip_prefix("want ") {
                            if rest.len() >= 40 {
                                if let Some(o) = hex_to_oid(&rest.as_bytes()[..40]) {
                                    wants.push(o);
                                }
                            }
                        } else if let Some(rest) = s.strip_prefix("have ") {
                            if rest.len() >= 40 {
                                if let Some(o) = hex_to_oid(&rest.as_bytes()[..40]) {
                                    haves.push(o);
                                }
                            }
                        } else if let Some(rest) = s.strip_prefix("shallow ") {
                            if rest.len() >= 40 {
                                if let Some(o) = hex_to_oid(&rest.as_bytes()[..40]) {
                                    client_shallow.insert(o);
                                }
                            }
                        } else if let Some(rest) = s.strip_prefix("deepen ") {
                            deepen = rest.trim().parse().ok();
                        }
                    }
                }
                let (pack, _, _) = collect_fetch_pack(repo, &wants, &haves, &client_shallow, deepen);
                let mut pkt_body = Vec::with_capacity(pack.len() + 64);
                push_pkt_line(&mut pkt_body, b"NAK\n");
                push_sideband_pack(&mut pkt_body, &pack);
                let mut http = Vec::with_capacity(pkt_body.len() + 160);
                push_http_200(&mut http, "application/x-git-upload-pack-result", "", &pkt_body);
                (http, 0u8)
            }
        };

        let arc = Arc::new(built_http);
        if cache_kind != 0 {
            let mut store = store_lock.write().unwrap();
            if let Some(repo) = store.get_repo_mut(owner, repo_name) {
                if cache_kind == 1 && repo.ls_refs_http_cache.len() < 32 {
                    repo.ls_refs_http_cache.insert(body.to_vec(), Arc::clone(&arc));
                } else if cache_kind == 2 && repo.fetch_http_cache.len() < 32 {
                    repo.fetch_http_cache.insert(body.to_vec(), Arc::clone(&arc));
                }
            }
        }
        return Some(arc);
    }

    if method == "POST" && rest_path == "/git-receive-pack" {
        let mut store = store_lock.write().unwrap();
        if !store.is_authorized(auth) {
            push_http_err(resp_out, "401 Unauthorized", "Requires authentication");
            return None;
        }
        let repo = match store.get_repo_mut(owner, repo_name) {
            Some(r) => r,
            None => {
                push_http_err(resp_out, "404 Not Found", "Not Found");
                return None;
            }
        };

        let (commands, pack) = parse_receive_packets(body);
        let has_creates = commands.iter().any(|c| c.new_oid != ZERO_OID);
        let mut unpack_err: Option<&str> = None;
        if !pack.is_empty() && (has_creates || pack.len() >= 12) {
            if let Err(e) = unpack_packfile(pack, repo) {
                unpack_err = Some(e);
            }
        }

        let mut report = Vec::with_capacity(128);
        if let Some(err_msg) = unpack_err {
            let line = format!("unpack {}\n", err_msg);
            push_pkt_line(&mut report, line.as_bytes());
            for cmd in &commands {
                let ng = format!("ng {} unpack failed\n", cmd.ref_name);
                push_pkt_line(&mut report, ng.as_bytes());
            }
        } else {
            push_pkt_line(&mut report, b"unpack ok\n");
            for cmd in &commands {
                let cur = repo.refs.get(cmd.ref_name).copied().unwrap_or(ZERO_OID);
                if cmd.old_oid != cur {
                    let ng = format!("ng {} stale info\n", cmd.ref_name);
                    push_pkt_line(&mut report, ng.as_bytes());
                    continue;
                }
                if cmd.new_oid == ZERO_OID {
                    ForgeStore::set_ref(repo, cmd.ref_name, None);
                    let ok = format!("ok {}\n", cmd.ref_name);
                    push_pkt_line(&mut report, ok.as_bytes());
                    continue;
                }
                if !repo.objects.contains_key(&cmd.new_oid) {
                    let ng = format!("ng {} missing object\n", cmd.ref_name);
                    push_pkt_line(&mut report, ng.as_bytes());
                    continue;
                }
                if cmd.old_oid != ZERO_OID && !ForgeStore::is_ancestor(repo, cmd.old_oid, cmd.new_oid) {
                    let ng = format!("ng {} non-fast-forward\n", cmd.ref_name);
                    push_pkt_line(&mut report, ng.as_bytes());
                    continue;
                }
                ForgeStore::set_ref(repo, cmd.ref_name, Some(cmd.new_oid));
                let ok = format!("ok {}\n", cmd.ref_name);
                push_pkt_line(&mut report, ok.as_bytes());
            }
            repo.receive_refs_http_cache = Some(Arc::new(build_receive_refs_http(repo)));
        }
        report.extend_from_slice(b"0000");

        let mut body_out = Vec::with_capacity(report.len() + 16);
        let total = 4 + 1 + report.len();
        let hex = b"0123456789abcdef";
        body_out.push(hex[(total >> 12) & 0xf]);
        body_out.push(hex[(total >> 8) & 0xf]);
        body_out.push(hex[(total >> 4) & 0xf]);
        body_out.push(hex[total & 0xf]);
        body_out.push(1u8);
        body_out.extend_from_slice(&report);
        body_out.extend_from_slice(b"0000");

        push_http_200(resp_out, "application/x-git-receive-pack-result", "", &body_out);
        return None;
    }

    push_http_err(resp_out, "404 Not Found", "Not Found");
    None
}

fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    for part in query.split('&') {
        if let Some((k, v)) = part.split_once('=') {
            if k == key {
                return Some(v);
            }
        }
    }
    None
}
