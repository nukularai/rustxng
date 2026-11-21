#!/usr/bin/env python3
"""Rebuild the offline ranking fixture from a recorded search run.

The recording holds, for each question, the merged top-10 page: every result
with the engines that returned it and the position each engine placed it at.
Grouping those records by engine reconstructs the per-engine response window
the merged page came from. Recorded positions are kept, including the gaps the
recording left; it never held the results an engine placed below the top 10.

This is a reconstruction, not a fresh capture, and it cannot recover anything
outside the recorded window. Its only purpose is a fixed, network-free input
for the ranking regression test.

The recording is checked in with the repository; run this script again to
rebuild the fixture byte for byte:

    python3 tools/build_quality_fixture.py
"""

from __future__ import annotations

import gzip
import hashlib
import json
import tomllib
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "bench/results/issue-resolution-2026-11-05/coverage/standard-1.json.gz"
OUTPUT = ROOT / "tests/fixtures/quality/engine-snapshots.json"
SETTINGS = ROOT / "settings.toml"

# Questions whose recorded snippets mention third-party search or cloud
# services. The repository keeps those names out of checked-in fixtures, so
# the reconstruction starts after them. The list is by stable question id.
EXCLUDED = {
    "en-attention",
    "en-bert",
    "en-k8s-deploy",
    "en-nginx-conf",
    "en-transformer",
    "ja-docker",
    "ja-gcp",
    "ja-rs-doc",
    "ja-rust-jp",
}

SNIPPET_CHARS = 300


def engine_weights() -> dict[str, float]:
    """The weight configured for each engine, 1.0 when the list does not set one."""
    settings = tomllib.loads(SETTINGS.read_text(encoding="utf-8"))
    weights: dict[str, float] = {}
    for entry in settings.get("engines", []):
        name = entry.get("name")
        if name:
            weights[name] = float(entry.get("weight", 1.0))
    return weights


def recorded_engines(row: dict) -> dict[str, list[dict]]:
    """Regroup the recorded results by the engine that returned them."""
    engines: dict[str, list[dict]] = defaultdict(list)
    for result in row["payload"]["results"]:
        names = result.get("engines") or [result.get("engine", "")]
        positions = result.get("positions") or [1]
        for name, position in zip(names, positions):
            engines[name].append(
                {
                    "position": int(position),
                    "url": str(result.get("url", "")),
                    "title": str(result.get("title", "")),
                    "content": str(result.get("content", ""))[:SNIPPET_CHARS],
                }
            )
    for results in engines.values():
        results.sort(key=lambda item: (item["position"], item["url"]))
    return dict(engines)


def main() -> int:
    payload = SOURCE.read_bytes()
    recording = json.loads(gzip.decompress(payload))
    # The baseline is measured by the Rust regression, not derived from the
    # recording; keep the values of an existing fixture across a rebuild.
    previous_baseline: dict = {}
    if OUTPUT.is_file():
        prior = json.loads(OUTPUT.read_text(encoding="utf-8"))
        previous_baseline = prior.get("baseline") or {}
    weights = engine_weights()
    queries = []
    languages: dict[str, int] = defaultdict(int)
    for row in recording["rows"]:
        if row["id"] in EXCLUDED:
            continue
        engines = recorded_engines(row)
        if not engines:
            continue
        languages[row["lang"]] += 1
        queries.append(
            {
                "id": row["id"],
                "lang": row["lang"],
                "query": row["query"],
                "gold": row["gold"],
                "engines": [
                    {
                        "name": name,
                        "weight": weights.get(name, 1.0),
                        "results": results,
                    }
                    for name, results in sorted(engines.items())
                ],
            }
        )

    fixture = {
        "version": 1,
        "description": (
            "Reconstructed per-engine response window of one recorded search run. "
            "Fixed input for the offline ranking regression; not a live capture and "
            "not a search-quality benchmark."
        ),
        "source": {
            "file": str(SOURCE.relative_to(ROOT)),
            "sha256": hashlib.sha256(payload).hexdigest(),
            "questions": len(queries),
            "languages": dict(sorted(languages.items())),
            "excluded_questions": sorted(EXCLUDED),
            "reconstruction": (
                "Each engine list holds the recorded (position, url, title, snippet) "
                "pairs of that engine. Only the merged top-10 page was recorded, so "
                "a list is a window, not a complete response."
            ),
        },
        "baseline": {
            "note": (
                "Measured with the pre-change configuration: the shipped relevance "
                "and authority terms, URL-only deduplication, and the two new "
                "ranking terms at 0. Update together with the test."
            ),
            "hit@1": previous_baseline.get("hit@1"),
            "hit@5": previous_baseline.get("hit@5"),
            "mrr@10": previous_baseline.get("mrr@10"),
        },
        "queries": queries,
    }
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    OUTPUT.write_text(
        json.dumps(fixture, ensure_ascii=False, indent=1) + "\n", encoding="utf-8"
    )
    print(f"wrote {OUTPUT.relative_to(ROOT)}: {len(queries)} questions, {OUTPUT.stat().st_size} bytes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
