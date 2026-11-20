import { createServer, type IncomingMessage, type Server, type ServerResponse } from 'node:http';
import { handleGitRequest } from './git.ts';
import { handleGraphQLRequest } from './graphql.ts';
import { handleMcpRequest } from './mcp.ts';
import { handleRestRequest, sendJson } from './rest.ts';
import { ForgeStore } from './store.ts';

function readBody(req: IncomingMessage): Promise<Buffer> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    req.on('data', (chunk: Buffer) => chunks.push(chunk));
    req.on('end', () => resolve(Buffer.concat(chunks)));
    req.on('error', reject);
  });
}

export function createApp(): Server {
  const store = new ForgeStore();

  const server = createServer(async (req: IncomingMessage, res: ServerResponse) => {
    const reqId = store.nextRequestId();
    let url: URL;
    try {
      url = new URL(req.url ?? '/', 'http://entry');
    } catch {
      sendJson(res, 400, { message: 'Invalid URL' }, reqId);
      return;
    }
    const path = url.pathname;
    const method = req.method ?? 'GET';

    if (method === 'GET' && path === '/healthz') {
      sendJson(res, 200, { ok: true }, reqId);
      return;
    }

    let rawBody = Buffer.alloc(0);
    if (method !== 'GET' && method !== 'HEAD') {
      try {
        rawBody = await readBody(req);
      } catch {
        sendJson(res, 400, { message: 'Failed to read request body' }, reqId);
        return;
      }
    }

    try {
      if (path === '/api/v3' || path.startsWith('/api/v3/')) {
        handleRestRequest(req, res, store, path, url.searchParams, rawBody, reqId);
        return;
      }
      if (path === '/api/graphql') {
        handleGraphQLRequest(req, res, store, rawBody, reqId);
        return;
      }
      if (path === '/mcp') {
        handleMcpRequest(req, res, store, rawBody, reqId);
        return;
      }
      const gitMatch = path.match(/^\/([^/]+)\/([^/]+)\.git(\/.*)$/);
      if (gitMatch) {
        await handleGitRequest(
          req,
          res,
          store,
          decodeURIComponent(gitMatch[1]),
          decodeURIComponent(gitMatch[2]),
          gitMatch[3],
          url.searchParams,
          rawBody,
          reqId,
        );
        return;
      }
      sendJson(res, 404, { message: 'Not Found' }, reqId);
    } catch (err) {
      sendJson(res, 500, { message: err instanceof Error ? err.message : 'Internal Server Error' }, reqId);
    }
  });

  server.keepAliveTimeout = 60_000;
  server.headersTimeout = 65_000;
  return server;
}
