# ドキュメント案内

RustXNG の文書は目的別に分かれています。初めての場合は README の Quick start から始めてください。

## 導入と運用

- [AGENT-SETUP.md](AGENT-SETUP.md): エージェントが自分で導入する場合の手順（リリースバイナリ、検証、起動、確認記録）
- [FAQ.md](FAQ.md): よくある質問と制約
- [COMPATIBILITY.md](COMPATIBILITY.md): SearXNG との互換範囲、拡張パラメータ、`/readyz`、検証方法
- [RDSH.md](RDSH.md): rdsh から使う場合の設定
- [OBSERVABILITY.md](OBSERVABILITY.md): `/metrics` の系列、エンジンの健康状態（休止・サーキット）、アラート例と復旧手順、`/healthz`・`/readyz` との使い分け
- [DEPENDENCIES.md](DEPENDENCIES.md): 依存のライセンス・禁止事項・取得元の検査（`deny.toml`）
- [LIGHTWEIGHT.md](LIGHTWEIGHT.md): 既定 / 最小 / 全 feature の比較と既定を変えない判断

## 検索・索引

- [LOCAL-INDEX.md](LOCAL-INDEX.md): 自前クロール索引の作成、定期巡回、検索、評価手順
- [WIKIPEDIA-INDEX.md](WIKIPEDIA-INDEX.md): Wikipedia ダンプ索引の取り込み
- [SEALQA.md](SEALQA.md): SealQA を用いたオフライン評価
- [ACCURACY.md](ACCURACY.md): 精度に関する注意

## プロバイダー

- [PROVIDERS.md](PROVIDERS.md): 既定で有効な検索元と確認した条件
- [CLEAN-PROFILE.md](CLEAN-PROFILE.md): 外部検索を行わない構成
- [provider-policy.json](provider-policy.json) / [provider-review-pending.json](provider-review-pending.json): 機械可読な台帳
- [audit/](audit/): 監査の生記録

## API とエージェント

- [openapi.yaml](openapi.yaml): JSON API の仕様（`tools/test_openapi.py` がコードとの不一致を検出）
- [CONTENT-API.md](CONTENT-API.md): `/map`・`/extract`・`/mcp`（サイト列挙、構造化抽出、HTTP の MCP）
- [AGENT-OUTPUT.md](AGENT-OUTPUT.md): MCP の出力契約とトークン予算
- [BENCHMARK-MULTILINGUAL.md](BENCHMARK-MULTILINGUAL.md): 多言語の品質評価セットと実行手順
- [RANKING-QUALITY.md](RANKING-QUALITY.md): ランキング項と重複排除の変更、既定値の根拠、オフライン回帰

## 性能と評価

- [BENCHMARK.md](BENCHMARK.md): 過去の測定記録と現行 CI の範囲
- [PERFORMANCE-2026-11-05.md](PERFORMANCE-2026-11-05.md): Standard 見直し時の測定
- [engineering/PERFORMANCE.md](engineering/PERFORMANCE.md): 2026-11-16 以降の実測（起動・メモリ・索引・抽出）

## 開発とエンジニアリング記録

- [CONTRIBUTING.md](../CONTRIBUTING.md): 貢献の手順
- [engineering/ROADMAP.md](engineering/ROADMAP.md): Phase 1〜5 の計画と達成状況
- [engineering/EXECUTION_STATE.md](engineering/EXECUTION_STATE.md): 現在の作業状態（再開用）
- [engineering/AUDIT.md](engineering/AUDIT.md): 旧監査の再判定と新規指摘
- [engineering/DECISIONS.md](engineering/DECISIONS.md): 設計判断（ADR）
- [engineering/RISKS.md](engineering/RISKS.md): 未解決事項と追跡項目
- [engineering/RELEASE-PLAN.md](engineering/RELEASE-PLAN.md): 未コミット変更のリリース単位
- [engineering/final-audit/](engineering/final-audit/): 独立監査向けの資料一式

## 移植の記録

- [PORTING.md](PORTING.md): 移植方針と差分
- [PORT-MIGRATION.md](PORT-MIGRATION.md): 設定の移行
- [COVERAGE.md](COVERAGE.md): エンジン網羅率
- [RELEASING.md](RELEASING.md): リリース手順
