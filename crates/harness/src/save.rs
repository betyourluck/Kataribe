//! セーブ / ロード (spec 07) — セッションの器を 1 file にする。
//!
//! セーブは正本 state の**スナップショット**であり、生きた可変コピーではない
//! (ロードした瞬間に唯一の正本へ戻る = split-brain は構造的に起きない)。
//! 骨格 (scenario/campaign) は保存せず `content` の参照だけを刻む — 単一真実源・
//! セーブ肥大回避・content 修正がロード後に生きる。content が非互換に変わった場合の
//! 破れは engine の閉世界却下がそのまま守る。
//!
//! **語りの継続性も保存する**: state だけでは再開時に「経緯を忘れる GM」に戻る
//! (chronicle / last_narration / pending_* は state-truth と独立の第二チャネル、#27 系)。

use std::path::Path;

use gm_core::{CheckOutcome, GameState};
use serde::{Deserialize, Serialize};

use crate::campaign::{CampaignMemory, ModuleId};
use crate::error::HarnessError;
use crate::memoria::MemoryFragment;
use crate::synopsis::Synopsis;
use crate::turn::TurnLog;

/// セーブ形式の現行版。読み込み時に不一致なら拒否する (v1 は実験的)。
pub const SAVE_VERSION: u32 = 1;

/// 何を遊んでいたか (CLI/GUI の起動形に対応)。ロード側はこれで再ロード経路を選ぶ。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SavedContent {
    /// `play --package <dir>` / GUI のパッケージ (単発 scenario entry)。
    Package { path: String },
    /// `play --campaign <file>` (campaign file 直指定)。
    Campaign { path: String },
    /// `play <scenario.yaml>` (素の単一シナリオ)。
    Scenario { path: String },
}

/// セーブ 1 file = 再開に要る全て (骨格は含まない)。
///
/// フィールドは「正本 (state/campaign_memory)」と「語りの継続性 (history/last_narration/
/// pending_*)」の二群。後者を落とすと state は正しくても GM が経緯を忘れる。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSave {
    /// セーブ形式版 ([`SAVE_VERSION`])。
    pub version: u32,
    /// 再ロード元の content 参照。
    pub content: SavedContent,
    /// 記録時の package manifest version (不一致はロード側が警告。package 以外は空)。
    #[serde(default)]
    pub package_version: String,
    /// campaign 中の現在モジュール id (単発は None)。
    #[serde(default)]
    pub module: Option<ModuleId>,
    /// 正本 (rng/turn/fired/votes/flag_turns/taken_items/present_overrides 込み)。
    pub state: GameState,
    /// spec 02 の persistent フラグ蓄積 (campaign 再訪の記憶)。
    #[serde(default)]
    pub campaign_memory: CampaignMemory,
    /// 経緯ログ (chronicle) 全量。GM の中期記憶。
    #[serde(default)]
    pub history: Vec<TurnLog>,
    /// 直前の語り (継続性の持ち越し)。
    #[serde(default)]
    pub last_narration: String,
    /// 直前ターンの判定結果 (次ターン還流分)。
    #[serde(default)]
    pub pending_checks: Vec<CheckOutcome>,
    /// 発火済み recall の持ち越し (fragment 丸ごと = memoria/ 欠損でもロード可能)。
    #[serde(default)]
    pub pending_lore: Vec<MemoryFragment>,
    /// あらすじ (spec 10) — 圧縮済み章 + 遷移契機の凍結リトライ範囲 (pending_transition)。
    /// history と同一セーブで snapshot されるので resume では常に整合する。
    #[serde(default)]
    pub synopsis: Synopsis,
    /// 約束事 (spec 20) — プレイヤーと GM の覚え書き。campaign 遷移でも持ち越す
    /// (章を跨いで覚える = chronicle と同じ判断)。
    #[serde(default)]
    pub facts: Vec<crate::FactEntry>,
}

/// セーブを YAML で書く。**tmp → rename の原子的置換** — 受理ターン毎の上書き運用で
/// 書きかけ file がセーブを壊さないこと (クラッシュ耐性) を保証する。
pub fn save_session(path: &Path, save: &SessionSave) -> Result<(), HarnessError> {
    let yaml = serde_yaml::to_string(save).map_err(|e| HarnessError::SessionSave {
        path: path.display().to_string(),
        detail: format!("直列化に失敗: {e}"),
    })?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, yaml).map_err(|e| HarnessError::SessionSave {
        path: tmp.display().to_string(),
        detail: e.to_string(),
    })?;
    std::fs::rename(&tmp, path).map_err(|e| HarnessError::SessionSave {
        path: path.display().to_string(),
        detail: format!("原子的置換に失敗: {e}"),
    })
}

/// セーブを読む。形式版の不一致は拒否する (v1 は実験的 — 黙って壊れた再開をしない)。
pub fn load_session(path: &Path) -> Result<SessionSave, HarnessError> {
    let text = std::fs::read_to_string(path).map_err(|e| HarnessError::SessionLoad {
        path: path.display().to_string(),
        detail: e.to_string(),
    })?;
    let save: SessionSave =
        serde_yaml::from_str(&text).map_err(|e| HarnessError::SessionLoad {
            path: path.display().to_string(),
            detail: format!("パースに失敗: {e}"),
        })?;
    if save.version != SAVE_VERSION {
        return Err(HarnessError::SessionLoad {
            path: path.display().to_string(),
            detail: format!(
                "セーブ形式版が合わない (file={}, 対応={SAVE_VERSION})",
                save.version
            ),
        });
    }
    Ok(save)
}
