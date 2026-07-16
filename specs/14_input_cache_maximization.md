# 14. 入力キャッシュの最大化 — append-only あらすじの多段キャッシュ + キャッシュ実効の観測

Status: **Phase A〜C 実装済（2026-07-16、Draft rev2 のまま実装 — Phase D live 実測と Phase E 任意メーターが残）。**
実装: Phase A = Anthropic 多段 breakpoint（leading system 毎・cap 4）+ Gemini synopsis 除外
（encode/fingerprint/サイズゲートとも 1 本目のみ、PoC `anthropic_places_breakpoint_per_leading_system_capped_at_four` /
`gemini_excludes_second_leading_system_from_cache`）→ Phase B = turn.rs が synopsis を独立した
2 本目の leading system へ分離（空なら出さない、PoC `synopsis_becomes_second_leading_system_for_cache`）→
Phase C = `CacheStat` に累積 hit_tokens/total_tokens + 直近 32 件リングバッファ（有界）、
`[LLM_CACHE_STAT]` 機械可読行（PoC 計数則/有界/零値）。A→B の順序凍結どおりに実装。
契約は data_contract `input_cache_maximization` 節。
rev2 反映: #1 D4 に Gemini adapter の synopsis 除外を明記 + Phase A 受入条件へ / #2 実装に A→B 依存を凍結 /
W5 D5 に hit rate 定義（cached=cache_read, prompt=input_tokens）/ W4 CacheStat の有界化 / W2 D2 に user 側
breakpoint の拡張余地 / W3 D3 を章追加 dip に整合 / W1 未決2 に位置変化の検証追加 / #3 未決5 を「本 spec
非実装・観測のみ」と明記（GM_SYSTEM は harness ゆえ gm_core 無改修には非抵触、が出力レバーはスコープ
クリープ）/ M1 型保証に emit_delta schema（argless `schema_for!`）を追加 / M2 data_contract の置き場を
client/cache 系へ。

Scope: `crates/llm_client`（Anthropic 多段 cache breakpoint + Gemini synopsis 除外）+ `crates/harness`
（append-only あらすじを独立した leading message へ分離）+ 観測（`CacheStat` 拡張 / 実測経路）。app の
キャッシュ率メーターは任意（Phase E）。**gm_core / CLI / 正本は無改修**（キャッシュは配管の最適化、
裁定に触れない）。**未決5（出力の max tokens）は本 spec では実装せず観測のみ**（出力側の別レバー、
スコープクリープ回避）。

## 用語

- **静的プレフィックス** = `emit_delta schema + GM_SYSTEM + scenario_brief`（world/キャラ設定/GM 口調/盤面）。
  シナリオ内で不変。**型で保証（3 要素すべて）**: `emit_delta schema` = `state_delta_schema()` =
  `schemars::schema_for!(StateDelta)` の**引数ゼロ**（型からの導出、状態入力なし）/ `GM_SYSTEM` は harness の
  const（`prompt::GM_SYSTEM`）/ `scenario_brief(&Scenario)` と `gm_system_prompt(&Scenario, bool)` は
  `&GameState` を受け取れない（可変状態を物理的に見られない）。→ 3 要素とも byte 安定。
- **breakpoint** = Anthropic の `cache_control: ephemeral` を置く位置。先頭からその位置までの安定
  プレフィックスがキャッシュ対象になる。**最大 4 個**。現状 Kataribe は 1 個（静的プレフィックス末尾）。
- **append-only あらすじ** = spec 10 の `synopsis`。章を**末尾に足すだけ**（書き直さない＝複利ドリフト
  を構造的に断つ）。章追加の**間**は byte 安定 = 第二の不変領域。
- **cache hit rate** = `cached_tokens / prompt_tokens`。usage が一次ソース（#44/#45/#54 の系）。

## 目的（なぜ今 — ユーザー実測で確定）

1. **TRPG のコストは入力が支配的**（ユーザー観測）。1 ターン = フルプロンプト 1 往復で、入力は
   毎ターン再課金。静的プレフィックスは既にキャッシュ済（Anthropic cache_control / Gemini
   cachedContent spec 13 / OpenAI・Grok 自動）で**型で不変が保証**されている → **system prompt 側の
   レバーは使い切った**。
2. **残る入力コスト = 可変 user メッセージの成長**。`state_brief`（毎ターン変化）+ **成長する
   chronicle/あらすじ**が未キャッシュで積み上がり、session 長にほぼ二次で効く。あらすじは
   append-only ＝**本質的に安定なのにキャッシュされていない**。ここをキャッシュに寝かせれば
   成長曲線が平らになる（静的プレフィックスと同じ発想を、成長するが安定な領域へ一段深く適用）。
3. **実効を測る手段が弱い**。`CacheStat` は連続 miss 数までで、長セッションの hit rate 曲線を追えない。
   キャッシュの静かな漏出は無言で起きる（#54 の ~8000 崖が実例）→ 観測を一次ソース（usage）で持つ。

①（観測）と②（多段キャッシュ）を **1 spec に束ねる**理由: 観測は②の acceptance そのもの
（hit rate 曲線が長セッションで平らなら②が効いている、декой decay すれば効いていない）。

## 問題

1. **synopsis が可変 user メッセージの中（`state_brief` の後）にある**（[turn.rs:283-306](../crates/harness/src/turn.rs)）。
   user メッセージは 1 本の連結 String で、先頭の `state_brief` が毎ターン変わる → **user メッセージは
   byte 0 から可変** → synopsis を含めどの部分も自動キャッシュに乗らない。append-only なのに。
2. **Anthropic encode は breakpoint を 1 個しか使わない**（[anthropic.rs:133-136](../crates/llm_client/src/anthropic.rs)、
   leading system の最後のブロックだけ cache_control）。4 個まで使えるのに第二の安定領域を活かせていない。
3. **hit rate 曲線を測れない**。`CacheStat{last_cache_read, consecutive_misses, total_requests}` は
   警告用で、per-turn の `cached/prompt` 比の履歴を持たない。

## 決定（設計の核）

### D1 — append-only あらすじを独立した leading message へ分離し、第二 breakpoint を打つ

`turn.rs` のメッセージ構成を変える:

- **現在**: `[system(静的), user(state+moved+synopsis+history+…+action)]`
- **変更後**: `[system(静的), system(synopsis), user(state+moved+history+…+action)]`
  （synopsis が空なら 2 本目の system は出さない = breakpoint は増えない）

synopsis を静的プレフィックスの**直後・可変 user の前**の独立 leading message にする。これで:
- **安定プレフィックスが二段**になる: `静的`（常に安定）と `静的+synopsis`（章追加の間だけ安定）。
- 章が追加された 1 ターンだけ第二段が失効 → 第一段は生存 → 次ターン再 warm。**大半のターンで
  synopsis までキャッシュ**。

**behavior 変化（rev2・W1、未決2 の検証項目）**: synopsis は現在 user メッセージ内で `state_brief` の
後・`history_note` の前にあるが、変更後は system（可変 user の前）へ移る = **提示位置が「history の前」から
「state の前」へ跳ぶ**。role も user→system。Anthropic 的には system の方が強い背景文脈になるので望ましい
という仮説だが、GM が正しく織り込むかは未決2 で検証する。

### D2 — Anthropic encode を「各 leading system ブロック末尾に cache_control（cap 4）」へ拡張

現状の「最後の 1 ブロックだけ」を「**leading system メッセージ毎に 1 breakpoint**（先頭から 4 個まで）」へ。
turn.rs が 2 本の leading system を出せば自動で 2 breakpoint。**1 本だけの既存ケースは挙動不変**（回帰なし）。
canonical モデルは無改修（content: String のまま、spec 12 K3 を保つ）— breakpoint 数は
「leading system メッセージが何本か」で決まる（暗黙規約、明示 `cache: bool` フィールドは足さない）。
**将来の拡張余地（rev2・W2、未決2 のフォールバック用）**: synopsis を system でなく leading user に置く案
（未決2）でも、同じ規約を **leading user メッセージ**へ適用する（Anthropic は user content block にも
cache_control 可）。この時も canonical への `cache: bool` 追加は避け、「**先頭から連続する leading
system/user の各末尾に打つ**」規約で表す（暗黙規約の一般化で表現できる）。

### D3 — OpenAI / Grok は自動延伸（マーカー不要、ただし Anthropic と同じ章追加 dip）

OpenAI/Grok の自動キャッシュは安定 byte プレフィックスを自動で延ばす。synopsis が system 直後の
安定 message になるだけで、`[system(静的), system(synopsis)]` までが自動キャッシュ対象に**乗る**
（D1 の並べ替えの副産物、マーカー不要）。**ただし「無料でシームレス」ではない（rev2・W3）**:
synopsis が `章1 → 章1+章2` と伸びた**章追加ターンは伸びた差分がミス**（旧 synopsis 部分まではヒット）→
次ターンから新 synopsis 込みで再ヒット。Anthropic の二段 breakpoint と**同じ dip**（章追加時だけ一段
下がり翌ターン回復）。Grok は `x-grok-conv-id`（#45）が引き続き sticky routing を担保。

### D4 — Gemini は v1 では静的のみキャッシュ（synopsis は cachedContent から除外・inline 非キャッシュ）

Gemini の `cachedContent`（spec 13）は system+tools を単一リソースに pin する。synopsis を含めると
章追加毎に**再 pin**（storage 課金の churn）＝ D1 の「synopsis を安定 message にした」意図と逆。
しかも Gemini は `systemInstruction` が単一なので、2 本の leading system を素朴に畳むと synopsis まで
pin されてしまう。**そこで Gemini adapter の明示規約（rev2・#1 の解消）**:

> **Gemini adapter は 2 本目の leading system（synopsis）を `cachedContent` 対象から除外し、
> `generateContent` の inline `contents` として非キャッシュで送る。`cachedContent` に pin するのは
> 1 本目（静的プレフィックス）だけ。fingerprint（spec 13）は 1 本目のみから計算**し、synopsis の
> 増減で cache が再作成されないようにする。

これで Gemini は静的プレフィックスだけ安定 pin、synopsis は毎ターン inline（章追加でも再 pin なし）。
**この除外ルールは Phase A の受入条件**（Phase B で 2 本 system を出す前に全プロバイダの分岐を確定させる）。
再 pin する版（synopsis も pin）の損益は Phase D のコスト実測後に判断（未決 #1）。
**fallback は不変**: どのプロバイダでもキャッシュが効かなくても turn は落ちない（正しさは正本が握る）。

### D5 — 観測（`CacheStat` 拡張 + 実測経路）

**hit rate の定義を凍結（rev2・W5、観測ブレ防止）**: `cached = cache_read_input_tokens`（読取 0.1×）、
`prompt = input_tokens`（総入力）。**`cache_creation_input_tokens`（書込 1.25×）は cached に含めない**
（章追加ターンの再 warm を「hit」と誤計上しないため）。互換経路は `prompt_tokens_details.cached_tokens`
を read として扱う。→ 曲線 = `cache_read / input`。

`CacheStat` に per-turn の記録を足すが、**有界化する（rev2・W4）**: 無制限の per-turn 履歴は長セッション
（100 ターン超）で常駐メモリが伸びる → **累積 `hit_tokens / total_tokens` + 直近 N 件のリングバッファ**
（N は曲線の可視化に足りる小さな値）。`LLM_CACHE_DEBUG` の行を機械可読（turn + input + cache_read +
ratio）に整える。**ドル建てコストは出さない**（provider 価格は変動・stale 化）— token と比率に留める。
app メーター（セッションのキャッシュ率）は任意（Phase E、コスト可視化の布石）。

## 却下した代替案

- **synopsis を user メッセージの多ブロック content に分割 + cache_control**: canonical content は
  String（spec 12 K3）ゆえ多ブロック化は大改修。leading system への分離（D1）が最小差分。
- **synopsis を現状維持で①（観測）だけ**: ②も spec 化するユーザー要望に反する。
- **ドル建てコスト表示**: provider 価格の変動で stale 化 → token/比率で接地。
- **history retrieval（spec 08）もキャッシュ**: retrieval は毎ターン query で選び直す = 不変にならない。
  ここはキャッシュでなく**トークン予算を締める**別レバー（本 spec スコープ外）。

## データ（data_contract 追記 — コードの前に凍結）

- **キャッシュ規約は client/cache 系の節へ（`UnifiedToolLayer` のツール意味論でなく、spec 13 の
  `gemini_explicit_cache` と同じ層。rev2・M2）**: 「Anthropic adapter は先頭から連続する leading system
  メッセージ毎に cache_control を 1 個置く（最大 4）。Gemini adapter は 1 本目（静的）のみ cachedContent に
  pin し 2 本目（synopsis）は inline 非キャッシュ・fingerprint も 1 本目のみ。OpenAI/Grok は自動（章追加
  dip あり）。turn.rs は静的プレフィックスと append-only synopsis を別々の leading system として出す」。
- **hit rate 定義の凍結**: `cached = cache_read_input_tokens` / `prompt = input_tokens`
  （`cache_creation` は cached に含めない）。
- `CacheStat` 拡張: 累積 `hit_tokens/total_tokens` + 直近 N リングバッファ（無制限履歴は持たない）。
- メッセージ構成の凍結: `[system(静的), system(synopsis?), user(可変)]`。

## 実装（Phase 分割、各 Phase Red→Green）

**Phase 依存の凍結（rev2・#2）: A は B の前提。A→B の順でしか Green にならない。** B（turn.rs が 2 本
system を出す）を A より先に出すと、Anthropic は旧実装で最後の 1 ブロック（=synopsis）にしか cache_control
を打たず、**静的プレフィックス単独の breakpoint が消える** → 章追加ターンに静的まで巻き添えで書込に落ちる
（現状より悪化）。かつ Gemini は synopsis を pin してしまう（D4 違反）。よって A で全プロバイダの分岐を
確定させてから B へ。

- ✅ **Phase A — Anthropic 多段 breakpoint + Gemini synopsis 除外（純粋 encode 改修）**: Anthropic =「最後の
  1 ブロック」→「先頭から連続する leading system 毎に cache_control（cap 4）」。**Gemini = 2 本目の leading
  system を cachedContent から除外し inline 化、fingerprint は 1 本目のみ（受入条件、#1）**。PoC: Anthropic
  2 leading system → 2 cache_control / 1 本 → 1 個（既存回帰）/ 5 本 → 先頭 4 個 ; Gemini 2 leading system →
  cachedContent は 1 本目のみ・2 本目は inline contents。canonical は無改修。
- ✅ **Phase B — synopsis を別 leading message へ（turn.rs）。A 完了が前提**: 空 synopsis は 2 本目を出さない。
  既存 `synopsis_is_woven_into_prompt_before_history` / `empty_synopsis_means_no_synopsis_block` の
  position 変化を追従（synopsis が user 内から system へ移る = 提示位置が history の前から state の前へ）。
  PoC: synopsis 有 → system 2 枚 + user から synopsis 節が消える / 無 → system 1 枚。
- ✅ **Phase C — 観測（`CacheStat` 拡張 + 実測経路）**: per-turn ratio 記録 + デバッグ行の機械可読化。
  PoC: 計数則（両経路の cached/prompt を拾う）/ 零値スナップショット。
- **Phase D — live 実測（実キー 4 プロバイダ・長セッション）= ②の acceptance かつ①の実体**:
  30 ターン級の通しプレイで **hit rate 曲線が平ら**（章追加ターンだけ dip して次ターン回復）を確認。
  静的のみだった従来比で **cached tokens 総量が増える**（synopsis 分）。Anthropic は breakpoint 2 の
  cache_read>0、OpenAI/Grok は自動延伸を usage で確認、Gemini は静的のみ（D4）。
- **Phase E — app キャッシュ率メーター（任意）**: セッションの cached/prompt をヘッダ等に surface
  （#44 の健全性警告の基盤を流用）。コスト可視化の布石。

### Phase D 先行観測 — Grok・GUI 人間ペース 12 リクエスト（2026-07-16、ユーザー実測）

`cached` が **128 / 6144 の二値**で 2〜3 連の塊で交替（6 hit / 6 miss、累積 hit rate 42.7%、
prompt は 6395→8169 に単調成長）。読み:

- **6144 = 静的プレフィックス全体**（128 トークンブロック × 48。prompt が 7312→8169 と伸びても
  6144 で一定 = キャッシュされているのは静的 system+tools だけで、可変 user は乗っていない —
  本 spec の動機②の実測そのもの）。**128 = miss のフロア**（最小ブロック 1 個）。
  つまり「128 頭打ち」は上限でなく **miss の表示値**だった。
- spec 12 Phase E の「turn3 から ~88% 安定」は**台本駆動の高速ループ**（リクエスト間隔が秒オーダー）
  での観測。人間ペース（間隔が分オーダー）では 50% 前後に落ちる — **有力仮説は TTL/eviction**
  （自動キャッシュの保持が数分で、プレイヤーの思考・入力時間が閾値を跨ぐと miss）。
  対立仮説は sticky routing の best-effort 分散（x-grok-conv-id 送出済みでも LB が複数バックエンドに
  流し、温まっていない個体に当たる）。
- **弁別装置**: `[LLM_CACHE_STAT]` に `t=`（unix 秒）を追加済み。miss が長い間隔の直後に集中すれば
  TTL 説、間隔非依存なら分散説。次の Grok プレイは本ビルド + `LLM_CACHE_DEBUG=1` で計測する。
- **本 spec への含意**: Grok の実効 hit rate は入力間隔依存 = サーバ側 TTL は我々のレバーでない。
  Anthropic は cache_control の TTL 5 分が**読取で更新**されるので人間ペースに強い（対照実験候補）。

### 第二測定 — `t=` 付き 40 リクエスト（2026-07-16 同日、Grok・GUI 人間ペース・~39 ターン + エピローグ）

**結論 2 つ（failures.md #57 に凍結）**:

1. **TTL 説は棄却**: miss の直前間隔 29〜68 秒 / hit の直前間隔 25〜118 秒（平均 59 秒）—
   miss は間隔と無相関。残る説明は LB の best-effort 分散（sticky ヘッダでも時々コールド個体）
   ないし確率的 eviction。定常 miss 率 ~11%（4/35）、warm-up 3 リクエスト + 部分ヒット 4096 が 1、
   セッション累積 hit rate **60.6%**（人間ペース Grok の上限見積もりに使う）。
2. **本 spec の Grok live Green（D1+D3 実証）**: 章圧縮のたび `cached` が **6144→6400→6656 と
   +256/章 の階段**で成長 = synopsis（2 本目 leading system）が自動プレフィックスキャッシュに乗った。
   章追加直後は旧プレフィックス分（6144/6400）だけヒット → 1〜2 リクエストで新全量に回復 —
   **D3 が予測した dip の形そのもの**。圧縮後は prompt 自体も 9024→8436 へ縮む（spec 10 の注入経済）。

副次観測: ①summary 用 client（spec 10）の `[LLM_CACHE_STAT]` が **req 連番を 1 から別カウント**して
同じ stderr に混ざる（cached=0・input ~1200 の行）→ 行に `conv=` を追加して系列を弁別可能にした。
②エピローグ（spec 11、GM client の最終 req）は messages 形状が別物なので miss=128 が正常。
③GUI キャッシュ警告（連続 miss>=3）が Grok の warm-up 3 連 miss で開幕に発火しうる — 閾値 3→5 か
「一度でも hit した後だけ武装」への再調整候補（未実施）。

### 第三測定 — Anthropic 対照・53 リクエスト（2026-07-16 同日、GUI 人間ペース・~50 ターン + エピローグ）

**多段 breakpoint live Green + D5 定義の実測補正（failures.md #58）**:

1. **決定論の対照が成立**: 通常ターン 51 のうち **miss ゼロ**（Grok の確率的 ~11% と対照）。
   260 秒間隔でもヒット = TTL 5 分が読取で更新される決定論挙動の実証。
2. **多段 breakpoint の階段**: 章圧縮のたび cache_read が **8978→9359→9650→9978**（synopsis
   第二段が読まれている）。章追加ターンは静的分 8978 のみ読み + **synopsis ブロック全体を再書込**
   （write=381/672/1000）— Anthropic は breakpoint 単位の exact 一致なので、Grok（ブロック粒度の
   byte-prefix、延伸差分だけ miss）と違い章追加毎に synopsis 全体が 1.25× 書込になる。ただし
   synopsis は予算 2000 字で有界 → コストは無視できる（この差は仕様として記録、対処不要）。
3. **D5 の定義補正**: Anthropic native の `input_tokens` は**非キャッシュ分のみ**（総入力 =
   input + cache_read + cache_creation）→ 素通しだと ratio が 1 を超える（実測 6.88）。
   anthropic::decode で canonical `prompt` を総入力へ正規化（OpenAI/Gemini は元々総入力 = 無改修）。
   **凍結する分母 = 総入力**。正規化後の累積 hit rate **71.0%**（Grok 60.6% との差 = 決定論 vs
   確率分散 + 静的プレフィックスの相対サイズ）。
4. エピローグ（最終 req）は read=0/write=0 — 別形状 + 静的 prefix が最小キャッシュ（4096 tokens）
   未満で cache_control 黙殺。正常。

### 第四測定 — Gemini 対照・25 リクエスト（2026-07-16〜17、GUI 人間ペース・73 分の中断込み）

**D4（synopsis 除外）live Green + TTL 方言の発見（failures.md #59）**:

1. **D4 の実証**: 章圧縮（summary client の req=1）の後も `cached=5630`（静的 pin）のまま・
   **再 pin なし**（「cachedContent 作成」ログが出ない = fingerprint が 1 本目のみから計算されて
   いる Phase A 修正が効いた）。synopsis は inline contents で prompt に +214 乗る。章追加の
   storage churn ゼロ — 設計どおり。
2. **TTL は作成時刻から固定（読取で更新されない）**: 検算で cache age ~900s ちょうどに失効境界
   （req18 age~900s ヒット / req19 age~953s 失効、直前リクエストから 56 秒しか空いていない）。
   Anthropic（読取更新）と真逆 → 連続プレイでも **~15 分周期で失効→fallback→再 pin** が入る。
   fallback は設計どおりターンを落とさず、失効ターンは暗黙キャッシュが静的の ~72%（4074/5630）を
   拾う。73 分中断後の失効も同経路で透過回復。
3. 累積 hit rate **72.6%**（失効 2 回込み）— Anthropic 71.0% / Grok 60.6% と並ぶ。
   Gemini の promptTokenCount は総入力（cached を含む）なので #58 の正規化問題は無し。
4. 改善候補（未実施・優先度低）: `CacheHandle.created_at` + age >= ttl−margin で**先回り再 pin**
   （失効 403 のラウンドトリップ節約）。または PATCH で ttl 延長。

**Phase D の残り**: OpenAI 互換の対照測定（自動延伸の確認、任意）と、synopsis の system role 化の
behavior 検証（未決 2 — 実測 3 本 ~140 ターンで GM の語りの破綻報告は無し、明示検証は残）。

## 北極星との整合

三権分立は不変 — キャッシュは「LLM が提案する」脚の**配管の最適化**で、裁定・正本・schema 機械生成に
触れない。**fallback があるのでキャッシュが壊れても正しさは落ちない**（最適化と正しさの分離）。
同人配布のゼロ設定（既定で効く・受領者は何もしない）を守る。append-only の不変条件（spec 10）は
本 spec が依存する前提であり、崩さない。

## 未決

1. **Gemini の synopsis キャッシュ**（D4）— 章追加毎の再 pin（storage 課金）と非キャッシュの
   どちらが得か。Phase D のコスト実測後に確定。
2. **synopsis を system ロールに上げる behavioral 影響** — GM が正しく織り込むか（spec 10 は user
   メッセージ・history の前に置いていた）。**位置が二重に変わる（rev2・W1）**: (a) role が user→system、
   (b) 提示順が「history の前」→「state の前」へ跳ぶ。system の方が「安定した背景文脈」として自然という
   仮説だが、両変化を Phase B/D で検証。ダメなら **user ロールの leading message + user 側 breakpoint** へ
   （Anthropic は user content block にも cache_control 可、D2 の拡張余地）。
3. **history retrieval のトークン予算締め** — 本 spec スコープ外（キャッシュでなく削減の別レバー）。
   ②の効果を測った後に別 spec で扱うか判断。
4. **breakpoint の粒度をさらに増やすか** — 例えば安定な直近 chronicle prefix にも 3 個目を打つ余地
   （ただし retrieval が prefix を崩すので現状は不安定）。v1 は 2 段（静的 + synopsis）に留める。

5. **出力の max tokens — 作業アクションの narration を短くする二次レバー（別問題・入力の後）**。
   **★本 spec では実装しない — 観測のみ（rev2・#3、スコープクリープ回避）**。これは入力キャッシュと独立した
   出力側レバー。実装するなら harness の GM_SYSTEM（＝`prompt::GM_SYSTEM`、**gm_core ではない → Scope の
   「gm_core 無改修」には非抵触**）+ prompt 層への刷り込み。本 spec のスコープ外なので、ここでは
   **先行観測の方法だけを凍結**し、実装可否は観測結果を見て別途（別 spec or 本 spec の Phase 追加）判断する。
   コスト対策の対称ペア: **入力側 = プロンプトキャッシュ（本 spec 本体）/ 出力側 = max tokens**。
   本 spec は入力を叩くが、出力も削れる: 地味な行動（移動・調べる・持ち物確認など帰結の軽い手）
   は 1〜2 文で足り、豊かな描写は山場（戦闘・発覚・初対面・ゴール関連）に取っておく。これは
   コスト削減であると同時に **UX 改善**（「扉まで歩く」に美文は不要）＝トレードオフでなく win-win。
   **機構は二層**: (a) 軟＝prompt ペーシング（GM に短く狙わせ分布をずらす）/ (b) 硬＝API の
   `max_tokens` パラメータを上限として被せる。名前が指すのは (b) の硬い天井だが、(a) が LLM を
   天井の下に誘導するので**文途中の醜い truncate が起きにくい**（steering なしに硬 cap だけ被せると
   途中で切れる）。(b) を per-turn 可変にする場合も (a) と同じ**生成前の判定問題**を共有する
   （その手が地味か決まるのは delta 後 → ターンの max_tokens は player 行動 / 直前温度から推すしかない、
   fuzzy）。だから既定は「(a) で分布をずらす + (b) は醜い暴発を防ぐ緩めの固定天井」、per-turn 可変の
   (b) は先行観測で「作業アクションが安定して短い」と確認できてからの上積み。
   **難所は判定のタイミング**: 「作業アクションか」は delta を見て初めて確定するが、narration は同じ
   応答で delta と同時に生成される（check 結果が同ターンに間に合わない時間差と同型）→ **生成前の
   精密な分類器は筋が悪い**（意図解析は fuzzy、別 LLM 呼び出しは削減分を食う）。現実解は **prompt 層の
   自己ペーシング**（GM_SYSTEM に「ステークスに応じて長さを決めよ」を刷り込み、既に surface 済みの
   文脈=この場の挑戦/ゴールの近さ/直前の発火 で GM に自己判定させる）。任意で**直前ターンの
   eventfulness**（trigger 発火/check/goal 到達 = apply の戻りにある決定論事実）を次ターンに
   「直前は山場/平穏」の弱いペーシング手掛かりとして還流（現ターンの delta は無理だが直前の温度は
   決定論で渡せる。ただしレールロード圧を避け弱く）。**限界**: 自己制限は soft（弱/冗長モデルは
   従わない、Grok の過剰 narration 実例）、`max_tokens` は hard cap だが文途中で切れて醜い →
   「分布をずらす」もので「保証」ではない。
   **★観測はコード修正なしで今すぐできる（ユーザー指摘、本 spec の先行実験）**: 現行の
   system prompt（world / GM 口調 / package の語り素材）に**ペーシング指示を1行足すだけ**で、
   engine 無改修で挙動を観測できる。効果は**会話ログのテキスト保存**が narration 文字数の計測装置
   （spec 12 で tool-use 1.6× を測ったのと同じ経路）— 作業アクション台本 vs 山場台本の A/B で
   平均文字数の差を見る。**この先行観測で効果が確認できてから**、GM_SYSTEM への恒久刷り込み +
   直前温度の還流を prompt 層の小改修（`gm_system_demands_...` 型 PoC）として実装するか判断する。
   engine には触れない（narration の砦は prompt のみ、#23 同型）。本 spec の入力キャッシュとは
   独立したレバーだが、「コスト削減の複数レバー」として同じ傘で追跡する。
