//! package.rs — 配布単位 (フォルダ) のロードと注入。
//!
//! 「自己完結 file の純粋実行器」思想を **file → フォルダ**へ広げた脚。`packages/<name>/` が
//! 配布・管理・(将来) マーケットの単位で、フォルダ内に `package.yaml` (世界をまとめる1ファイル) +
//! `characters/` + `memoria/` + `scenarios/` + `campaign.yaml`(任意) を同梱する → zip→解凍→動く。
//!
//! file I/O と共有定義の注入ゆえ **harness の責務** (gm_core は純粋のまま不変)。package の
//! `player`/`globals` を各モジュールへ射し込むのは [`inject_cast`](crate::inject_cast) と同型。
//! 自己完結 = 参照は全てフォルダ相対、外部参照ゼロ (package.yaml 不在 / entry 不在は load 時エラー)。

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use gm_core::{FlagKey, ItemId, Scenario, SkillId, StatKey};
use serde::Deserialize;

use crate::error::HarnessError;
use crate::loader::inject_cast;

/// `packages/<name>/package.yaml` の凍結スキーマ。このパッケージの世界をまとめる1ファイル。
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PackageManifest {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub version: String,
    /// semver range (将来のマーケット互換チェック用)。今は不透明 string で保持、検証しない。
    #[serde(default)]
    pub engine: String,
    /// `"campaign.yaml"` or `"scenarios/xxx.yaml"` (フォルダ相対)。
    pub entry: String,
    /// 世界観 lore = 語りの素材 (非検証、prompt 供給)。可変状態は持てない (北極星)。
    #[serde(default)]
    pub world: String,
    /// 主人公を一度だけ宣言 → harness が各モジュールへ注入 (継承)。
    #[serde(default)]
    pub player: Option<PlayerDef>,
    /// パッケージ横断の世界フラグ宣言。
    #[serde(default)]
    pub globals: Option<Globals>,
}

/// 主人公の宣言。各モジュールの `initial_stats`/`initial_skills` へ注入される。
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PlayerDef {
    #[serde(default)]
    pub name: String,
    /// 初期数値。各モジュールへ merge する際 **package が勝つ** (モジュール跨ぎの分裂を防ぐ)。
    #[serde(default)]
    pub stats: BTreeMap<StatKey, i64>,
    /// 初期スキル (閉世界)。各モジュールへ union。
    #[serde(default)]
    pub skills: BTreeSet<SkillId>,
    /// 初期所持品 (閉世界)。各モジュールの `initial_inventory` へ union → initial_state で seed。
    #[serde(default)]
    pub items: BTreeSet<ItemId>,
    /// 主人公の性向 = 語りの素材 (非検証、prompt 供給)。surfacing は後続。
    #[serde(default)]
    pub profile: String,
}

/// パッケージ横断の宣言。
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Globals {
    /// 世界フラグ宣言。load 時に各モジュールの `allowed_flags`(使える) と
    /// `global_flags`(跨いで生きる) の両方へ union される。
    #[serde(default)]
    pub flags: BTreeSet<FlagKey>,
}

/// [`load_package`] の戻り。entry シナリオ (注入・検証済) + manifest (world/profile を語り素材として保持)。
#[derive(Debug, Clone)]
pub struct LoadedPackage {
    pub manifest: PackageManifest,
    /// entry シナリオ。`inject_cast` + [`inject_package`] + `validate` 済。
    pub scenario: Scenario,
}

/// package の `player`/`globals` を1つのモジュール (scenario) へ注入する。
///
/// gm_core 無改修 — `inject_cast` (NPC 注入) と同型の「authored 共有定義をモジュールへ射し込む」操作。
/// - **player**: package が一度宣言したものを各モジュールへ。`initial_stats` は **package 優先で merge**
///   (key 衝突は package が上書き) — 「同じ主人公がモジュール毎に HP10/HP8 へ分裂」を防ぐ (再定義禁止の実装)。
///   `initial_skills` は union。
/// - **globals.flags**: 「使える (`allowed_flags`)」と「跨いで生きる (`global_flags`)」の両方へ union。
///   これで一元宣言が閉世界検査 (`global_flags ⊆ allowed_flags`) を自動で通り、手動転記が消える。
pub fn inject_package(scenario: &mut Scenario, manifest: &PackageManifest) {
    // 世界観 (語りの素材) を注入 → scenario_brief が GM に供給。
    if !manifest.world.trim().is_empty() {
        scenario.world = manifest.world.clone();
    }
    if let Some(p) = &manifest.player {
        for (k, v) in &p.stats {
            scenario.initial_stats.insert(k.clone(), *v); // package が勝つ
        }
        scenario.initial_skills.extend(p.skills.iter().cloned());
        scenario.initial_inventory.extend(p.items.iter().cloned());
        // 主人公の設定 (name/profile) を注入 → NPC がプレイヤーを認識する材料 (語りの素材)。
        if !p.name.trim().is_empty() {
            scenario.protagonist.name = p.name.clone();
        }
        if !p.profile.trim().is_empty() {
            scenario.protagonist.profile = p.profile.clone();
        }
    }
    if let Some(g) = &manifest.globals {
        for f in &g.flags {
            scenario.allowed_flags.insert(f.clone());
            scenario.global_flags.insert(f.clone());
        }
    }
}

/// `dir/package.yaml` の **manifest だけ**を読む (entry は解決しない)。
///
/// GUI のパッケージ一覧表示用 — title/description を出すのに entry の重いロードは要らないし、
/// campaign-entry の未対応パッケージでも manifest は読めるべき。
pub fn read_manifest(dir: &Path) -> Result<PackageManifest, HarnessError> {
    let manifest_path = dir.join("package.yaml");
    let text = std::fs::read_to_string(&manifest_path).map_err(|e| HarnessError::PackageLoad {
        path: manifest_path.display().to_string(),
        detail: e.to_string(),
    })?;
    serde_yaml::from_str(&text).map_err(|e| HarnessError::PackageLoad {
        path: manifest_path.display().to_string(),
        detail: e.to_string(),
    })
}

/// パッケージフォルダを読む。`dir/package.yaml` → manifest → entry を解決して scenario を組む。
///
/// 自己完結の保証: package.yaml 不在 / entry 不在 / cast の定義不在は **load 時エラー**。
/// (campaign-entry = entry が campaign.yaml の時の複数モジュール束ねは後続。今は単一シナリオ entry。)
pub fn load_package(dir: &Path) -> Result<LoadedPackage, HarnessError> {
    let manifest = read_manifest(dir)?;

    let entry_path = dir.join(&manifest.entry);
    let entry_text = std::fs::read_to_string(&entry_path).map_err(|e| HarnessError::PackageLoad {
        path: entry_path.display().to_string(),
        detail: format!("entry を読めない: {e}"),
    })?;
    let mut scenario = Scenario::from_yaml(&entry_text).map_err(|e| HarnessError::PackageLoad {
        path: entry_path.display().to_string(),
        detail: e.to_string(),
    })?;

    // 自己完結: cast はフォルダ内 characters/ から注入 (定義不在はエラー)。
    inject_cast(&mut scenario, &dir.join("characters"))?;
    // package の player/globals を注入。
    inject_package(&mut scenario, &manifest);
    // 整合性 (幻フラグ/幻 goal を弾く)。globals union 後に走らせるので閉世界が通る。
    let errs = scenario.validate();
    if !errs.is_empty() {
        return Err(HarnessError::PackageLoad {
            path: entry_path.display().to_string(),
            detail: format!("scenario 整合性エラー: {errs:?}"),
        });
    }
    Ok(LoadedPackage { manifest, scenario })
}

// =============================================================================
// PoC: 配布単位フォルダのロードと注入 (Red→Green)
// 「package で一度宣言 → 各モジュールへ注入」を実証。gm_core 無改修。
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// dogfood パッケージ (packages/houkago)。
    fn pkg_dir() -> PathBuf {
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../packages/houkago")).to_path_buf()
    }

    /// 【結線の核心】package をロードすると player/globals が entry シナリオへ注入される。
    /// player は package が勝ち (HP 分裂防止)、globals は allowed_flags+global_flags へ union。
    #[test]
    fn load_package_injects_player_and_globals_and_is_self_contained() {
        let loaded = load_package(&pkg_dir()).expect("package をロードできる");
        assert_eq!(loaded.manifest.title, "放課後の教室");
        assert_eq!(loaded.manifest.entry, "scenarios/classroom.yaml");

        // player: classroom.yaml の hp:0 を package の hp:10 が上書き (package が勝つ=分裂防止)。
        assert_eq!(
            loaded.scenario.initial_stats.get("hp"),
            Some(&10),
            "package の player.stats が scenario へ注入され勝つ"
        );
        // globals: met_moka が「使える」と「跨いで生きる」の両方へ union。
        assert!(loaded.scenario.allowed_flags.contains("met_moka"), "allowed_flags へ union");
        assert!(loaded.scenario.global_flags.contains("met_moka"), "global_flags へ union");
        // 自己完結: cast の moka が同梱 characters/ から注入される。
        assert!(loaded.scenario.characters.contains_key("moka"), "フォルダ内 characters/ から注入");
        // 注入後も整合 (閉世界検査が通る)。
        assert!(loaded.scenario.validate().is_empty());
        // 語り素材 (world / 主人公設定) が scenario へ注入され GM に供給される (NPC 認識の材料)。
        assert!(!loaded.scenario.world.trim().is_empty(), "world が scenario へ注入される");
        assert!(!loaded.scenario.protagonist.name.trim().is_empty(), "主人公の呼称が注入される");
        assert!(!loaded.scenario.protagonist.profile.trim().is_empty(), "主人公の設定が注入される");
    }

    /// 【自己完結検査】package.yaml が無いフォルダはエラー (配布物の体裁を満たさない)。
    #[test]
    fn missing_manifest_is_error() {
        assert!(load_package(Path::new("/no/such/package/xyz")).is_err());
    }

    /// 【manifest 単独読み】GUI 一覧用に entry を解決せず title/description を読める。
    /// campaign-entry (escape) でも manifest は読める (load_package は未対応でも一覧には出せる)。
    #[test]
    fn read_manifest_reads_metadata_without_resolving_entry() {
        let m = read_manifest(&pkg_dir()).expect("manifest を読める");
        assert_eq!(m.title, "放課後の教室");
        assert_eq!(m.entry, "scenarios/classroom.yaml");
        // escape は campaign-entry だが manifest は読める。
        let escape = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../packages/escape"));
        let me = read_manifest(escape).expect("campaign パッケージの manifest も読める");
        assert_eq!(me.entry, "campaign.yaml");
    }

    /// 【merge 規則】scenario が player stat を宣言していても package が勝つ (分裂を防ぐ)。
    /// globals は両フラグ集合へ union される (一元宣言が閉世界を通る)。
    #[test]
    fn inject_package_player_wins_and_globals_union_both_sets() {
        let mut sc = Scenario::from_yaml(concat!(
            "title: t\nstart: a\ninitial_stats: { hp: 8 }\nallowed_flags: []\n",
            "locations:\n  a: { description: d, items: {}, exits: [] }\n",
            "goal: { kind: always }\n"
        ))
        .unwrap();
        let manifest = PackageManifest {
            entry: "x".into(),
            world: "現代日本の高校。".into(),
            player: Some(PlayerDef {
                name: "先生".into(),
                profile: "高校教師。".into(),
                stats: BTreeMap::from([("hp".to_string(), 10)]),
                items: BTreeSet::from(["chalk".to_string()]),
                ..Default::default()
            }),
            globals: Some(Globals { flags: BTreeSet::from(["w".to_string()]) }),
            ..Default::default()
        };
        inject_package(&mut sc, &manifest);
        assert_eq!(sc.initial_stats.get("hp"), Some(&10), "package が勝つ (8→10)");
        assert!(sc.initial_inventory.contains("chalk"), "package の player.items が initial_inventory へ注入");
        assert!(sc.initial_state(1).has_item("player", "chalk"), "initial_state で player に seed される");
        assert!(
            sc.allowed_flags.contains("w") && sc.global_flags.contains("w"),
            "globals は allowed_flags と global_flags の両方へ union"
        );
        // 語り素材 (world / 主人公) も scenario へ注入され、NPC がプレイヤーを認識できる。
        assert_eq!(sc.world, "現代日本の高校。", "world が注入される");
        assert_eq!(sc.protagonist.name, "先生", "主人公の呼称が注入される");
        assert_eq!(sc.protagonist.profile, "高校教師。", "主人公の設定が注入される");
    }
}
