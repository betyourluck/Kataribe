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
//! [`Memoria`] trait に対して書く。第一実装 [`LoreStore`] は **tag/id 一致の決定論 recall**。
//! embedding ベースの semantic recall 版は同 trait の裏で差し替えられる
//! (`ScriptedProposer` → `LlmClient` と同じ swap パス)。`()` は「recall しない」null 実装。

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

/// 第一実装: ロード済み lore 断片への **tag/id 一致の決定論 recall**。
///
/// embedding semantic 版に差し替えても [`Memoria`] の利用側 (resolve_recall / CLI) は無変更。
#[derive(Debug, Clone, Default)]
pub struct LoreStore {
    fragments: Vec<MemoryFragment>,
}

impl LoreStore {
    pub fn new(fragments: Vec<MemoryFragment>) -> Self {
        Self { fragments }
    }

    pub fn len(&self) -> usize {
        self.fragments.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fragments.is_empty()
    }
}

impl Memoria for LoreStore {
    fn recall(&self, cue: &str) -> Vec<MemoryFragment> {
        // id 完全一致を優先、無ければ tag 一致。決定論順 (authored 順)。
        self.fragments
            .iter()
            .filter(|f| f.id == cue || f.tags.iter().any(|t| t == cue))
            .cloned()
            .collect()
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

    /// 【id recall】cue が id に一致すると、その伏線が返る。
    #[test]
    fn recall_by_id_returns_lore() {
        let got = store().recall("childhood_promise");
        assert_eq!(got.len(), 1);
        assert!(got[0].text.contains("指切り"), "伏線の本文が返る");
    }

    /// 【tag recall】cue が tag に一致しても hit する (semantic surface)。
    #[test]
    fn recall_by_tag_returns_lore() {
        let got = store().recall("幼少期");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "childhood_promise");
    }

    /// 【該当無し】無関係な cue は空を返す (捏造しない)。
    #[test]
    fn recall_miss_is_empty() {
        assert!(store().recall("存在しない伏線").is_empty());
    }

    /// 【橋渡し】発火トリガーの cue を Memoria で解決すると FiredBeat に伏線が載る。
    #[test]
    fn resolve_recall_bridges_fire_to_lore() {
        let fired = vec![fired("recall_promise", Some("childhood_promise"))];
        let beats = resolve_recall(&store(), &fired);
        assert_eq!(beats.len(), 1);
        assert_eq!(beats[0].id, "recall_promise");
        assert_eq!(beats[0].recalled.len(), 1, "cue が伏線に解決される");
        assert!(beats[0].recalled[0].text.contains("樫の木"));
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
        let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../memoria"));
        let store = load_lore(dir).expect("memoria/ をロードできる");
        let got = store.recall("childhood_promise");
        assert_eq!(got.len(), 1, "ファイル名 childhood_promise が id になる");
        assert!(!got[0].text.trim().is_empty(), "伏線の本文がある");
    }

    /// 存在しないディレクトリは空 (伏線無しは正常)。
    #[test]
    fn missing_lore_dir_is_empty() {
        assert!(load_lore(Path::new("/no/such/dir/xyz")).unwrap().is_empty());
    }
}
