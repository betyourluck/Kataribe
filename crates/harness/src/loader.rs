//! 外部キャラ定義ファイル (`characters/*.yaml`) のローダ。
//!
//! 1 ファイル 1 キャラ。**ファイル名 (拡張子なし) が EntityId** になる。
//! ファイル I/O ゆえ engine ではなく harness (アプリ層) の責務。

use std::collections::BTreeMap;
use std::path::Path;

use gm_core::{CharacterDef, EntityId, Scenario};

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

/// `scenario.cast` で宣言された外部キャラを `dir/{id}.yaml` から注入する。
///
/// **`cast` に挙げられた entity だけ**を注入する (全シナリオへの無差別注入を防ぐ = alice が密室脱出に
/// 混入する問題の修正)。inline `characters` に在る entity はそちらが優先。`cast` が空なら何もしない。
/// cast に挙げたのに定義ファイルが無ければエラー (宣言と実体の乖離を黙認しない)。
pub fn inject_cast(scenario: &mut Scenario, dir: &Path) -> Result<(), HarnessError> {
    if scenario.cast.is_empty() {
        return Ok(());
    }
    let available = load_characters(dir)?;
    // cast を先に clone して scenario への可変借用と衝突させない。
    let cast: Vec<EntityId> = scenario.cast.iter().cloned().collect();
    for id in cast {
        if scenario.characters.contains_key(&id) {
            continue; // inline 優先
        }
        let def = available.get(&id).ok_or_else(|| HarnessError::CharacterLoad {
            path: dir.join(format!("{id}.yaml")).display().to_string(),
            detail: format!("cast '{id}' の定義ファイルが見つからない"),
        })?;
        scenario.characters.insert(id.clone(), def.clone());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// fixtures の characters/ から alice が読め、ファイル名が EntityId になる。
    #[test]
    fn loads_alice_from_repo_characters_dir() {
        let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/characters"));
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

    fn repo_chars_dir() -> std::path::PathBuf {
        // alice 等 harness 統合テスト用のキャラ fixture (フラット characters/ から移行)。
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/characters")).to_path_buf()
    }

    /// 【cast 宣言】cast に挙げた entity だけが外部ファイルから注入される。
    #[test]
    fn inject_cast_loads_declared_only() {
        // 邂逅シナリオ (inline キャラ無し、cast: [alice])。
        const HEROINE_MEET: &str = include_str!("../fixtures/heroine_meet.yaml");
        let mut sc = Scenario::from_yaml(HEROINE_MEET).unwrap();
        assert!(sc.characters.is_empty(), "注入前は登場人物が居ない");
        inject_cast(&mut sc, &repo_chars_dir()).expect("cast の注入が成功する");
        assert!(sc.characters.contains_key("alice"), "cast の alice が注入される");
    }

    /// 【混入しない】cast を宣言しないシナリオには、外部キャラが一切注入されない
    /// (alice が密室脱出に混入する問題の回帰防止)。
    #[test]
    fn no_cast_means_no_injection() {
        const LOCKED_ROOM: &str = include_str!("../fixtures/locked_room.yaml");
        let mut sc = Scenario::from_yaml(LOCKED_ROOM).unwrap();
        inject_cast(&mut sc, &repo_chars_dir()).expect("cast 空でも成功する");
        assert!(sc.characters.is_empty(), "cast 未宣言なら alice は混入しない");
    }

    /// 【classroom シナリオの整合性】cast [moka] が外部ファイルから注入され、start が実在し、
    /// goal が参照する moka の 好感度 stat が初期化される (start≠location バグの回帰防止)。
    #[test]
    fn classroom_injects_moka_and_is_coherent() {
        // 配布パッケージ houkago が classroom galge の正本 (フラット scenarios/ から移行済)。
        const CLASSROOM: &str = include_str!("../../../packages/houkago/scenarios/classroom.yaml");
        let houkago_chars =
            Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../packages/houkago/characters"));
        let mut sc = Scenario::from_yaml(CLASSROOM).unwrap();
        inject_cast(&mut sc, houkago_chars).expect("cast [moka] の注入が成功する");
        assert!(sc.characters.contains_key("moka"), "moka が characters/moka.yaml から注入される");
        assert!(
            sc.location(&sc.start).is_some(),
            "start が指す場所が定義されている (start={} ≠ location の不整合を防ぐ)",
            sc.start
        );
        // goal が参照する moka の 好感度 stat が宣言されている (初期値は作者が変えうるので値は固定しない)。
        assert!(sc.knows_stat("moka", "好感度"), "moka の 好感度 stat が宣言されている");
        assert!(sc.validate().is_empty(), "整合性チェックが通る");
    }

    /// cast に挙げたのに定義ファイルが無ければエラー (宣言と実体の乖離を黙認しない)。
    #[test]
    fn inject_cast_missing_definition_errors() {
        let mut sc = Scenario::from_yaml(concat!(
            "title: t\nstart: a\ncast: [ghost]\n",
            "locations:\n  a:\n    description: d\n    exits: []\n",
            "goal: { kind: location_is, at: a }\n"
        ))
        .unwrap();
        assert!(inject_cast(&mut sc, &repo_chars_dir()).is_err());
    }
}
