#!/usr/bin/env python3
"""Drive `zone mcp` over real stdio: initialize, tools/list, every tool, the prompts, the
resources, and the agents switch (turned off and on again while the server runs).

Prints counts, shapes and timings only. It never prints item text, so it is safe to run
against a private library.

    ZONE_HOME=/some/temp/dir scripts/mcp_roundtrip.py --zone target/release/zone \
        --when "Wednesday 12pm to 5pm" --timezone America/Los_Angeles

ZONE_HOME must be set, so a run can never touch the real library by accident.
"""

import argparse
import json
import os
import select
import subprocess
import sys
import time


class Server:
    def __init__(self, zone, extra):
        self.proc = subprocess.Popen(
            [zone, "mcp", *extra], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True
        )
        self.next_id = 1
        self.notifications = []

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
            if "method" in message:
                self.notifications.append(message["method"])

    def wait_notification(self, method, seconds):
        """Waits for a notification the server sends by itself. Returns the seconds it took."""
        started = time.perf_counter()
        while method not in self.notifications:
            left = seconds - (time.perf_counter() - started)
            ready, _, _ = select.select([self.proc.stdout], [], [], max(left, 0))
            if not ready:
                return None
            message = json.loads(self.proc.stdout.readline())
            if "method" in message:
                self.notifications.append(message["method"])
        return time.perf_counter() - started

    def tool_raw(self, name, arguments):
        reply, _ = self.request("tools/call", {"name": name, "arguments": arguments})
        return reply["result"]

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
    parser.add_argument("--when", help="a window in words for list_items, e.g. \"Wednesday 12pm to 5pm\"")
    parser.add_argument("--start")
    parser.add_argument("--end")
    parser.add_argument("--timezone")
    parser.add_argument("--query", help="words for search_items (the words are not echoed)")
    parser.add_argument("--read-only", action="store_true")
    parser.add_argument("--keep-switch", action="store_true", help="do not turn agent access off and on")
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

    window = {key: value for key, value in (("when", args.when), ("start", args.start), ("end", args.end), ("timezone", args.timezone)) if value}

    status, ms, size = server.tool("sync_status", {})
    print(f"sync_status       {ms:7.1f} ms  now {status['now']['local']} ({status['now']['timezone']}), "
          f"{status['items']['total']} items {json.dumps(status['items']['by_kind'])}, {len(status['phones'])} phones")

    listed, ms, size = server.tool("list_items", {**window, "limit": 200})
    print(f"list_items        {ms:7.1f} ms  range {listed['range']['words']} ({listed['range']['start_iso']} to {listed['range']['end_iso']}), text {listed['text']}")
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

    prompts, ms = server.request("prompts/list")
    names = [prompt["name"] for prompt in prompts["result"]["prompts"]]
    print(f"prompts/list      {ms:7.1f} ms  {len(names)} prompts: {', '.join(names)}")
    if "what-did-i-say" in names:
        got, ms = server.request("prompts/get", {"name": "what-did-i-say", "arguments": {"day": "yesterday afternoon"}})
        text = got["result"]["messages"][0]["content"]["text"]
        print(f"prompts/get       {ms:7.1f} ms  what-did-i-say, {len(text)} chars, names when: {'when' in text}")
    missing, _ = server.request("prompts/get", {"name": "find-topic"})
    print(f"                              find-topic without its topic: error {missing.get('error', {}).get('code')}")

    listed_resources, ms = server.request("resources/list")
    uris = [r["uri"] for r in listed_resources["result"]["resources"]]
    templates, _ = server.request("resources/templates/list")
    print(f"resources/list    {ms:7.1f} ms  {', '.join(uris)}; templates {', '.join(t['uriTemplate'] for t in templates['result']['resourceTemplates'])}")
    for uri in uris + ["zone://day/yesterday"]:
        read, ms = server.request("resources/read", {"uri": uri})
        if "error" in read:
            print(f"resources/read    {ms:7.1f} ms  {uri}: error {read['error']['message']}")
        else:
            content = read["result"]["contents"][0]
            print(f"resources/read    {ms:7.1f} ms  {uri}: {content['mimeType']}, {len(content['text'])} chars")

    if not args.keep_switch:
        # The agents switch, flipped while this server runs.
        subprocess.run([args.zone, "agents", "off"], check=True, capture_output=True)
        waited = server.wait_notification("notifications/tools/list_changed", 10)
        print(f"zone agents off               list_changed after {waited:.1f} s" if waited is not None else "zone agents off               no list_changed within 10 s")
        tools_off, _ = server.request("tools/list")
        refused = server.tool_raw("list_items", {})
        offered = server.tool_raw("sync_status", {})
        prompts_off, _ = server.request("prompts/list")
        resources_off, _ = server.request("resources/list")
        print(f"                              tools {[t['name'] for t in tools_off['result']['tools']]}, list_items isError {refused.get('isError')}, "
              f"sync_status says {json.loads(offered['content'][0]['text'])['agent_access']}, prompts {len(prompts_off['result']['prompts'])}, resources {len(resources_off['result']['resources'])}")
        subprocess.run([args.zone, "agents", "on"], check=True, capture_output=True)
        server.notifications.clear()
        waited = server.wait_notification("notifications/tools/list_changed", 10)
        tools_on, _ = server.request("tools/list")
        print(f"zone agents on                list_changed after {waited:.1f} s, {len(tools_on['result']['tools'])} tools again" if waited is not None else "zone agents on                no list_changed")

    server.close()
    print("ok")


if __name__ == "__main__":
    sys.exit(main())
