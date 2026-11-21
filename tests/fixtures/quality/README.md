# Offline ranking fixture

`engine-snapshots.json` is a fixed, network-free input for the ranking
regression in `src/results/quality.rs`. It is not a live capture and it is not
a search-quality benchmark.

## Where it comes from

The fixture is reconstructed from a recorded search run that is checked in at
`bench/results/issue-resolution-2026-11-05/coverage/standard-1.json.gz`
(SHA-256 recorded in the fixture). Each recorded row holds the merged top-10
page of one question: URL, title, snippet, the engines that returned each
result, and the position each of those engines gave it. Grouping the results by
engine reconstructs the per-engine response window the merged page came from.

The reconstruction keeps the recorded positions, including gaps: a result an
engine placed at 7 stays at 7. It cannot recover results the engine placed
below the merged top 10, so every list is a window, not a complete response.

Nine questions are left out because their recorded snippets mention third-party
search or cloud services, and this repository keeps those names out of shipped
fixtures. They are listed by question id under `source.excluded_questions`.
The remaining 91 questions are all English and Japanese; the multilingual
60-question set has never been run with per-engine capture, so no Korean
question appears here.

## What the test measures

`src/results/quality.rs` replays the fixture through `ResultContainer` twice:
once with the pre-change configuration (URL-only deduplication, the new ranking
terms at 0) and once with the shipped defaults. It reports Hit@1, Hit@5 and
MRR@10 overall and per language, and asserts that the defaults do not lower any
of them. The recorded `baseline` block holds the measured before numbers; a
change that moves them must update it together with the test.

The numbers are a regression signal for the ranker on one reconstructed
recording. They do not measure live provider quality and must not be quoted as
such.

## Rebuild

```bash
python3 tools/build_quality_fixture.py
cargo test --lib results::quality -- --nocapture
```

The builder is deterministic and keeps an existing `baseline` block.
