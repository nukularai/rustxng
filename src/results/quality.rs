//! Offline ranking regression over a fixed per-engine snapshot.
//!
//! The fixture is a recorded run regrouped per engine (see
//! `tools/build_quality_fixture.py` and `tests/fixtures/quality/README.md`).
//! Nothing here contacts a search provider, so the numbers are a regression
//! signal for the ranker, not a measurement of live search quality.
//!
//! Run with `cargo test --lib results::quality -- --nocapture` to print the
//! before/after table.

use std::path::Path;

use serde::Deserialize;

use super::{DedupPolicy, EngineItem, RankingOptions, ResultContainer, SearchResult, query_terms};
use crate::settings::Settings;

const FIXTURE: &str = "tests/fixtures/quality/engine-snapshots.json";

#[derive(Deserialize)]
struct Fixture {
    version: u32,
    source: Source,
    baseline: Baseline,
    queries: Vec<FixtureQuery>,
}

#[derive(Deserialize)]
struct Source {
    file: String,
    sha256: String,
    questions: usize,
    languages: std::collections::BTreeMap<String, usize>,
}

#[derive(Deserialize)]
struct Baseline {
    #[serde(rename = "hit@1")]
    hit1: Option<usize>,
    #[serde(rename = "hit@5")]
    hit5: Option<usize>,
    #[serde(rename = "mrr@10")]
    mrr: Option<f64>,
}

#[derive(Deserialize)]
struct FixtureQuery {
    id: String,
    lang: String,
    query: String,
    gold: Vec<String>,
    engines: Vec<FixtureEngine>,
}

#[derive(Deserialize)]
struct FixtureEngine {
    name: String,
    weight: f64,
    results: Vec<FixtureResult>,
}

#[derive(Deserialize)]
struct FixtureResult {
    position: u32,
    url: String,
    title: String,
    content: String,
}

#[derive(Debug, Clone, Copy)]
struct Config {
    url_variants: bool,
    near_duplicates: bool,
    relevance: f64,
    authority: f64,
    consensus: f64,
    text_match: f64,
}

impl Config {
    /// The behaviour before this change: the shipped relevance and authority
    /// terms, URL-only deduplication, and no consensus or title/snippet term.
    fn baseline() -> Self {
        let search = Settings::defaults().search;
        Self {
            url_variants: false,
            near_duplicates: false,
            relevance: search.relevance_weight,
            authority: search.authority_weight,
            consensus: 0.0,
            text_match: 0.0,
        }
    }

    fn shipped() -> Self {
        let search = Settings::defaults().search;
        Self {
            url_variants: search.dedup_fuzzy,
            near_duplicates: search.dedup_fuzzy,
            relevance: search.relevance_weight,
            authority: search.authority_weight,
            consensus: search.consensus_weight,
            text_match: search.text_match_weight,
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct Metrics {
    questions: usize,
    hit1: usize,
    hit5: usize,
    mrr: f64,
}

fn fixture() -> Fixture {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

/// All questions plus the same numbers split by the question language,
/// mirroring the `--by-lang` view of tools/eval_quality.py.
#[derive(Debug, Default, Clone)]
struct Run {
    all: Metrics,
    by_language: std::collections::BTreeMap<String, Metrics>,
}

fn tally(metrics: &mut Metrics, rank: Option<usize>) {
    metrics.questions += 1;
    if let Some(rank) = rank {
        if rank == 1 {
            metrics.hit1 += 1;
        }
        if rank <= 5 {
            metrics.hit5 += 1;
        }
        metrics.mrr += 1.0 / rank as f64;
    }
}

fn finish(metrics: &mut Metrics) {
    metrics.mrr /= metrics.questions.max(1) as f64;
}

fn run(fixture: &Fixture, config: Config) -> Run {
    let mut run = Run::default();
    for query in &fixture.queries {
        let mut container = ResultContainer::new();
        container.set_dedup(DedupPolicy {
            url_variants: config.url_variants,
            near_duplicates: config.near_duplicates,
        });
        for engine in &query.engines {
            let items: Vec<(u32, EngineItem)> = engine
                .results
                .iter()
                .map(|result| {
                    (
                        result.position,
                        EngineItem::result(SearchResult {
                            url: result.url.clone(),
                            title: result.title.clone(),
                            content: result.content.clone(),
                            ..SearchResult::default()
                        }),
                    )
                })
                .collect();
            container.extend_at_positions(&engine.name, engine.weight, items);
        }
        let terms = query_terms(&query.query);
        container.close_with(
            |_| 1.0,
            &terms,
            &query.query,
            &RankingOptions {
                relevance: config.relevance,
                authority: config.authority,
                consensus: config.consensus,
                text_match: config.text_match,
            },
        );
        let urls: Vec<&str> = container
            .ordered()
            .iter()
            .map(|result| result.url.as_str())
            .collect();
        let rank = first_hit_rank(&urls, &query.gold);
        tally(&mut run.all, rank);
        tally(run.by_language.entry(query.lang.clone()).or_default(), rank);
    }
    finish(&mut run.all);
    for metrics in run.by_language.values_mut() {
        finish(metrics);
    }
    run
}

// ---------------------------------------------------------------------------
// Reference URL matching, mirroring tools/reference_urls.py. Gold entries are
// prefixes at a path boundary; scheme, fragment and version segments do not
// count; an extensionless gold also accepts its .html/.htm page.
// ---------------------------------------------------------------------------

fn normalize_path(path: &str) -> Option<String> {
    let decoded = percent_encoding::percent_decode_str(path)
        .decode_utf8()
        .ok()?
        .to_string();
    let mut segments = Vec::new();
    for segment in decoded.split('/') {
        if segment.is_empty() {
            continue;
        }
        if segment == "." || segment == ".." {
            return None;
        }
        segments.push(if is_version_segment(segment) {
            "*".to_string()
        } else {
            segment.to_string()
        });
    }
    Some(segments.join("/"))
}

fn is_version_segment(segment: &str) -> bool {
    if matches!(segment, "stable" | "current" | "latest") {
        return true;
    }
    let digits = segment.strip_prefix('v').unwrap_or(segment);
    !digits.is_empty()
        && digits
            .split('.')
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

fn url_parts(value: &str) -> Option<(String, Option<u16>, String, String)> {
    let parsed = url::Url::parse(value).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return None;
    }
    let host = parsed.host_str()?.to_ascii_lowercase();
    let port = parsed.port().filter(|port| *port != 80 && *port != 443);
    let path = normalize_path(parsed.path())?;
    Some((host, port, path, parsed.query().unwrap_or("").to_string()))
}

fn matches_reference(candidate: &str, reference: &str) -> bool {
    let (Some(found), Some(gold)) = (url_parts(candidate), url_parts(reference)) else {
        return false;
    };
    if found.0 != gold.0 || found.1 != gold.1 {
        return false;
    }
    if !gold.3.is_empty() && found.3 != gold.3 {
        return false;
    }
    let (path, prefix) = (found.2, gold.2);
    path == prefix
        || path.starts_with(&format!("{prefix}/"))
        || path == format!("{prefix}.html")
        || path == format!("{prefix}.htm")
}

fn first_hit_rank(urls: &[&str], references: &[String]) -> Option<usize> {
    urls.iter()
        .position(|url| {
            references
                .iter()
                .any(|reference| matches_reference(url, reference))
        })
        .map(|index| index + 1)
}

#[test]
fn the_fixture_describes_the_recorded_run() {
    let fixture = fixture();
    assert_eq!(fixture.version, 1);
    assert!(fixture.source.file.ends_with(".json.gz"));
    assert_eq!(fixture.source.sha256.len(), 64);
    assert_eq!(fixture.queries.len(), fixture.source.questions);
    assert!(
        fixture.source.questions > 80,
        "the snapshot keeps most questions"
    );
    assert!(fixture.source.languages.contains_key("en"));
    assert!(fixture.source.languages.contains_key("ja"));
    let ids: std::collections::BTreeSet<&str> = fixture
        .queries
        .iter()
        .map(|query| query.id.as_str())
        .collect();
    assert_eq!(
        &ids.len(),
        &fixture.queries.len(),
        "question ids are unique"
    );
}

#[test]
fn the_reference_matcher_follows_the_documented_rules() {
    let gold = vec!["https://docs.e.test/3/library/asyncio".to_string()];
    assert_eq!(
        first_hit_rank(&["https://docs.e.test/3/library/asyncio.html"], &gold),
        Some(1)
    );
    assert_eq!(
        first_hit_rank(&["https://docs.e.test/stable/library/asyncio"], &gold),
        Some(1)
    );
    assert_eq!(
        first_hit_rank(&["https://docs.e.test/3/library/asyncio-old"], &gold),
        None
    );
    assert_eq!(
        first_hit_rank(&["https://other.e.test/3/library/asyncio.html"], &gold),
        None
    );
    let with_query = vec!["https://e.test/watch?v=1".to_string()];
    assert_eq!(
        first_hit_rank(&["https://e.test/watch?v=1"], &with_query),
        Some(1)
    );
    assert_eq!(
        first_hit_rank(&["https://e.test/watch?v=2"], &with_query),
        None
    );
}

#[test]
fn the_baseline_reproduces_the_recorded_before_metrics() {
    let fixture = fixture();
    let run = run(&fixture, Config::baseline());
    println!("baseline {run:?}");
    if let (Some(hit1), Some(hit5), Some(mrr)) = (
        fixture.baseline.hit1,
        fixture.baseline.hit5,
        fixture.baseline.mrr,
    ) {
        assert_eq!(
            run.all.hit1, hit1,
            "baseline hit@1 moved; update the fixture only with a measured change"
        );
        assert_eq!(run.all.hit5, hit5, "baseline hit@5 moved");
        assert!(
            (run.all.mrr - mrr).abs() < 1e-9,
            "baseline MRR moved: {} vs {}",
            run.all.mrr,
            mrr
        );
    }
}

#[test]
fn the_shipped_defaults_do_not_lower_the_recorded_snapshot() {
    let fixture = fixture();
    let baseline = run(&fixture, Config::baseline());
    let shipped = run(&fixture, Config::shipped());
    println!(
        "offline fixture (reconstructed recording, not live quality): {} questions",
        baseline.all.questions
    );
    for (lang, before) in &baseline.by_language {
        let after = &shipped.by_language[lang];
        println!(
            "  {lang}: before hit@1={}/{} hit@5={}/{} mrr@10={:.4} | \
             after hit@1={}/{} hit@5={}/{} mrr@10={:.4}",
            before.hit1,
            before.questions,
            before.hit5,
            before.questions,
            before.mrr,
            after.hit1,
            after.questions,
            after.hit5,
            after.questions,
            after.mrr,
        );
        assert!(
            after.hit1 >= before.hit1,
            "{lang} hit@1 fell: {} -> {}",
            before.hit1,
            after.hit1
        );
        assert!(
            after.hit5 >= before.hit5,
            "{lang} hit@5 fell: {} -> {}",
            before.hit5,
            after.hit5
        );
        assert!(
            after.mrr >= before.mrr - 1e-9,
            "{lang} MRR fell: {} -> {}",
            before.mrr,
            after.mrr
        );
    }
    assert!(
        shipped.all.hit1 >= baseline.all.hit1,
        "hit@1 fell: {} -> {}",
        baseline.all.hit1,
        shipped.all.hit1
    );
    assert!(
        shipped.all.hit5 >= baseline.all.hit5,
        "hit@5 fell: {} -> {}",
        baseline.all.hit5,
        shipped.all.hit5
    );
    assert!(
        shipped.all.mrr >= baseline.all.mrr - 1e-9,
        "MRR fell: {} -> {}",
        baseline.all.mrr,
        shipped.all.mrr
    );
}
