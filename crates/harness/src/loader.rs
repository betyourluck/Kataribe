//! 外部キャラ定義ファイル (`characters/*.yaml`) のローダ。
//!
//! 1 ファイル 1 キャラ。**ファイル名 (拡張子なし) が EntityId** になる。
//! ファイル I/O ゆえ engine ではなく harness (アプリ層) の責務。

use std::collections::BTreeMap;
use std::path::Path;

use gm_core::{CharacterDef, EntityId};

use crate::error::HarnessError;

/// `dir` 直下の `*.yaml` を各 [`CharacterDef`] として読み、`{ファイル名: def}` を返す。
/// `dir` が無ければ空 (キャラ無しシナリオは正常)。
pub fn load_characters(dir: &Path) -> Result<BTreeMap<EntityId, CharacterDef>, HarnessError> {
    let mut out = BTreeMap::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    let entries = std::fs::read_dir(dir).map_err(|e| HarnessError::CharacterLoad {
        path: dir.display().to_string(),
        detail: e.to_string(),
    })?;
    for entry in entries {
        let path = entry
            .map_err(|e| HarnessError::CharacterLoad {
                path: dir.display().to_string(),
                detail: e.to_string(),
            })?
            .path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        let id = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let text = std::fs::read_to_string(&path).map_err(|e| HarnessError::CharacterLoad {
            path: path.display().to_string(),
            detail: e.to_string(),
        })?;
        let def: CharacterDef =
            serde_yaml::from_str(&text).map_err(|e| HarnessError::CharacterLoad {
                path: path.display().to_string(),
                detail: e.to_string(),
            })?;
        out.insert(id, def);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// リポジトリの characters/ から alice が読め、ファイル名が EntityId になる。
    #[test]
    fn loads_alice_from_repo_characters_dir() {
        let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../characters"));
        let chars = load_characters(dir).expect("characters/ をロードできる");
        let alice = chars.get("alice").expect("ファイル名 alice が EntityId になる");
        assert_eq!(alice.name, "アリス");
        assert!(alice.stats.contains_key("好感度"), "好感度 stat を宣言");
        assert!(!alice.taboos.is_empty(), "硬い禁忌を持つ");
    }

    /// 存在しないディレクトリは空 (エラーにしない)。
    #[test]
    fn missing_dir_is_empty() {
        let chars = load_characters(Path::new("/no/such/dir/xyz")).unwrap();
        assert!(chars.is_empty());
    }
}
