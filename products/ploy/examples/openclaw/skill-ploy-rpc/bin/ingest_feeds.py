#!/usr/bin/env python3
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import sys
import urllib.request
import xml.etree.ElementTree as ET


def _text(el: ET.Element | None) -> str | None:
    if el is None or el.text is None:
        return None
    s = el.text.strip()
    return s or None


def _first(el: ET.Element, tags: list[str]) -> ET.Element | None:
    for t in tags:
        child = el.find(t)
        if child is not None:
            return child
    return None


def _safe_id(*parts: str) -> str:
    raw = "||".join([p for p in parts if p]).encode("utf-8", "ignore")
    return hashlib.sha256(raw).hexdigest()[:24]


def _parse_date(s: str | None) -> str | None:
    if not s:
        return None
    s = s.strip()
    for fmt in (
        "%a, %d %b %Y %H:%M:%S %z",  # RFC 2822
        "%a, %d %b %Y %H:%M:%S %Z",
        "%Y-%m-%dT%H:%M:%S%z",  # ISO-ish
        "%Y-%m-%dT%H:%M:%SZ",
        "%Y-%m-%dT%H:%M:%S.%fZ",
    ):
        try:
            d = dt.datetime.strptime(s, fmt)
            if d.tzinfo is None:
                d = d.replace(tzinfo=dt.timezone.utc)
            return d.astimezone(dt.timezone.utc).isoformat()
        except ValueError:
            pass
    return None


def _fetch_xml(url: str, timeout: int) -> ET.Element:
    req = urllib.request.Request(
        url,
        headers={
            "User-Agent": "ploy-openclaw-feed-ingest/1.0 (+https://github.com/; contact: local)",
            "Accept": "application/rss+xml, application/atom+xml, application/xml, text/xml, */*",
        },
        method="GET",
    )
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        data = resp.read()
    return ET.fromstring(data)


def parse_feed(url: str, source: str | None, limit: int, timeout: int) -> list[dict]:
    root = _fetch_xml(url, timeout=timeout)

    items: list[dict] = []

    # RSS: <rss><channel><item>...
    channel = root.find("channel")
    if channel is not None:
        for item in channel.findall("item")[:limit]:
            title = _text(item.find("title")) or ""
            link = _text(item.find("link")) or ""
            guid = _text(item.find("guid")) or ""
            published = _parse_date(_text(item.find("pubDate")) or _text(item.find("date")))
            item_id = _safe_id(url, guid or link or title)
            items.append(
                {
                    "id": item_id,
                    "source": source or url,
                    "title": title,
                    "link": link,
                    "published": published,
                }
            )
        return items

    # Atom: <feed><entry>...
    # Try to tolerate namespaces by scanning for localname "entry".
    def is_entry(el: ET.Element) -> bool:
        return el.tag.endswith("entry")

    entries = [el for el in root.iter() if is_entry(el)]
    for entry in entries[:limit]:
        title_el = _first(entry, ["title", "{http://www.w3.org/2005/Atom}title"])
        title = _text(title_el) or ""

        link = ""
        for child in list(entry):
            if child.tag.endswith("link"):
                href = child.attrib.get("href", "").strip()
                if href:
                    link = href
                    break

        id_el = _first(entry, ["id", "{http://www.w3.org/2005/Atom}id"])
        guid = _text(id_el) or ""

        published_el = _first(
            entry,
            ["published", "updated", "{http://www.w3.org/2005/Atom}published", "{http://www.w3.org/2005/Atom}updated"],
        )
        published = _parse_date(_text(published_el))

        item_id = _safe_id(url, guid or link or title)
        items.append(
            {
                "id": item_id,
                "source": source or url,
                "title": title,
                "link": link,
                "published": published,
            }
        )

    return items


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--config", required=True, help="JSON file: { feeds: [{url, source?, limit?}, ...] }")
    ap.add_argument("--timeout", type=int, default=15)
    ap.add_argument("--default-limit", type=int, default=20)
    ap.add_argument("--state", help="State JSON to dedupe previously seen ids")
    args = ap.parse_args()

    cfg = json.loads(open(args.config, "r", encoding="utf-8").read())
    feeds = cfg.get("feeds", [])
    if not isinstance(feeds, list) or not feeds:
        print(json.dumps({"ok": False, "error": "config missing feeds[]"}))
        return 2

    seen: set[str] = set()
    if args.state:
        try:
            st = json.loads(open(args.state, "r", encoding="utf-8").read())
            for x in st.get("seen_ids", []):
                if isinstance(x, str):
                    seen.add(x)
        except FileNotFoundError:
            pass

    out_items: list[dict] = []
    new_ids: list[str] = []

    for f in feeds:
        if not isinstance(f, dict) or "url" not in f:
            continue
        url = str(f["url"])
        source = f.get("source")
        limit = int(f.get("limit") or args.default_limit)
        try:
            items = parse_feed(url, source=source, limit=limit, timeout=args.timeout)
        except Exception as e:
            out_items.append(
                {
                    "id": _safe_id(url, str(e)),
                    "source": source or url,
                    "title": "",
                    "link": "",
                    "published": None,
                    "error": str(e),
                }
            )
            continue

        for it in items:
            item_id = it.get("id", "")
            if not item_id or item_id in seen:
                continue
            out_items.append(it)
            new_ids.append(item_id)

    # Save updated state
    if args.state:
        merged = list(seen.union(new_ids))
        state = {
            "updated_at": dt.datetime.now(tz=dt.timezone.utc).isoformat(),
            "seen_ids": merged[-5000:],  # cap
        }
        with open(args.state, "w", encoding="utf-8") as fp:
            fp.write(json.dumps(state, indent=2, ensure_ascii=False) + "\n")

    payload = {
        "ok": True,
        "count": len(out_items),
        "items": out_items,
    }
    print(json.dumps(payload, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
