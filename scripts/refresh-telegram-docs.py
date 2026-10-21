#!/usr/bin/env python3
"""Refresh factual Telegram method availability. Normal documentation builds are offline.

Only public documentation is fetched. No account, API key, or session is used.
The checked-in TL schema remains authoritative for method signatures and types.
"""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime, timezone
from email.utils import parsedate_to_datetime
import hashlib
import json
from pathlib import Path
import re
import time
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parents[1]
VERSION = 1
AVAILABILITIES = {"users", "bots", "both", "unknown"}
NAME = re.compile(r"[A-Za-z_][A-Za-z_0-9]*(?:\.[A-Za-z_][A-Za-z_0-9]*)*\Z")


def read_schema(root: Path) -> dict[str, dict]:
    """Read flat TL declarations, retaining local parameter types verbatim."""
    methods = {}
    for filename in ("api.tl", "mtproto.tl"):
        path = root / "grammers-tl-types" / "tl" / filename
        source = re.sub(r"//[^\n]*", "", path.read_text(encoding="utf-8"))
        functions = False
        for part in re.split(r"(---(?:functions|types)---)", source):
            if part.startswith("---"):
                functions = part == "---functions---"
                continue
            if not functions:
                continue
            for declaration in part.split(";"):
                declaration = " ".join(declaration.split())
                if not declaration:
                    continue
                match = re.fullmatch(r"([\w.]+)#([0-9a-fA-F]+)\s*(.*?)\s*=\s*(.+)", declaration)
                if not match or not NAME.fullmatch(match[1]):
                    raise ValueError(f"Unsupported TL method declaration in {filename}: {declaration[:100]}")
                name, constructor, body, result = match.groups()
                # TL's generic declarations are not value parameters.
                body = re.sub(r"\{[^{}]*\}", "", body)
                params = []
                for token in body.split():
                    if ":" not in token:
                        raise ValueError(f"Unsupported TL parameter in {name}: {token}")
                    param, kind = token.split(":", 1)
                    params.append({"name": param, "type": kind})
                if name in methods:
                    raise ValueError(f"Duplicate TL method: {name}")
                methods[name] = {"name": name, "constructor": constructor.lower().zfill(8),
                                 "params": params, "result": result,
                                 "declaration": declaration + ";", "schema": filename}
    return methods


def source_url(name: str) -> str:
    if not NAME.fullmatch(name):
        raise ValueError("Invalid Telegram method name")
    return f"https://core.telegram.org/method/{name}.md"


def parse_official(text: str, name: str) -> dict:
    """Recognize explicit, whole availability headings, never prose/namespace hints."""
    headings = {re.sub(r"[.!]+$", "", line.strip().lower()) for line in
                re.findall(r"^#{1,6}\s+(.+?)\s*$", text, re.M)}
    candidates = set()
    for heading, value in (("both users and bots can use this method", "both"),
                           ("only users can use this method", "users"),
                           ("only bots can use this method", "bots")):
        if heading in headings:
            candidates.add(value)
    constructors = {value.lower().zfill(8) for value in re.findall(
        rf"(?m)^{re.escape(name)}#([0-9a-fA-F]{{1,8}})\s", text)}
    return {"availability": next(iter(candidates)) if len(candidates) == 1 else "unknown",
            "official_constructor": next(iter(constructors)) if len(constructors) == 1 else None}


def load_cache(path: Path) -> dict:
    if not path.exists():
        return {"version": VERSION, "methods": {}}
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict) or data.get("version") != VERSION or not isinstance(data.get("methods"), dict):
        raise ValueError("Invalid Telegram method cache format")
    for name, record in data["methods"].items():
        if not isinstance(name, str) or not NAME.fullmatch(name) or not isinstance(record, dict):
            raise ValueError("Invalid Telegram method cache record")
        if (record.get("source") != source_url(name)
                or not isinstance(record.get("availability"), str)
                or record["availability"] not in AVAILABILITIES):
            raise ValueError(f"Invalid source/availability for {name}")
        for key in ("constructor", "official_constructor"):
            value = record.get(key)
            if (value is None and key == "official_constructor"):
                continue
            if not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{8}", value):
                raise ValueError(f"Invalid {key} for {name}")
        if not isinstance(record.get("retrieved_at"), str):
            raise ValueError(f"Missing retrieval time for {name}")
    return data


def fetch(method: dict) -> dict:
    url = source_url(method["name"])
    record = {"constructor": method["constructor"], "source": url,
              "retrieved_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
              "availability": "unknown", "official_constructor": None, "status": "unavailable"}
    for attempt in range(3):
        try:
            request = Request(url, headers={"User-Agent": "grammers-docs-cache/1.0 (public API reference; max 4 workers)",
                                           "Accept": "text/markdown, text/plain"})
            with urlopen(request, timeout=20) as response:
                body = response.read(2_000_001)
                if len(body) > 2_000_000:
                    raise ValueError("Documentation response exceeded size limit")
                # Redirects must stay on the public official documentation host.
                if not response.url.startswith("https://core.telegram.org/"):
                    raise ValueError("Documentation redirected outside the official host")
                text = body.decode("utf-8")
            record.update(parse_official(text, method["name"]))
            record["sha256"] = hashlib.sha256(body).hexdigest()
            record["status"] = "ok" if text.lstrip().startswith("#") else "unrecognized"
            if record["status"] != "ok":
                record["availability"] = "unknown"
            return record
        except HTTPError as error:
            record["status"] = f"http-{error.code}"
            if error.code not in (429, 500, 502, 503, 504):
                break
            retry_after = error.headers.get("Retry-After", "")
            if retry_after.isdigit():
                # Do not retry earlier than requested. Long server delays are left for a later run.
                if int(retry_after) > 60:
                    break
                delay = max(int(retry_after), 2 ** attempt)
            else:
                delay = 2 ** attempt
                if retry_after:
                    try:
                        requested = max(0, (parsedate_to_datetime(retry_after) - datetime.now(timezone.utc)).total_seconds())
                        if requested > 60:
                            break
                        delay = max(delay, requested)
                    except (TypeError, ValueError):
                        pass
        except (URLError, TimeoutError, UnicodeError, ValueError, OSError):
            record["status"] = "fetch-failed"
            delay = 2 ** attempt
        if attempt < 2:
            time.sleep(delay)
    return record


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--refresh", action="store_true", help="Refresh every cached method, including known entries")
    parser.add_argument("--method", action="append", help="Limit to named method(s)")
    parser.add_argument("--workers", type=int, default=4, choices=range(1, 5))
    parser.add_argument("--cache", type=Path, default=ROOT / "docs" / "telegram-methods.json")
    args = parser.parse_args()
    schema = read_schema(ROOT)
    cache = load_cache(args.cache)
    names = args.method or sorted(schema)
    if any(name not in schema for name in names):
        parser.error("Requested method is absent from the checked-in schema")
    pending = [name for name in names if args.refresh or name not in cache["methods"]
               or cache["methods"][name]["constructor"] != schema[name]["constructor"]]
    print(f"Refreshing {len(pending)} of {len(schema)} local methods (max {args.workers} requests at once).", flush=True)
    with ThreadPoolExecutor(max_workers=args.workers) as pool:
        work = {pool.submit(fetch, schema[name]): name for name in pending}
        for index, future in enumerate(as_completed(work), 1):
            name = work[future]
            record = future.result()
            previous = cache["methods"].get(name)
            # Transient refresh failures must not erase a successful same-constructor snapshot.
            if record["status"] != "ok" and previous and previous.get("status") == "ok" and previous["constructor"] == record["constructor"]:
                previous["last_refresh_status"] = record["status"]
                previous["last_refresh_attempt"] = record["retrieved_at"]
            else:
                cache["methods"][name] = record
            if index % 50 == 0 or index == len(pending):
                print(f"Fetched {index}/{len(pending)}", flush=True)
    cache["methods"] = {name: cache["methods"][name] for name in sorted(schema) if name in cache["methods"]}
    args.cache.parent.mkdir(parents=True, exist_ok=True)
    temporary = args.cache.with_suffix(".json.tmp")
    temporary.write_text(json.dumps(cache, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    temporary.replace(args.cache)
    counts = {key: sum(record["availability"] == key for record in cache["methods"].values()) for key in sorted(AVAILABILITIES)}
    print(json.dumps(counts, sort_keys=True))


if __name__ == "__main__":
    main()
