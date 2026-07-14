import { createReadStream, existsSync, statSync } from "node:fs";
import { createServer } from "node:http";
import { extname, join, normalize, resolve } from "node:path";

const root = resolve(new URL("..", import.meta.url).pathname);
const port = Number(process.env.PORT || 8787);
const bindHost = process.env.BIND_HOST || "127.0.0.1";

const proxyTargets = new Map([
  ["/proxy/codex/responses", "https://chatgpt.com/backend-api/codex/responses"],
  ["/proxy/openai/responses", "https://api.openai.com/v1/responses"],
]);

const contentTypes = new Map([
  [".css", "text/css; charset=utf-8"],
  [".html", "text/html; charset=utf-8"],
  [".js", "text/javascript; charset=utf-8"],
  [".json", "application/json; charset=utf-8"],
  [".wasm", "application/wasm"],
]);

function send(res, status, body, headers = {}) {
  res.writeHead(status, {
    "Access-Control-Allow-Origin": "*",
    "Access-Control-Allow-Headers": "Authorization, Content-Type, Accept, OpenAI-Beta, chatgpt-account-id, originator, session_id",
    "Access-Control-Allow-Methods": "GET, POST, OPTIONS",
    ...headers,
  });
  res.end(body);
}

function safeFilePath(pathname) {
  const normalized = normalize(decodeURIComponent(pathname)).replace(/^(\.\.[/\\])+/, "");
  const relative = normalized === "/" ? "/index.html" : normalized;
  const fullPath = resolve(join(root, relative));
  if (!fullPath.startsWith(root)) return null;
  return fullPath;
}

async function readRequestBody(req) {
  const chunks = [];
  for await (const chunk of req) {
    chunks.push(chunk);
  }
  return Buffer.concat(chunks);
}

function proxyHeaders(req) {
  const headers = {};
  for (const [key, value] of Object.entries(req.headers)) {
    if (!value) continue;
    const lower = key.toLowerCase();
    if (["host", "connection", "content-length", "origin", "referer"].includes(lower)) continue;
    headers[key] = value;
  }
  if (!headers["user-agent"] && !headers["User-Agent"]) {
    headers["User-Agent"] = "pi_agent_rust";
  }
  return headers;
}

async function proxyRequest(req, res, targetUrl) {
  const body = req.method === "GET" || req.method === "HEAD" ? undefined : await readRequestBody(req);
  const upstream = await fetch(targetUrl, {
    method: req.method,
    headers: proxyHeaders(req),
    body,
    duplex: body ? "half" : undefined,
  });

  const responseHeaders = {
    "Access-Control-Allow-Origin": "*",
    "Access-Control-Expose-Headers": "*",
  };
  for (const [key, value] of upstream.headers.entries()) {
    if (["content-encoding", "content-length", "connection", "transfer-encoding"].includes(key.toLowerCase())) {
      continue;
    }
    responseHeaders[key] = value;
  }
  res.writeHead(upstream.status, responseHeaders);

  if (!upstream.body) {
    res.end();
    return;
  }

  const reader = upstream.body.getReader();
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      res.write(Buffer.from(value));
    }
    res.end();
  } catch (error) {
    res.destroy(error);
  }
}

createServer(async (req, res) => {
  try {
    const url = new URL(req.url || "/", `http://${req.headers.host || `${bindHost}:${port}`}`);
    if (req.method === "OPTIONS") {
      send(res, 204, "");
      return;
    }

    const proxyTarget = proxyTargets.get(url.pathname);
    if (proxyTarget) {
      await proxyRequest(req, res, proxyTarget);
      return;
    }

    const fullPath = safeFilePath(url.pathname === "/" ? "/web/" : url.pathname);
    if (!fullPath) {
      send(res, 403, "Forbidden");
      return;
    }

    const filePath =
      existsSync(fullPath) && statSync(fullPath).isDirectory() ? join(fullPath, "index.html") : fullPath;
    if (!existsSync(filePath) || !statSync(filePath).isFile()) {
      send(res, 404, "Not found");
      return;
    }

    const ext = extname(filePath);
    res.writeHead(200, {
      "Access-Control-Allow-Origin": "*",
      "Content-Type": contentTypes.get(ext) || "application/octet-stream",
    });
    createReadStream(filePath).pipe(res);
  } catch (error) {
    send(res, 500, error instanceof Error ? error.stack || error.message : String(error));
  }
}).listen(port, bindHost, () => {
  console.log(`Serving Pi WASM prototype at http://${bindHost}:${port}/web/`);
  console.log("Proxy routes:");
  for (const [path, target] of proxyTargets.entries()) {
    console.log(`  ${path} -> ${target}`);
  }
});
