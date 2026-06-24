//! Memoria 脚 (memoria_bridge)。トリガー発火点で**伏線・キャラ性格を semantic recall** し、
//! 語りに注入する。三権分立の「Memoria が覚える」脚。
//!
//! # 北極星の不変条件 (可変世界状態は禁忌)
//!
//! Memoria が持てるのは **不変の authored lore** ([`MemoryFragment`]: 伏線・性格) **だけ**。
//! HP・所持品・フラグ・位置・数値といった**可変世界状態は絶対に持たない** — それらは正本
//! ([`gm_core`]) の専有。可変状態を曖昧な recall に置くと「忘れる GM」を再現するため。
//! この不変条件は**型で構造的に保証**される: [`MemoryFragment`] は state フィールドを持てず、
//! [`Memoria::recall`] は `&self` (retrieval only、state を変えない)。
//!
//! # 依存性逆転 ([`DeltaProposer`](crate::DeltaProposer) と同型)
//!
//! [`Memoria`] trait に対して書く。実装 [`LoreStore`] は **文字 bigram TF-IDF の cosine 類似**で
//! semantic recall する (日本語は単語境界が無いため文字 n-gram が頑健、依存ゼロ・決定論・テスト可能)。
//! authored cue の exact id/tag 一致は意味類似に依らず常に最上位で保証する (旧 exact 挙動の上位互換)。
//! 神経 embedding 版が要れば同 trait 裏で差し替え可 (`ScriptedProposer` → `LlmClient` と同じ swap)。
//! `()` は「recall しない」null 実装。

use std::collections::HashMap;
use std::path::Path;

use gm_core::{FiredTrigger, TriggerId};
use serde::{Deserialize, Serialize};

use crate::error::HarnessError;

/// 不変の authored lore 断片 (伏線・キャラ性格)。**可変世界状態は持てない** —
/// フィールドは `id`(recall キー) / `tags`(別名キー) / `text`(語りに注入する本文) のみ。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryFragment {
    /// recall の主キー。`memoria/*.yaml` のファイル名 (拡張子なし)。loader が充填する。
    #[serde(default)]
    pub id: String,
    /// recall の別名キー (semantic surface)。cue がこのいずれかに一致すれば hit。
    #[serde(default)]
    pub tags: Vec<String>,
    /// 語りに注入する伏線/性格の本文。
    pub text: String,
}

/// 伏線・キャラ性格の recall の抽象。**可変状態は recall できない** ([`MemoryFragment`] のみ返す)。
///
/// `recall(cue)` は cue に関連する lore 断片を返す。`&self` であることが「Memoria は
/// 世界状態を変えない」ことの型レベルの保証になっている。
pub trait Memoria {
    /// cue (tag/id) に関連する lore を返す。該当無しなら空。
    fn recall(&self, cue: &str) -> Vec<MemoryFragment>;
}

/// 「recall しない」null 実装。recall を使わないターンループ/テストで `&()` を渡す。
impl Memoria for () {
    fn recall(&self, _cue: &str) -> Vec<MemoryFragment> {
        Vec::new()
    }
}

/// cosine 類似がこの閾値以上の fragment を recall する (exact id/tag 一致は score 1.0 で常に通過)。
const RECALL_THRESHOLD: f64 = 0.08;

/// fragment の検索対象テキスト (id + tags + 本文) を結合する。
fn document_of(f: &MemoryFragment) -> String {
    format!("{} {} {}", f.id, f.tags.join(" "), f.text)
}

/// 文字 bigram に分解する。日本語は単語境界が無いため文字 n-gram が頑健。
/// 空白は無視。1 文字しか無ければ unigram にフォールバック。
fn bigrams(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().filter(|c| !c.is_whitespace()).collect();
    match chars.len() {
        0 => Vec::new(),
        1 => vec![chars[0].to_string()],
        _ => chars.windows(2).map(|w| w.iter().collect()).collect(),
    }
}

/// 語の出現頻度 (TF)。
fn term_freq(tokens: &[String]) -> HashMap<String, f64> {
    let mut tf = HashMap::new();
    for t in tokens {
        *tf.entry(t.clone()).or_insert(0.0) += 1.0;
    }
    tf
}

/// 疎ベクトルの内積 (小さい方を走査)。
fn dot(a: &HashMap<String, f64>, b: &HashMap<String, f64>) -> f64 {
    let (small, big) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    small.iter().filter_map(|(k, v)| big.get(k).map(|w| v * w)).sum()
}

/// L2 ノルム。
fn l2(v: &HashMap<String, f64>) -> f64 {
    v.values().map(|x| x * x).sum::<f64>().sqrt()
}

/// 実装: ロード済み lore への **文字 bigram TF-IDF cosine の semantic recall**。
///
/// 構築時に各 fragment の TF-IDF ベクトルと IDF (コーパス統計) を前計算する。recall は cue を
/// 同じ方式でベクトル化し cosine を取る。embedding 版に差し替えても利用側 (resolve_recall / CLI) は無変更。
#[derive(Debug, Clone, Default)]
pub struct LoreStore {
    fragments: Vec<MemoryFragment>,
    /// bigram -> IDF (smoothed)。コーパス (fragments) から算出。
    idf: HashMap<String, f64>,
    /// fragments と整列した TF-IDF ベクトル。
    vectors: Vec<HashMap<String, f64>>,
    /// vectors の L2 ノルム (cosine 用に前計算)。
    norms: Vec<f64>,
}

impl LoreStore {
    pub fn new(fragments: Vec<MemoryFragment>) -> Self {
        let n = fragments.len() as f64;
        // 各 fragment の bigram TF。
        let tfs: Vec<HashMap<String, f64>> = fragments
            .iter()
            .map(|f| term_freq(&bigrams(&document_of(f))))
            .collect();
        // DF (出現 fragment 数) → IDF (smoothed: ln((N+1)/(df+1)) + 1)。
        let mut df: HashMap<String, f64> = HashMap::new();
        for tf in &tfs {
            for k in tf.keys() {
                *df.entry(k.clone()).or_insert(0.0) += 1.0;
            }
        }
        let idf: HashMap<String, f64> = df
            .into_iter()
            .map(|(k, d)| (k, ((n + 1.0) / (d + 1.0)).ln() + 1.0))
            .collect();
        // TF-IDF ベクトルと L2 ノルム。
        let vectors: Vec<HashMap<String, f64>> = tfs
            .iter()
            .map(|tf| {
                tf.iter()
                    .map(|(k, t)| (k.clone(), t * idf.get(k).copied().unwrap_or(1.0)))
                    .collect()
            })
            .collect();
        let norms = vectors.iter().map(l2).collect();
        Self { fragments, idf, vectors, norms }
    }

    pub fn len(&self) -> usize {
        self.fragments.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fragments.is_empty()
    }

    /// cue を TF-IDF ベクトル化する (IDF はコーパスから流用、未知 bigram は idf=1.0)。
    fn vectorize(&self, cue: &str) -> HashMap<String, f64> {
        term_freq(&bigrams(cue))
            .iter()
            .map(|(k, t)| (k.clone(), t * self.idf.get(k).copied().unwrap_or(1.0)))
            .collect()
    }
}

impl Memoria for LoreStore {
    fn recall(&self, cue: &str) -> Vec<MemoryFragment> {
        let cue_vec = self.vectorize(cue);
        let cue_norm = l2(&cue_vec);

        let mut scored: Vec<(usize, f64)> = self
            .fragments
            .iter()
            .enumerate()
            .filter_map(|(i, f)| {
                // authored cue の exact id/tag 一致は意味類似に依らず保証 (旧 exact 挙動の上位互換)。
                let exact = f.id == cue || f.tags.iter().any(|t| t == cue);
                let score = if exact {
                    1.0
                } else if cue_norm == 0.0 || self.norms[i] == 0.0 {
                    0.0
                } else {
                    dot(&cue_vec, &self.vectors[i]) / (cue_norm * self.norms[i])
                };
                (score >= RECALL_THRESHOLD).then_some((i, score))
            })
            .collect();
        // score 降順、同点は id 昇順で決定論的に。
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| self.fragments[a.0].id.cmp(&self.fragments[b.0].id))
        });
        scored.into_iter().map(|(i, _)| self.fragments[i].clone()).collect()
    }
}

/// 発火した反応ビートに、Memoria から recall した伏線を解決したもの (FiredTrigger の harness 拡張)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FiredBeat {
    pub id: TriggerId,
    /// authored な静的語り (トリガー定義の narration)。
    pub narration: String,
    /// `recall` cue を Memoria で解決して得た伏線。cue が無い/該当無しなら空。
    pub recalled: Vec<MemoryFragment>,
}

/// 発火トリガー列の `recall` cue を Memoria で解決し [`FiredBeat`] 列にする。
///
/// **純粋 retrieval** — 可変世界状態には一切触れない (Memoria の不変条件を体現)。
/// `gm_core` の [`apply`](gm_core::apply) が返した `fired` をこれに通すのが memoria_bridge の本体。
pub fn resolve_recall<M: Memoria>(memoria: &M, fired: &[FiredTrigger]) -> Vec<FiredBeat> {
    fired
        .iter()
        .map(|f| FiredBeat {
            id: f.id.clone(),
            narration: f.narration.clone(),
            recalled: f
                .recall
                .as_deref()
                .map(|cue| memoria.recall(cue))
                .unwrap_or_default(),
        })
        .collect()
}

/// `dir` 直下の `*.yaml` を各 [`MemoryFragment`] として読み、[`LoreStore`] を作る。
/// **ファイル名 (拡張子なし) が `id`**。`dir` が無ければ空 (伏線無しシナリオは正常)。
/// I/O ゆえ engine ではなく harness の責務 (`load_characters` と同型)。
pub fn load_lore(dir: &Path) -> Result<LoreStore, HarnessError> {
    let mut fragments = Vec::new();
    if !dir.is_dir() {
        return Ok(LoreStore::new(fragments));
    }
    let entries = std::fs::read_dir(dir).map_err(|e| HarnessError::LoreLoad {
        path: dir.display().to_string(),
        detail: e.to_string(),
    })?;
    let mut paths: Vec<_> = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|e| HarnessError::LoreLoad {
                path: dir.display().to_string(),
                detail: e.to_string(),
            })?
            .path();
        if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
            paths.push(path);
        }
    }
    paths.sort(); // ファイル名順で決定論的に。
    for path in paths {
        let id = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let text = std::fs::read_to_string(&path).map_err(|e| HarnessError::LoreLoad {
            path: path.display().to_string(),
            detail: e.to_string(),
        })?;
        let mut frag: MemoryFragment =
            serde_yaml::from_str(&text).map_err(|e| HarnessError::LoreLoad {
                path: path.display().to_string(),
                detail: e.to_string(),
            })?;
        frag.id = id; // ファイル名を id に充填 (load_characters と同じ規約)。
        fragments.push(frag);
    }
    Ok(LoreStore::new(fragments))
}

// =============================================================================
// PoC: memoria_bridge の実証 (Red→Green)
// トリガー発火 → recall cue → Memoria が伏線を返す。可変状態は Memoria に無いことを構造で保証。
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> LoreStore {
        LoreStore::new(vec![
            MemoryFragment {
                id: "childhood_promise".into(),
                tags: vec!["約束".into(), "幼少期".into()],
                text: "幼い二人は、丘の上の古い樫の木の下で「いつか必ず戻る」と指切りをした。".into(),
            },
            MemoryFragment {
                id: "alice_sweet_tooth".into(),
                tags: vec!["性格".into()],
                text: "アリスは甘いものに目がなく、緊張すると蜂蜜飴を舐める癖がある。".into(),
            },
        ])
    }

    fn fired(id: &str, recall: Option<&str>) -> FiredTrigger {
        FiredTrigger {
            id: id.into(),
            narration: "（反応ビートの語り）".into(),
            recall: recall.map(|s| s.into()),
        }
    }

    /// 【id recall (上位互換)】cue が id に一致すると score 1.0 で最上位に返る。
    #[test]
    fn recall_by_id_returns_lore() {
        let got = store().recall("childhood_promise");
        assert!(!got.is_empty());
        assert_eq!(got[0].id, "childhood_promise", "exact id 一致は最上位");
        assert!(got[0].text.contains("指切り"), "伏線の本文が返る");
    }

    /// 【tag recall (上位互換)】cue が tag に一致しても最上位で hit する。
    #[test]
    fn recall_by_tag_returns_lore() {
        let got = store().recall("幼少期");
        assert!(!got.is_empty());
        assert_eq!(got[0].id, "childhood_promise", "exact tag 一致は最上位");
    }

    /// 【該当無し】無関係な cue は空を返す (捏造しない)。
    #[test]
    fn recall_miss_is_empty() {
        assert!(store().recall("竜と魔法の城砦").is_empty());
    }

    /// 【semantic】exact 一致でない cue でも、本文に bigram が重なれば cosine で hit する。
    /// 「樫の木の下の誓い」は id/tag のどれとも一致しないが、伏線本文 (樫の木/誓) と意味的に近い。
    #[test]
    fn recall_ranks_semantically_related_cue() {
        let got = store().recall("樫の木の下で誓った");
        assert!(!got.is_empty(), "exact 一致でなくても近い伏線を引く");
        assert_eq!(got[0].id, "childhood_promise", "最も近い伏線が最上位");
    }

    /// 【ランク】cue に近い方の伏線が先頭に来る (cosine 降順)。
    #[test]
    fn recall_ranks_closest_first() {
        let got = store().recall("甘い飴をなめる癖");
        assert!(!got.is_empty());
        assert_eq!(got[0].id, "alice_sweet_tooth", "性格の伏線が約束より上位");
    }

    /// 【橋渡し】発火トリガーの cue を Memoria で解決すると FiredBeat に伏線が載る。
    #[test]
    fn resolve_recall_bridges_fire_to_lore() {
        let fired = vec![fired("recall_promise", Some("childhood_promise"))];
        let beats = resolve_recall(&store(), &fired);
        assert_eq!(beats.len(), 1);
        assert_eq!(beats[0].id, "recall_promise");
        assert!(!beats[0].recalled.is_empty(), "cue が伏線に解決される");
        assert!(beats[0].recalled[0].text.contains("樫の木"), "最上位は cue に最も近い伏線");
    }

    /// 【cue 無し】recall を持たないトリガーは伏線を引かない (静的な反応ビート)。
    #[test]
    fn trigger_without_cue_recalls_nothing() {
        let beats = resolve_recall(&store(), &[fired("plain_beat", None)]);
        assert!(beats[0].recalled.is_empty());
    }

    /// 【null Memoria】`()` は常に空を返す (recall を使わない経路)。
    #[test]
    fn unit_memoria_recalls_nothing() {
        let beats = resolve_recall(&(), &[fired("recall_promise", Some("childhood_promise"))]);
        assert!(beats[0].recalled.is_empty());
    }

    /// リポジトリの `memoria/` から伏線がロードでき、ファイル名が id になる。
    #[test]
    fn loads_lore_from_repo_memoria_dir() {
        let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/memoria"));
        let store = load_lore(dir).expect("memoria/ をロードできる");
        let got = store.recall("childhood_promise");
        assert!(!got.is_empty(), "ファイル名 childhood_promise が id になる");
        assert_eq!(got[0].id, "childhood_promise");
        assert!(!got[0].text.trim().is_empty(), "伏線の本文がある");
    }

    /// 存在しないディレクトリは空 (伏線無しは正常)。
    #[test]
    fn missing_lore_dir_is_empty() {
        assert!(load_lore(Path::new("/no/such/dir/xyz")).unwrap().is_empty());
    }
}
