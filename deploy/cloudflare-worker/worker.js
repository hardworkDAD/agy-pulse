const encoder = new TextEncoder();
const MAX_KV_VALUE_BYTES = 25 * 1024 * 1024;

function json(data, status = 200) {
  return new Response(JSON.stringify(data), {
    status,
    headers: { "Content-Type": "application/json; charset=utf-8" },
  });
}

async function constantTimeEqual(a, b) {
  const [ha, hb] = await Promise.all([
    crypto.subtle.digest("SHA-256", encoder.encode(a)),
    crypto.subtle.digest("SHA-256", encoder.encode(b)),
  ]);
  const va = new Uint8Array(ha);
  const vb = new Uint8Array(hb);
  let diff = 0;
  for (let i = 0; i < va.length; i++) {
    diff |= va[i] ^ vb[i];
  }
  return diff === 0;
}

async function isAuthorized(request, env) {
  const expected = typeof env.SYNC_TOKEN === "string" ? env.SYNC_TOKEN.trim() : "";
  if (!expected) return false;
  const auth = request.headers.get("Authorization") || "";
  if (!auth.startsWith("Bearer ")) return false;
  const provided = auth.slice(7);
  if (!provided) return false;
  return constantTimeEqual(expected, provided);
}

function validateKey(key, allowEmpty = false) {
  if (!key) return allowEmpty;
  if (key.startsWith("/") || key.endsWith("/") || key.includes("\\")) return false;
  if (/[\x00-\x1f\x7f]/.test(key)) return false;
  const segments = key.split("/");
  return segments.every((seg) => seg.length > 0 && seg !== "." && seg !== "..");
}

function getStorage(env) {
  if (env.BUCKET) return { kind: "r2", backend: env.BUCKET };
  if (env.KV) return { kind: "kv", backend: env.KV };
  return null;
}

function resolveApiPath(pathname, env) {
  const baseRaw = typeof env.BASE_PATH === "string" ? env.BASE_PATH.trim() : "";
  if (baseRaw) {
    const normalizedBase = "/" + baseRaw.replace(/^\/+|\/+$/g, "");
    if (normalizedBase !== "/") {
      if (pathname !== normalizedBase && !pathname.startsWith(normalizedBase + "/")) {
        return null;
      }
      return pathname.slice(normalizedBase.length) || "/";
    }
    return pathname;
  }

  const v1Idx = pathname.indexOf("/v1/");
  if (v1Idx < 0) return null;
  if (v1Idx > 0) {
    const mountPrefix = pathname.slice(1, v1Idx);
    let decodedPrefix;
    try {
      decodedPrefix = decodeURIComponent(mountPrefix);
    } catch {
      return null;
    }
    if (!validateKey(decodedPrefix, false)) {
      return null;
    }
  }
  return pathname.slice(v1Idx);
}

export default {
  async fetch(request, env) {
    try {
      if (!(await isAuthorized(request, env))) {
        return json({ error: "unauthorized" }, 401);
      }

      const storage = getStorage(env);
      if (!storage) {
        return json({ error: "neither BUCKET (R2) nor KV binding is configured" }, 500);
      }

      const url = new URL(request.url);
      const { pathname, searchParams } = url;
      const apiPath = resolveApiPath(pathname, env);
      if (!apiPath) {
        return json({ error: "not found" }, 404);
      }

      if (request.method === "GET" && apiPath === "/v1/health") {
        return json({ ok: true, storage: storage.kind }, 200);
      }

      if (request.method === "GET" && apiPath === "/v1/objects") {
        const prefix = (searchParams.get("prefix") || "").replace(/\/+$/, "");
        if (!validateKey(prefix, true)) {
          return json({ error: "invalid prefix" }, 400);
        }
        const cursor = searchParams.get("cursor") || undefined;
        if (storage.kind === "r2") {
          const listed = await storage.backend.list({ prefix, cursor, limit: 1000 });
          const truncated = Boolean(listed.truncated);
          return json({
            keys: listed.objects.map((o) => o.key),
            cursor: truncated ? listed.cursor || null : null,
            truncated,
          });
        }
        const listed = await storage.backend.list({ prefix, cursor, limit: 1000 });
        const truncated = !listed.list_complete;
        return json({
          keys: listed.keys.map((k) => k.name),
          cursor: truncated ? listed.cursor || null : null,
          truncated,
        });
      }

      if (!apiPath.startsWith("/v1/objects/")) {
        return json({ error: "not found" }, 404);
      }

      let key;
      try {
        key = decodeURIComponent(apiPath.slice("/v1/objects/".length));
      } catch {
        return json({ error: "invalid percent encoding in key" }, 400);
      }
      if (!validateKey(key, false)) {
        return json({ error: "invalid object key" }, 400);
      }

      if (request.method === "GET") {
        if (storage.kind === "r2") {
          const obj = await storage.backend.get(key);
          if (!obj) return json({ error: "not found" }, 404);
          const contentType = obj.httpMetadata?.contentType || "application/octet-stream";
          return new Response(obj.body, { status: 200, headers: { "Content-Type": contentType } });
        }
        const { value, metadata } = await storage.backend.getWithMetadata(key, { type: "arrayBuffer" });
        if (value === null) return json({ error: "not found" }, 404);
        const contentType = metadata?.contentType || "application/octet-stream";
        return new Response(value, { status: 200, headers: { "Content-Type": contentType } });
      }

      if (request.method === "PUT") {
        if (storage.kind === "kv") {
          const rawLen = request.headers.get("Content-Length");
          if (rawLen !== null) {
            const parsedLen = Number(rawLen);
            if (Number.isFinite(parsedLen) && parsedLen > MAX_KV_VALUE_BYTES) {
              return json({ error: "payload too large for KV storage (max 25 MiB)" }, 413);
            }
          }
        }
        const contentType = request.headers.get("Content-Type") || "application/octet-stream";
        const body = await request.arrayBuffer();
        if (storage.kind === "kv" && body.byteLength > MAX_KV_VALUE_BYTES) {
          return json({ error: "payload too large for KV storage (max 25 MiB)" }, 413);
        }
        if (storage.kind === "r2") {
          await storage.backend.put(key, body, { httpMetadata: { contentType } });
        } else {
          await storage.backend.put(key, body, { metadata: { contentType } });
        }
        return new Response(null, { status: 204 });
      }

      if (request.method === "DELETE") {
        await storage.backend.delete(key);
        return new Response(null, { status: 204 });
      }

      return json({ error: "method not allowed" }, 405);
    } catch {
      return json({ error: "internal server error" }, 500);
    }
  },
};
