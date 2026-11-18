#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""agy-statusline: Zero-inflation real-time context & compaction monitor for Antigravity CLI.

Reads the authoritative server-pushed CheckpointConfig.max_context_tokens directly
from ~/.gemini/antigravity-cli/conversations/<conversation_id>.db (Protobuf wire fields:
gen_metadata f1.f9.f10.f4 and executor_metadata f10.f2.f5), so any backend limit
change (e.g. 256K -> 512K -> 900K -> 1M) is reflected automatically on Turn 1
without hardcoding model limits.
"""

import glob
import json
import os
import sqlite3
import sys
import tempfile
from urllib.parse import quote


def fmt_tok(n):
    n = int(n or 0)
    if n >= 1000000:
        val = n / 1048576.0 if n % 1048576 == 0 else n / 1000000.0
        s = "{:.1f}".format(val)
        return (s[:-2] if s.endswith(".0") else s) + "M"
    if n >= 1000:
        s = "{:.1f}".format(n / 1000.0)
        return (s[:-2] if s.endswith(".0") else s) + "K"
    return str(n)


def build_bar(pct, width=12):
    pct = max(0.0, min(100.0, float(pct or 0.0)))
    exact = pct * width / 100.0
    filled = int(exact)
    rem = int((exact - filled) * 100)
    chars = []
    for i in range(width):
        if i < filled:
            chars.append("█")
        elif i == filled:
            if rem >= 75:
                chars.append("▓")
            elif rem >= 50:
                chars.append("▒")
            elif rem >= 25:
                chars.append("░")
            else:
                chars.append("·")
        else:
            chars.append("·")
    return "".join(chars)


# ─── Minimal Protobuf Wire Decoder (Zero External Dependencies) ──────────────
def _decode_varint(buf, o):
    res = 0
    shift = 0
    n = len(buf)
    while o < n:
        b = buf[o]
        o += 1
        res |= (b & 0x7F) << shift
        if not (b & 0x80):
            return res, o
        shift += 7
    return None, o


def _pb_get_sub(buf, field_num):
    if not buf:
        return None
    o = 0
    n = len(buf)
    while o < n:
        tag, o = _decode_varint(buf, o)
        if tag is None:
            break
        fn = tag >> 3
        wire = tag & 7
        if wire == 0:
            _, o = _decode_varint(buf, o)
        elif wire == 2:
            ln, o = _decode_varint(buf, o)
            if ln is None or o + ln > n:
                break
            if fn == field_num:
                return buf[o : o + ln]
            o += ln
        elif wire == 1:
            o += 8
        elif wire == 5:
            o += 4
        else:
            break
    return None


def _pb_get_varint(buf, field_num):
    if not buf:
        return None
    o = 0
    n = len(buf)
    while o < n:
        tag, o = _decode_varint(buf, o)
        if tag is None:
            break
        fn = tag >> 3
        wire = tag & 7
        if wire == 0:
            v, o = _decode_varint(buf, o)
            if fn == field_num:
                return v
        elif wire == 2:
            ln, o = _decode_varint(buf, o)
            if ln is None or o + ln > n:
                break
            o += ln
        elif wire == 1:
            o += 8
        elif wire == 5:
            o += 4
        else:
            break
    return None


def _sqlite_ro_uri(db_path):
    norm = os.path.abspath(db_path).replace("\\", "/")
    if not norm.startswith("/"):
        norm = "/" + norm
    return "file:{}?mode=ro".format(quote(norm, safe="/:"))


def _extract_limit_from_db(db_path):
    """Reads server-pushed CheckpointConfig.max_context_tokens from conversation SQLite DB."""
    if not db_path or not os.path.isfile(db_path):
        return None
    conn = None
    try:
        conn = sqlite3.connect(_sqlite_ro_uri(db_path), uri=True, timeout=0.15)
        # 1. Latest generation metadata (f1.f9.f10.f4) reflects active turn's live limit
        row = conn.execute("SELECT data FROM gen_metadata ORDER BY idx DESC LIMIT 1").fetchone()
        if row and row[0]:
            b = _pb_get_sub(row[0], 1)
            b = _pb_get_sub(b, 9)
            b = _pb_get_sub(b, 10)
            lim = _pb_get_varint(b, 4)
            if lim and 16000 <= lim <= 10000000:
                return int(lim)
        # 2. Executor CascadeConfig.CheckpointConfig.max_context_tokens (f10.f2.f5)
        erow = conn.execute("SELECT data FROM executor_metadata ORDER BY idx DESC LIMIT 1").fetchone()
        if erow and erow[0]:
            b = _pb_get_sub(erow[0], 10)
            b = _pb_get_sub(b, 2)
            lim = _pb_get_varint(b, 5)
            if lim and 16000 <= lim <= 10000000:
                return int(lim)
    except Exception:
        return None
    finally:
        if conn is not None:
            try:
                conn.close()
            except Exception:
                pass
    return None


def resolve_server_compaction_limit(conv_id, t_path, cache):
    """Resolves live server compaction limit for active conversation (with WAL-aware cache)."""
    base_dir = os.path.expanduser("~/.gemini/antigravity-cli")
    if t_path:
        expanded = os.path.expanduser(t_path).replace("\\", "/")
        marker = "/brain/"
        pos = expanded.find(marker)
        if pos > 0:
            base_dir = expanded[:pos]
            if not conv_id:
                rest = expanded[pos + len(marker) :]
                conv_id = rest.split("/")[0]

    conv_dir = os.path.join(base_dir, "conversations")
    db_path = os.path.join(conv_dir, "{}.db".format(conv_id)) if conv_id else ""

    if db_path and os.path.isfile(db_path):
        try:
            db_sz = os.path.getsize(db_path)
            wal_path = db_path + "-wal"
            wal_sz = os.path.getsize(wal_path) if os.path.isfile(wal_path) else 0
            sig = "{}:{}".format(db_sz, wal_sz)
        except Exception:
            sig = ""

        db_cache = cache.setdefault("_db_limits", {})
        cached = db_cache.get(db_path)
        if isinstance(cached, dict) and cached.get("sig") == sig and cached.get("limit"):
            return int(cached["limit"]), True

        lim = _extract_limit_from_db(db_path)
        if lim:
            if len(db_cache) > 16 and db_path not in db_cache:
                db_cache.pop(next(iter(db_cache)), None)
            db_cache[db_path] = {"sig": sig, "limit": lim}
            cache["_last_known_limit"] = lim
            return lim, True

    # Brand-new session before Turn 1: inherit most recent conversation DB's server limit
    if cache.get("_last_known_limit"):
        return int(cache["_last_known_limit"]), False

    if os.path.isdir(conv_dir):
        try:
            dbs = sorted(
                glob.glob(os.path.join(conv_dir, "*.db")),
                key=os.path.getmtime,
                reverse=True,
            )[:5]
            for cand in dbs:
                lim = _extract_limit_from_db(cand)
                if lim:
                    cache["_last_known_limit"] = lim
                    return lim, False
        except Exception:
            pass

    return 512000, False


def _cache_path():
    uid = getattr(os, "getuid", lambda: 0)()
    return os.path.join(tempfile.gettempdir(), "agy_ctx_status_{}.json".format(uid))


def load_cache():
    cache_file = _cache_path()
    try:
        if os.path.isfile(cache_file):
            with open(cache_file, "r", encoding="utf-8") as cf:
                return cache_file, (json.load(cf) or {})
    except Exception:
        pass
    return cache_file, {}


def save_cache(cache_file, cache):
    try:
        tmp_file = "{}.{}".format(cache_file, os.getpid())
        with open(tmp_file, "w", encoding="utf-8") as cf:
            json.dump(cache, cf)
        os.replace(tmp_file, cache_file)
    except Exception:
        pass


def scan_transcript_incremental(t_path, cache):
    """Incrementally scans transcript.jsonl for checkpoints, peak context, and last turn."""
    if not t_path:
        return None
    t_path = os.path.expanduser(t_path)
    if not os.path.isfile(t_path):
        return None

    try:
        st = os.stat(t_path)
        f_ino = int(getattr(st, "st_ino", 0))
        f_size = int(st.st_size)
    except Exception:
        return None

    t_map = cache.setdefault("_transcripts", {})
    entry = t_map.get(t_path)
    if (
        isinstance(entry, dict)
        and entry.get("ino") == f_ino
        and isinstance(entry.get("offset"), int)
        and 0 <= entry["offset"] <= f_size
    ):
        offset = entry["offset"]
        ckpts = int(entry.get("ckpts") or 0)
        peak_ctx = int(entry.get("peak_ctx") or 0)
        tot_out = int(entry.get("tot_out") or 0)
        last_turn = entry.get("last_turn")
    else:
        offset = 0
        ckpts = 0
        peak_ctx = 0
        tot_out = 0
        last_turn = None

    if offset < f_size:
        try:
            with open(t_path, "rb") as f:
                f.seek(offset)
                raw_bytes = f.read()
            last_nl = raw_bytes.rfind(b"\n")
            if last_nl >= 0:
                chunk = raw_bytes[: last_nl + 1]
                offset += last_nl + 1
                for bline in chunk.splitlines():
                    if b'"CHECKPOINT"' in bline:
                        try:
                            obj = json.loads(bline.decode("utf-8", "ignore"))
                            if obj.get("type") == "CHECKPOINT":
                                ckpts += 1
                        except Exception:
                            pass
                    elif b'"input_tokens"' in bline and b'"PLANNER_RESPONSE"' in bline:
                        try:
                            obj = json.loads(bline.decode("utf-8", "ignore"))
                        except Exception:
                            continue
                        if obj.get("type") == "PLANNER_RESPONSE" and isinstance(obj.get("input_tokens"), int):
                            inp = int(obj.get("input_tokens") or 0)
                            c_read = int(obj.get("cache_read_tokens") or 0)
                            c_create = int(obj.get("cache_creation_tokens") or 0)
                            out = int(obj.get("output_tokens") or 0)
                            tot_out += out
                            turn_total = inp + c_create + c_read + out
                            if turn_total > peak_ctx:
                                peak_ctx = turn_total
                            last_turn = {
                                "input_tokens": inp,
                                "cache_read_input_tokens": c_read,
                                "cache_creation_input_tokens": c_create,
                                "output_tokens": out,
                            }
                if len(t_map) > 12 and t_path not in t_map:
                    t_map.pop(next(iter(t_map)), None)
                t_map[t_path] = {
                    "ino": f_ino,
                    "offset": offset,
                    "ckpts": ckpts,
                    "peak_ctx": peak_ctx,
                    "tot_out": tot_out,
                    "last_turn": last_turn,
                }
        except Exception:
            pass

    return {
        "ckpts": ckpts,
        "peak_ctx": peak_ctx,
        "tot_out": tot_out,
        "last_turn": last_turn,
    }


def main():
    if hasattr(sys.stdout, "reconfigure"):
        try:
            sys.stdout.reconfigure(encoding="utf-8")
        except Exception:
            pass

    raw = sys.stdin.read().strip()
    if not raw:
        return
    data = json.loads(raw)

    R = "\033[0m"
    B = "\033[1m"
    DIM = "\033[2m"
    RED = "\033[31m"
    GREEN = "\033[32m"
    YELLOW = "\033[33m"
    CYAN = "\033[36m"
    MAGENTA = "\033[35m"

    cols = int(data.get("terminal_width") or 100)

    model_obj = data.get("model")
    model_name = ""
    if isinstance(model_obj, dict):
        model_name = model_obj.get("display_name") or model_obj.get("id") or ""
        effort = model_obj.get("effort") or ""
        if effort and effort.lower() not in model_name.lower():
            model_name = "{} ({})".format(model_name, effort)
    elif isinstance(model_obj, str):
        model_name = model_obj

    cache_file, cache = load_cache()
    before_snapshot = json.dumps(cache, sort_keys=True)

    conv_id = data.get("conversation_id") or data.get("session_id") or ""
    t_path = data.get("transcript_path") or ""

    t_stats = scan_transcript_incremental(t_path, cache)
    ckpts = int(t_stats["ckpts"]) if t_stats else 0
    peak_ctx = int(t_stats["peak_ctx"]) if t_stats else 0

    server_limit, _ = resolve_server_compaction_limit(conv_id, t_path, cache)

    if json.dumps(cache, sort_keys=True) != before_snapshot:
        save_cache(cache_file, cache)

    cw = data.get("context_window") or {}
    raw_win_size = int(cw.get("context_window_size") or 0)
    raw_used_pct = float(cw.get("used_percentage") or 0.0)
    tot_out = int(cw.get("total_output_tokens") or 0)
    if not tot_out and t_stats:
        tot_out = int(t_stats.get("tot_out") or 0)

    cur = cw.get("current_usage")
    if not isinstance(cur, dict) and t_stats and isinstance(t_stats.get("last_turn"), dict):
        cur = t_stats["last_turn"]

    has_turn = isinstance(cur, dict)
    inp = int(cur.get("input_tokens") or 0) if has_turn else 0
    c_create = int(cur.get("cache_creation_input_tokens") or 0) if has_turn else 0
    c_read = int(cur.get("cache_read_input_tokens") or 0) if has_turn else 0
    turn_out = int(cur.get("output_tokens") or 0) if has_turn else 0

    prompt_ctx = inp + c_create + c_read
    active_ctx = prompt_ctx + turn_out

    if active_ctx == 0 and raw_used_pct > 0 and raw_win_size > 0:
        active_ctx = int(round(raw_win_size * raw_used_pct / 100.0))

    if active_ctx > peak_ctx:
        peak_ctx = active_ctx

    # Effective ceiling = min(model physical window, server-pushed CheckpointConfig.max_context_tokens)
    if raw_win_size > 0 and server_limit > 0:
        effective_limit = min(raw_win_size, server_limit)
    else:
        effective_limit = server_limit or raw_win_size or 512000

    real_pct = (active_ctx * 100.0 / float(effective_limit)) if active_ctx > 0 else 0.0

    if real_pct >= 85.0:
        bar_color = RED
    elif real_pct >= 65.0:
        bar_color = YELLOW
    else:
        bar_color = CYAN

    bar_width = 12 if cols >= 80 else 8
    bar = build_bar(real_pct, bar_width)

    if active_ctx > 0 or has_turn:
        tok_label = "{}/{}".format(fmt_tok(active_ctx), fmt_tok(effective_limit))
        ctx_seg = "{dim}上下文{r} {bc}{bar}{r} {b}{pct:.1f}%{r} {dim}({tok}){r}".format(
            dim=DIM, r=R, bc=bar_color, bar=bar, b=B, pct=real_pct, tok=tok_label
        )
    else:
        ctx_seg = "{dim}上下文{r} {dim}{bar}{r} {b}0.0%{r} {dim}(上限 {lim}){r}".format(
            dim=DIM, r=R, bar=bar, b=B, lim=fmt_tok(effective_limit)
        )

    sep = "{dim} · {r}".format(dim=DIM, r=R)
    parts = []

    if cols >= 105 and model_name:
        parts.append("{m}◈ {name}{r}".format(m=MAGENTA, name=model_name, r=R))

    parts.append(ctx_seg)

    if ckpts > 0:
        if cols >= 95 and peak_ctx > 0:
            parts.append("{y}压缩×{n}{r}{dim}(峰{pk}){r}".format(
                y=YELLOW, n=ckpts, r=R, dim=DIM, pk=fmt_tok(peak_ctx)
            ))
        else:
            parts.append("{y}压缩×{n}{r}".format(y=YELLOW, n=ckpts, r=R))

    if prompt_ctx > 0:
        cache_pct = c_read * 100.0 / float(prompt_ctx)
        cache_color = GREEN if cache_pct >= 50.0 else DIM
        if cols >= 105 and ckpts == 0:
            parts.append("{dim}缓存{r} {cc}{pct:.0f}%{r} {dim}({cr}){r}".format(
                dim=DIM, r=R, cc=cache_color, pct=cache_pct, cr=fmt_tok(c_read)
            ))
        else:
            parts.append("{dim}缓存{r} {cc}{pct:.0f}%{r}".format(
                dim=DIM, r=R, cc=cache_color, pct=cache_pct
            ))

    if cols >= 85 and tot_out > 0:
        parts.append("{dim}输出{r} {b}{out}{r}".format(dim=DIM, r=R, b=B, out=fmt_tok(tot_out)))

    cost_obj = data.get("cost")
    if cols >= 95 and isinstance(cost_obj, dict) and cost_obj.get("total_usd") is not None:
        usd = float(cost_obj.get("total_usd") or 0.0)
        if usd > 0:
            usd_fmt = "${:.3f}".format(usd) if usd < 0.01 else "${:.2f}".format(usd)
            parts.append("{dim}费用{r} {b}{u}{r}".format(dim=DIM, r=R, b=B, u=usd_fmt))

    if real_pct >= 85.0:
        parts.append("{red}{b}即将触发压缩{r}".format(red=RED, b=B, r=R))

    sys.stdout.write(sep.join(parts) + "\n")


if __name__ == "__main__":
    try:
        main()
    except Exception:
        sys.stdout.write("\n")
        sys.exit(0)
