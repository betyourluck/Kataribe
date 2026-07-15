# 13. Gemini 明示キャッシュ (cachedContent) — 暗黙キャッシュの ~8000 崖を迂回する

Status: **Phase A+B+C Done + D 未着手 (2026-07-15)。** Phase A = encode 分割 + fingerprint の純
リファクタ (回帰ゼロ)、Phase B = cachedContents create/参照 + lifecycle (client に wire、fake なし
純ロジックを PoC)、Phase C = 台帳追従 (data_contract `gemini_explicit_cache` / CLAUDE.md spec-13
bullet / .env.example の env 3つ / failures #54 の解ポインタ)。llm_client 40→44 PoC green・clippy
clean・app backend check green。残 = **Phase D (実キー live 検証)** のみ。
動機 (実測): **failures #54** — `gemini-flash-latest`(=gemini-3.5-flash) の**暗黙キャッシュは
総プロンプト ~8000 tokens を超えると停止**する (以下なら ~4074=GM_SYSTEM prefix のみ cache、
超えると `cached=0`)。制御実験 (friday の world 段階パディング + user 側パディング) で
「因果は総プロンプトサイズ・sysInstr でも package 内容でもない」を単離済み。**大シナリオ
(sun_girl_ntr は turn1 から超過) / 長セッション (chronicle 蓄積で誰でも ~8000 超) 全般で不発**。
Kataribe の静的プレフィックス (systemInstruction + tools) は**構造上キャッシュ可能** (毎ターン
1 文字も変わらないと実測) なのに、Gemini の best-effort 暗黙キャッシュが気まぐれで拾わない。
→ **明示 cachedContent で静的プレフィックスを決定論的に pin し、暗黙の閾値に無関係にヒットさせる。**

Scope: `crates/llm_client` の **Gemini adapter に cachedContent の create / reference / TTL /
invalidation を足す**。**Anthropic は既に明示キャッシュ済** (system 末尾 `cache_control:ephemeral`、
#44) — この spec の Gemini 版はその双対。**OpenAI 互換 / Grok は対象外** (明示キャッシュ API を
持たない。sticky routing #45 が既に対処)。**harness / CLI / app は原則無改修** — cache lifecycle
は `LlmClient` 内に閉じる (`conv_id` #45 と同じ「client がセッション状態を持つ」流儀)。

## 用語

- **cachedContent** = Gemini の明示キャッシュリソース。`POST /v1beta/cachedContents` で
  `{model, systemInstruction, tools, ttl}` を登録すると `name: "cachedContents/<id>"` が返る。
  以後 `generateContent` に `cachedContent: <name>` を載せると、キャッシュ済みトークンが
  前置され**割引課金**される (storage は token×時間で別課金)。
- **静的プレフィックス** = Kataribe の場合 `systemInstruction`(=GM_SYSTEM + scenario_brief) +
  `tools`(=emit_delta schema)。scenario 単位で不変 (spec 12 で確認、turn.rs はこれを system
  1 メッセージに、可変値=state_brief/chronicle/… は全て user メッセージに置く)。
- **暗黙キャッシュ** = Gemini 2.5/3.x の自動キャッシュ (best-effort、~8000 崖、failures #54)。
  明示とは別機構 — 明示は**閾値・崖に無関係**にヒットする (登録した resource ゆえ)。

## 目的 (なぜ今 — failures #54 の実測)

1. **大シナリオ・長セッションのコスト/レイテンシを下げる** — 暗黙が効かない領域こそ
   キャッシュの旨みが最大 (プレフィックスが大きい)。sun_girl_ntr は systemInstruction 9855 文字
   (~6500 tokens) が毎ターン full 課金されている。明示で pin すれば ~6500 tokens/ターンが割引に。
2. **同人配布の北極星に直結** — 受領者が安いモデル (Gemini flash) で長く遊ぶほど累積コストが
   効く。作者が何も設定しなくても効く**ゼロ設定**が理想 (`Provider::detect` の自動判定と同じ精神)。
3. **spec 12 の前提を接地し直す** — 「2.5 系は暗黙自動ゆえ明示不要」は 3.5-flash で反証された
   (failures #54)。この spec がその訂正の実体。

## 問題 (暗黙キャッシュは大プロンプトで無言で 0)

`gemini::encode` は毎ターン systemInstruction + tools + contents を組んで送る。static な
systemInstruction+tools は暗黙キャッシュに載る**はず**だが、総プロンプトが ~8000 を超えると
Gemini が拾わなくなる (failures #54、best-effort の未文書挙動)。しかも効いても ~4074
(GM_SYSTEM のみ) で scenario_brief 部分は元から未キャッシュ。**「静的なのにキャッシュされない
トークン」が大シナリオほど積み上がる** — これは暗黙の best-effort に依存する限り構造的に直らない。

## 決定 (設計の核)

**採用: cachedContent を `LlmClient` 内でセッション単位に lazy 作成し、systemInstruction+tools を
pin する。generateContent は cache を参照して可変 contents だけ送る。**

### D1 — 分割: 静的は cache へ、可変は request へ

`gemini::encode` は既に systemInstruction (静的) と contents (可変 user) を分離している。
明示キャッシュ版はこの境界をそのまま使う:
- **cachedContent 作成時**の body = `{model, systemInstruction, tools}` (静的プレフィックス)。
- **generateContent 送信時**の body = `{contents, cachedContent: <name>, toolConfig, generationConfig}`
  — systemInstruction / tools は**送らない** (cache が持つ)。`toolConfig`(functionCallingConfig
  mode:ANY) は**request 側に残す** (per-request の強制指定であってツール宣言ではない)。

### D2 — lifecycle: lazy 作成 + キー照合 + TTL 再作成 (client 内に閉じる)

`LlmClient` に `gemini_cache: Mutex<Option<CacheHandle>>` を足す (`conv_id`/`cache_stat` と同じ流儀)。
`CacheHandle { name: String, key: u64, expire_hint: ... }`。毎 Gemini リクエストで:
1. 現リクエストの静的プレフィックス (systemInstruction+tools) の **fingerprint** (安定ハッシュ) を計算。
2. `gemini_cache` が同じ key を持てば **reuse** (name を参照)。
3. key が違う (scenario 変化=campaign 遷移 / dev-mode トグル / モデル変更) or None なら **作成**して差し替え。
4. cache が失効 (generateContent が `cachedContent` 不在で 4xx) したら **一度だけ再作成してリトライ**
   (`EmptyResponse` の一過性昇格と同型の透過リトライ)。

### D3 — サイズゲート + fallback (ゼロ設定で安全に)

明示キャッシュには**最小トークン数**がある (モデル依存、~1024〜4096)。静的プレフィックスが
最小未満なら作成が 400 で失敗する。よって:
- **サイズゲート**: 静的プレフィックスの推定トークンが閾値未満なら**作成を試みない** (暗黙のまま)。
- **fallback**: 作成が何らかの理由で失敗したら**現行の full request にフォールバック** (turn は
  絶対に落とさない)。キャッシュは最適化であって正しさの前提ではない — 三権分立の正本に触れない。
- **既定 on / opt-out** (`LLM_GEMINI_CACHE=0` で無効化)。北極星のゼロ設定 (受領者は何もしない) を
  優先しつつ、storage コストを嫌う作者は切れる。**注**: 明示キャッシュは storage 課金 (token×時間)
  があるので、超短時間セッションでは損益分岐を割る — サイズゲート + TTL 短め (既定 ~10 分?) で抑える。

### D4 — 計測は流用 (新資産ゼロ)

明示キャッシュのヒットも `usageMetadata.cachedContentTokenCount` に出る (暗黙と同じフィールド)。
`gemini::decode` は既にこれを `Usage.cache_read` へ写している → `CacheStat` / `[LLM_CACHE]` /
GUI ⚠ (#44/#45 の健全性警告) が**改修ゼロで明示キャッシュにも効く**。live 検証もこの数字で行う。

### D5 — Anthropic との対称性 (adapter 内に閉じる、spec 12 D5)

Anthropic は encode で system 末尾に cache_control を差すだけ (無状態)。Gemini は cache が
**別リソース (create API + name)** ゆえ**状態を持つ** — が、その状態は `LlmClient` 内
(`gemini_cache`) に閉じ、harness/app からは不可視 (conv_id と同じ)。adapter の外に漏らさない。

### 却下した代替案

- **static プレフィックスを user 側へ移す**: 総プロンプトサイズは不変ゆえ ~8000 崖は動かない
  (failures #54 で user 側パディングでも崖を確認済み)。無効。
- **プロンプトを ~8000 未満に削る**: chronicle/あらすじ/brief は北極星の機能そのもの。削れない。
- **モデルを 2.5-flash に固定**: 404「新規提供終了」で不能 (failures #54)。逃げ道なし。

## データ (data_contract 追記 — コードの前に凍結)

`data_contract.yaml` の `UnifiedToolLayer` 節に追記:
- `CacheHandle { name: String("cachedContents/<id>"), key: u64(静的プレフィックスの fingerprint),
  expire_hint: 作成時刻+TTL }` — `LlmClient` 内部状態、外部非公開。
- `GeminiCacheConfig { enabled: bool(既定 true, LLM_GEMINI_CACHE), ttl_secs: u64(既定 600?),
  min_tokens: u64(サイズゲート、モデル最小に合わせる) }` — `LlmConfig` に merge。
- cachedContent の wire 形 (create request/response、generateContent の `cachedContent` フィールド)
  をコメントで凍結。

## 実装 (Phase 分割、各 Phase Red→Green)

- **Phase A — encode の分割 + fingerprint (純粋、live 不要) ✅ Done (2026-07-15)**:
  `gemini::encode(req)` = `encode_with_cache(req, None)` の薄いラッパへ。`encode_with_cache(req,
  Some(name))` は systemInstruction/tools を送らず `cachedContent: name` を参照 (tool_config=mode ANY
  は per-request で残す)。`GenerateContentRequest.cached_content: Option<String>` (skip if none =
  None は従来 body と完全一致=回帰ゼロ)。`fingerprint(req)` = model+先頭 system+tools の FNV-1a
  64bit (可変 user は除外)。cache は未 wire (fingerprint/fnv1a は `#[allow(dead_code)]`、Phase B で
  client の照合に接続)。PoC 2 本: `gemini_fingerprint_keys_static_prefix_only` (user 不変・
  system/model/tools 変化で別 key) / `gemini_encode_with_cache_omits_prefix_and_references_cache`
  (None=従来一致 / Some=プレフィックス省略+cache 参照)。既存 `gemini_encode_maps_system_tools_and_roles`
  green 維持が回帰証明。
- **Phase B — cachedContents create/参照 + lifecycle ✅ Done (2026-07-15)**: gemini.rs に純ロジック
  (`CacheHandle{name,fingerprint}` / `CacheAction{Reuse|Create|Bypass}` / `decide_cache_action`
  (無効・サイズゲート未満→Bypass / fp 一致→Reuse / それ以外→Create) / `static_prefix_chars` (サイズ
  ゲートの char 近似) / `CreateCacheRequest`+`build_create_request` (encode_with_cache(None) の抽出を
  再利用=乖離ゼロ・model は `models/` プレフィックス付与 / `is_cache_miss_error` (Api 403/404))。
  config に `LLM_GEMINI_CACHE`(既定 on)/`_TTL`(900)/`_MIN_CHARS`(4000) + `cachedcontents_endpoint()`。
  client に `gemini_cache: Mutex<Option<CacheHandle>>` + `gemini_complete` (fingerprint→decide→
  reuse/create/bypass→encode_with_cache→送信→失効時 handle クリア+full 再試行) + `gemini_create_cache`
  (`POST /cachedContents` + x-goog-api-key)。**fallback 徹底**: create 失敗/サイズゲート未満/無効化は
  full request に落ちる=turn を絶対落とさない。**lock は await を跨がない** (判定だけ lock 内で即 drop)。
  PoC: `gemini_decide_cache_action_reuse_create_bypass` / `gemini_build_create_request_extracts_static_prefix`。
  HTTP オーケストレーション (create 実呼び出し・reuse・失効 fallback) は unit 非対象 → Phase D live。
- **Phase C — 台帳追従 ✅ Done (2026-07-15)**: (config フィールド `LLM_GEMINI_CACHE`/`_TTL`/
  `_MIN_CHARS` は Phase B で `LlmConfig` へ実装済)。data_contract に `gemini_explicit_cache` 節 /
  CLAUDE.md に spec-13 bullet / `.env.example` に env 3つ (既定 on・fallback で止まらない旨) /
  failures #54 の【解の方向】を「spec 13 (A+B 実装済)」へ更新。**app 設定 UI は無改修** —
  cache は client 内に閉じ (D5)、既定 on ゆえ受領者は何もしなくてよい。**outcast package_spec.md は
  無関係** (作者向けパッケージ形式でなく client 内部のキャッシュ最適化ゆえ写し不要)。
- **Phase D — live 検証 (実キー)**: sun_girl_ntr を gemini-flash-latest で回し、**総プロンプト
  >8000 でも `cachedContentTokenCount > 0`** を確認 (暗黙では 0 だった領域)。確認項目:
  (a) 明示キャッシュが 3.5-flash / `-latest` エイリアスで作成できるか (**エイリアスは cachedContents
  非対応の可能性 — その時は具体版へ解決してから作成**、要 live) (b) 最小トークン閾値の実値
  (c) storage コストの体感 (d) campaign 遷移で key が変わり再作成されるか (e) 却下ループ
  (self-repair) 中も cache が効くか。**完了判定**: sun_girl で cache_read が全ターン >0。

## 北極星との整合

三権分立は不変 — キャッシュは「LLM が提案する」脚の**配管の最適化**で、裁定・正本・schema
機械生成に一切触れない。fallback があるので**キャッシュが壊れても正しさは落ちない** (最適化と
正しさの分離)。同人配布のゼロ設定は D3 (既定 on + サイズゲート + fallback) で守る。

## 未決

1. **既定 on か opt-in か** — storage コスト (token×時間) があるので、既定 on だと短時間
   セッションで僅かな無駄。サイズゲート + 短 TTL で吸収できると見て**既定 on を推す**が、
   live のコスト実測 (Phase D-c) 後に確定。
2. **TTL の既定値** — 長い (1h) ほどヒット機会が増えるが storage 課金も増える。1 セッションの
   典型長 (数十分) を見て ~10-30 分で調整。TTL 内に収まらない長セッションは失効→透過再作成 (D2-4)。
3. **エイリアス (`-latest`) の cachedContents 対応** — 非対応なら create 前に具体バージョンへ
   解決する経路が要る (Phase D-a で確認)。gemini-flash-latest → 実体 gemini-3.5-flash は
   modelVersion で観測済み。
4. **cache の明示 delete** — TTL 自動失効に任せる (delete 不要) か、new_game/セッション終了で
   掃除するか。掃除しないと TTL まで storage 課金が残る — 短 TTL なら気にしない方針を推す。
5. **他プロバイダへの一般化は不要** — Anthropic 済 / OpenAI 互換・Grok は明示 API 無し。
   本 spec は Gemini 単独で閉じる (spec 12 の adapter 分離の恩恵)。
