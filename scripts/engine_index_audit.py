#!/usr/bin/env python3
import argparse
import json
import os
import re
import sqlite3
from collections import Counter
from pathlib import Path


PLUGIN_PATTERN = re.compile(r"/Engine/Plugins/([^/]+(?:/[^/]+)?)")


def normalize(path: str) -> str:
    return path.replace("\\", "/")


def derive_engine_root(engine_db: Path, explicit_root: str | None) -> Path | None:
    if explicit_root:
        return Path(explicit_root)

    metadata_path = engine_db.with_name("metadata.json")
    if not metadata_path.is_file():
        return None

    try:
        data = json.loads(metadata_path.read_text(encoding="utf-8"))
    except Exception:
        return None

    engine_root = data.get("engine_root")
    if not engine_root:
        return None
    return Path(engine_root)


def collect_db_plugin_rows(conn: sqlite3.Connection) -> tuple[int, int, Counter, set[str]]:
    total_rows = conn.execute("SELECT COUNT(*) FROM search_symbols").fetchone()[0]
    plugin_rows = 0
    plugin_counter: Counter[str] = Counter()
    plugin_roots: set[str] = set()

    for (path,) in conn.execute(
        "SELECT path FROM search_symbols WHERE path LIKE '%/Engine/Plugins/%'"
    ):
        plugin_rows += 1
        match = PLUGIN_PATTERN.search(normalize(path))
        if not match:
            continue
        plugin_root = match.group(1)
        plugin_roots.add(plugin_root)
        plugin_counter[plugin_root] += 1

    return total_rows, plugin_rows, plugin_counter, plugin_roots


def collect_disk_plugins(engine_root: Path) -> set[str]:
    plugins_root = engine_root / "Engine" / "Plugins"
    plugin_roots: set[str] = set()
    if not plugins_root.is_dir():
        return plugin_roots

    for root, _, files in os.walk(plugins_root):
        if not any(name.lower().endswith(".uplugin") for name in files):
            continue
        rel = os.path.relpath(root, plugins_root).replace("\\", "/")
        plugin_roots.add(rel)

    return plugin_roots


def derive_asset_db(engine_db: Path) -> Path:
    return engine_db.with_name(f"{engine_db.stem}-asset.db")


def collect_asset_summary(asset_db: Path) -> dict | None:
    if not asset_db.is_file():
        return None

    conn = sqlite3.connect(asset_db)
    try:
        assets = conn.execute("SELECT COUNT(*) FROM assets").fetchone()[0]
        usages = conn.execute("SELECT COUNT(*) FROM search_asset_usages").fetchone()[0]
        version_row = conn.execute(
            "SELECT value FROM asset_meta WHERE key = 'db_version'"
        ).fetchone()
        return {
            "asset_db": str(asset_db),
            "db_version": version_row[0] if version_row else None,
            "assets": assets,
            "search_asset_usages": usages,
        }
    finally:
        conn.close()


def print_human_summary(summary: dict) -> None:
    print(f"engine_db: {summary['engine_db']}")
    print(f"engine_root: {summary['engine_root']}")
    print(f"total_search_symbols: {summary['total_search_symbols']}")
    print(f"plugin_symbol_rows: {summary['plugin_symbol_rows']}")
    print(f"distinct_plugins_in_db: {summary['distinct_plugins_in_db']}")
    print(f"distinct_plugins_on_disk: {summary['distinct_plugins_on_disk']}")
    print(f"missing_plugins_count: {summary['missing_plugins_count']}")
    print(f"extra_plugins_count: {summary['extra_plugins_count']}")
    print("top_plugins:")
    for name, count in summary["top_plugins"]:
        print(f"  {count:>7}  {name}")

    if summary["missing_plugins_sample"]:
        print("missing_plugins_sample:")
        for name in summary["missing_plugins_sample"]:
            print(f"  {name}")

    if summary["extra_plugins_sample"]:
        print("extra_plugins_sample:")
        for name in summary["extra_plugins_sample"]:
            print(f"  {name}")

    asset_summary = summary.get("asset_summary")
    if asset_summary:
        print("asset_summary:")
        print(
            f"  db_version={asset_summary['db_version']} "
            f"assets={asset_summary['assets']} "
            f"search_asset_usages={asset_summary['search_asset_usages']}"
        )
    else:
        print("asset_summary: missing")


def main() -> int:
    parser = argparse.ArgumentParser(description="Audit a live UCore engine index database.")
    parser.add_argument("engine_db", help="Path to engine.db")
    parser.add_argument(
        "--engine-root",
        help="Engine root path. If omitted, reads metadata.json next to engine.db.",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Print JSON instead of human-readable text.",
    )
    parser.add_argument(
        "--limit",
        type=int,
        default=20,
        help="Number of top plugins / sample rows to print.",
    )
    args = parser.parse_args()

    engine_db = Path(args.engine_db)
    if not engine_db.is_file():
        raise SystemExit(f"engine_db not found: {engine_db}")

    engine_root = derive_engine_root(engine_db, args.engine_root)
    if engine_root is None:
        raise SystemExit("engine_root not provided and metadata.json did not resolve it")

    conn = sqlite3.connect(engine_db)
    try:
        total_rows, plugin_rows, plugin_counter, db_plugins = collect_db_plugin_rows(conn)
    finally:
        conn.close()

    disk_plugins = collect_disk_plugins(engine_root)
    missing_plugins = sorted(disk_plugins - db_plugins)
    extra_plugins = sorted(db_plugins - disk_plugins)
    asset_summary = collect_asset_summary(derive_asset_db(engine_db))

    summary = {
        "engine_db": str(engine_db),
        "engine_root": str(engine_root),
        "total_search_symbols": total_rows,
        "plugin_symbol_rows": plugin_rows,
        "distinct_plugins_in_db": len(db_plugins),
        "distinct_plugins_on_disk": len(disk_plugins),
        "missing_plugins_count": len(missing_plugins),
        "extra_plugins_count": len(extra_plugins),
        "top_plugins": plugin_counter.most_common(args.limit),
        "missing_plugins_sample": missing_plugins[: args.limit],
        "extra_plugins_sample": extra_plugins[: args.limit],
        "asset_summary": asset_summary,
    }

    if args.json:
        print(json.dumps(summary, ensure_ascii=False, indent=2))
    else:
        print_human_summary(summary)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
