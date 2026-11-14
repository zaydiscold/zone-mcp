#!/usr/bin/env python3
"""Drive `zone mcp` over real stdio: initialize, tools/list, then every tool.

Prints counts, shapes and timings only. It never prints item text, so it is safe to run
against a private library.

    ZONE_HOME=/some/temp/dir scripts/mcp_roundtrip.py --zone target/release/zone \
        --start "2026-11-11 12:00" --end "2026-11-11 17:00" --timezone America/Los_Angeles

ZONE_HOME must be set, so a run can never touch the real library by accident.
"""

import argparse
import json
import os
import subprocess
import sys
import time


class Server:
    def __init__(self, zone, extra):
        self.proc = subprocess.Popen(
            [zone, "mcp", *extra], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True
        )
        self.next_id = 1

    def send(self, message):
        self.proc.stdin.write(json.dumps(message) + "\n")
        self.proc.stdin.flush()

    def request(self, method, params=None):
        ident = self.next_id
        self.next_id += 1
        started = time.perf_counter()
        self.send({"jsonrpc": "2.0", "id": ident, "method": method, "params": params or {}})
        while True:
            line = self.proc.stdout.readline()
            if not line:
                raise SystemExit("zone mcp closed its output")
            message = json.loads(line)  # anything that is not JSON-RPC on stdout fails here
            if message.get("id") == ident:
                return message, (time.perf_counter() - started) * 1000

    def tool(self, name, arguments):
        reply, ms = self.request("tools/call", {"name": name, "arguments": arguments})
        if "error" in reply:
            raise SystemExit(f"{name}: protocol error {reply['error']}")
        result = reply["result"]
        text = result["content"][0]["text"]
        if result.get("isError"):
            raise SystemExit(f"{name}: tool error: {text}")
        return json.loads(text), ms, len(text)

    def close(self):
        self.proc.stdin.close()
        self.proc.wait(timeout=10)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--zone", default="target/release/zone")
    parser.add_argument("--start")
    parser.add_argument("--end")
    parser.add_argument("--timezone")
    parser.add_argument("--query", help="words for search_items (the words are not echoed)")
    parser.add_argument("--read-only", action="store_true")
    args = parser.parse_args()
    if not os.environ.get("ZONE_HOME"):
        raise SystemExit("set ZONE_HOME to a temporary directory first")

    server = Server(args.zone, ["--read-only"] if args.read_only else [])
    reply, ms = server.request(
        "initialize",
        {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "mcp_roundtrip.py", "version": "1"}},
    )
    info = reply["result"]
    print(f"initialize        {ms:7.1f} ms  protocol {info['protocolVersion']}, server {info['serverInfo']['name']} "
          f"{info['serverInfo']['version']}, instructions {len(info.get('instructions', ''))} chars")
    server.send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    reply, ms = server.request("tools/list")
    tools = reply["result"]["tools"]
    print(f"tools/list        {ms:7.1f} ms  {len(tools)} tools")
    for tool in tools:
        hint = "read-only" if tool["annotations"].get("readOnlyHint") else "writes a flag"
        print(f"                              {tool['name']:15} {hint}")

    window = {key: value for key, value in (("start", args.start), ("end", args.end), ("timezone", args.timezone)) if value}

    status, ms, size = server.tool("sync_status", {})
    print(f"sync_status       {ms:7.1f} ms  now {status['now']['local']} ({status['now']['timezone']}), "
          f"{status['items']['total']} items {json.dumps(status['items']['by_kind'])}, {len(status['phones'])} phones")

    listed, ms, size = server.tool("list_items", {**window, "limit": 200})
    print(f"list_items        {ms:7.1f} ms  range {listed['range']['start']} to {listed['range']['end']}")
    print(f"                              total {listed['total']}, by kind {json.dumps(listed['total_by_kind'])}, "
          f"returned {listed['returned']}, next_cursor {'yes' if listed['next_cursor'] else 'none'}, {size} bytes")
    ids = [item["id"] for item in listed["items"]]

    paged, ms, _ = server.tool("list_items", {**window, "limit": 5, "order": "oldest"})
    seen = len(paged["items"])
    while paged["next_cursor"]:
        paged, _, _ = server.tool("list_items", {**window, "limit": 5, "order": "oldest", "cursor": paged["next_cursor"]})
        seen += len(paged["items"])
    print(f"list_items paged              walked {seen} items five at a time, oldest first (matches total: {seen == listed['total']})")

    if args.query:
        found, ms, size = server.tool("search_items", {"query": args.query, **window, "limit": 5})
        print(f"search_items      {ms:7.1f} ms  total {found['total']}, returned {found['returned']}, truncated {found['truncated']}, {size} bytes")
    else:
        print("search_items                  skipped (no --query)")

    if ids:
        got, ms, size = server.tool("get_items", {"ids": ids[:3], "max_chars": 500})
        shapes = [(item["kind"], item["total_chars"], item["truncated"]) for item in got["items"]]
        print(f"get_items         {ms:7.1f} ms  {len(got['items'])} items (kind, chars, truncated): {shapes}, not_found {len(got['not_found'])}")

    stats, ms, _ = server.tool("library_stats", {**window, "group_by": "kind"})
    groups = {group["kind"]: group["items"] for group in stats["groups"]}
    print(f"library_stats     {ms:7.1f} ms  total {stats['total']['items']} items, {stats['total']['words']} words, by kind {json.dumps(groups)}")
    days, ms, _ = server.tool("library_stats", {**window, "group_by": "day"})
    print(f"library_stats day {ms:7.1f} ms  {len(days['groups'])} days")

    if not args.read_only and ids:
        marked, ms, _ = server.tool("mark_processed", {"ids": ids[:2]})
        after, _, _ = server.tool("list_items", {**window, "unprocessed_only": True, "limit": 1})
        print(f"mark_processed    {ms:7.1f} ms  updated {marked['updated']}, unprocessed in range now {after['total']} of {listed['total']}")
        server.tool("mark_processed", {"ids": ids[:2], "processed": False})
        restored, _, _ = server.tool("list_items", {**window, "unprocessed_only": True, "limit": 1})
        print(f"                              cleared again, unprocessed in range {restored['total']}")

    server.close()
    print("ok")


if __name__ == "__main__":
    sys.exit(main())
