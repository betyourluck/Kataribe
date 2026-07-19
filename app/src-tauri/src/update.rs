//! spec 17: パッケージ更新 — 出所メタ (`.kataribe_source.json`) / tree_hash / 残骸掃除。
//!
//! 書庫から取得したパッケージの「由来」をフォルダ自身に記録し、更新検知 (content_hash) と
//! ローカル編集検知 (tree_hash) の基準にする。メタの無いフォルダ (手動配置・自作) は
//! 更新機構が構造的に触らない (聖域)。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// 出所メタのファイル名 (展開フォルダ直下)。
pub const SOURCE_META_FILE: &str = ".kataribe_source.json";

/// tree_hash が無視するファイル名 (basename 一致・どの階層でも)。
/// OS が勝手に落とす付随ファイルで「編集あり」の偽陽性を作らない (rev2 B-6)。
/// メタ自身も除外 (メタの書き込みが tree_hash を変えない)。
const TREE_HASH_EXCLUDE: &[&str] = &[SOURCE_META_FILE, ".DS_Store", "Thumbs.db", "desktop.ini"];

/// 書庫からの取得の出所メタ (spec 17 機構①)。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceMeta {
    /// 取得元サイト (正規化済み)。check/update は**現在設定の siteUrl と一致する時のみ**
    /// 照会に使う (rev2 A-4 SSRF 遮断 — 細工メタを手動配置されても照会先は登録サイトだけ)。
    pub site_url: String,
    /// 書庫のパッケージ id (UUID)。
    pub id: String,
    /// 取得時の package.yaml version の写し (人間向け表示用)。欠落は None → 表示「(不明)」(rev2 B-9)。
    pub version: Option<String>,
    /// ダウンロードした zip の sha256 (クライアント自前計算・サーバ申告と一致検証済み)。
    /// 更新検知の基準: サーバの現在 sha256 と違えば「更新あり」。
    pub content_hash: String,
    /// 展開直後のフォルダ内容の正規化ハッシュ ([`tree_hash`])。ローカル編集検知の基準。
    pub tree_hash: String,
    /// 取得時刻 (unix 秒)。表示は提示層が locale 変換する (chrono 依存を足さない)。
    pub installed_at_unix: u64,
}

/// フォルダの出所メタを読む。無い/壊れているは None (= 手動配置扱い・更新対象外)。
#[allow(dead_code)] // Phase C (check_package_updates / update_site_package) が使う読み口。
pub fn read_source_meta(dir: &Path) -> Option<SourceMeta> {
    let raw = std::fs::read_to_string(dir.join(SOURCE_META_FILE)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// フォルダへ出所メタを書く (tmp→rename の原子書き込みはサイズが小さいので不要と判断)。
pub fn write_source_meta(dir: &Path, meta: &SourceMeta) -> Result<(), String> {
    let raw = serde_json::to_string_pretty(meta).map_err(|e| format!("メタの整形に失敗: {e}"))?;
    std::fs::write(dir.join(SOURCE_META_FILE), raw).map_err(|e| format!("メタの書き込みに失敗: {e}"))
}

/// ファイルの sha256 (hex 小文字)。DL した zip の検証・記録に使う。
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("読めません: {e}"))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut f, &mut hasher).map_err(|e| format!("読み込みに失敗: {e}"))?;
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// フォルダ内容の正規化ハッシュ (spec 17 rev2 B-6 の凍結式)。
///
/// ```text
/// files = walk(root) の通常ファイルのみ (TREE_HASH_EXCLUDE を basename 除外・
///         空ディレクトリと symlink は無視)
/// entry = 相対パス(UTF-8・'/' 区切り) + '\0' + hex(sha256(内容)) + '\n'
/// tree_hash = hex(sha256(相対パスの辞書順 (バイト列比較) で entry を連結))
/// ```
///
/// install 直後に記録し、更新直前に再計算して比較する = ローカル編集検知。
/// 比較は同一マシン内で閉じるのでパスの Unicode 正規化はしない。
pub fn tree_hash(root: &Path) -> Result<String, String> {
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    collect_files(root, root, &mut files)?;
    // 相対パスの辞書順 (バイト列比較) — walk の順序に依存しない決定論。
    files.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut hasher = Sha256::new();
    for (rel, path) in files {
        let file_hash = sha256_file(&path)?;
        hasher.update(rel.as_bytes());
        hasher.update(b"\0");
        hasher.update(file_hash.as_bytes());
        hasher.update(b"\n");
    }
    Ok(hex(&hasher.finalize()))
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("フォルダを走査できません: {e}"))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("フォルダを走査できません: {e}"))?;
        let path = entry.path();
        // symlink は無視 (extract が symlink を作らないことと整合)。file_type は
        // symlink を追わない (metadata と違い実体でなくリンク自身を見る)。
        let ft = entry.file_type().map_err(|e| format!("種別を判定できません: {e}"))?;
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            collect_files(root, &path, out)?;
            continue;
        }
        if !ft.is_file() {
            continue;
        }
        let name = entry.file_name();
        if TREE_HASH_EXCLUDE.iter().any(|x| name.to_string_lossy() == *x) {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .map_err(|_| "相対パスの導出に失敗".to_string())?
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        out.push((rel, path));
    }
    Ok(())
}

/// クラッシュ残骸の掃除 (spec 17 rev2 A-3)。`packages_dir` (app_data/packages) 直下を走査:
/// - `.update_tmp_*` → 無条件削除 (書きかけ展開の残骸)。
/// - `X.bak` が在り `X` が無い → `X.bak → X` に自動復旧 (スワップ中間でのクラッシュ)。
/// - `X.bak` と `X` が両方在る → `.bak` を削除 (スワップ完了後の削除だけ失敗した残骸)。
///
/// 失敗は握り潰す (掃除は best-effort。一覧表示を壊さない)。起動時/一覧読込時に呼ぶ =
/// Phase C の update より先に入れておくことで、update のどんな失敗にも耐える器になる。
pub fn cleanup_leftovers(packages_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(packages_dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(".update_tmp_") {
            let _ = std::fs::remove_dir_all(&path);
            continue;
        }
        if let Some(stem) = name.strip_suffix(".bak") {
            let original = packages_dir.join(stem);
            if original.exists() {
                // スワップは完了している (新が生きている) → 残骸の bak を破棄。
                let _ = std::fs::remove_dir_all(&path);
            } else {
                // スワップ中間でクラッシュ → 旧を復旧 (旧フォルダが必ず生き残る不変条件)。
                let _ = std::fs::rename(&path, &original);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kataribe_update_test_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 【sha256 の固定ベクトル】既知の入力で既知の hex (実装の自己検証)。
    #[test]
    fn sha256_known_vector() {
        let dir = temp_dir("vector");
        let f = dir.join("hello.txt");
        std::fs::write(&f, b"hello").unwrap();
        assert_eq!(
            sha256_file(&f).unwrap(),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 【tree_hash の決定論 (rev2 B-6)】作成順に依らず同一 / 内容変更で変わる /
    /// 無視リスト (.DS_Store / メタ自身) は影響しない / 入れ子ディレクトリを含む。
    #[test]
    fn tree_hash_is_deterministic_and_respects_excludes() {
        let a = temp_dir("tree_a");
        std::fs::create_dir_all(a.join("scenarios")).unwrap();
        std::fs::write(a.join("package.yaml"), "title: t").unwrap();
        std::fs::write(a.join("scenarios/main.yaml"), "start: r").unwrap();

        // 逆順で作った同内容フォルダ。
        let b = temp_dir("tree_b");
        std::fs::create_dir_all(b.join("scenarios")).unwrap();
        std::fs::write(b.join("scenarios/main.yaml"), "start: r").unwrap();
        std::fs::write(b.join("package.yaml"), "title: t").unwrap();

        let ha = tree_hash(&a).unwrap();
        assert_eq!(ha, tree_hash(&b).unwrap(), "作成順に依らない");

        // 無視リストは hash を変えない (OS 付随ファイル + メタ自身)。
        std::fs::write(a.join(".DS_Store"), b"junk").unwrap();
        std::fs::write(a.join(SOURCE_META_FILE), "{}").unwrap();
        std::fs::write(a.join("scenarios/Thumbs.db"), b"junk").unwrap();
        assert_eq!(ha, tree_hash(&a).unwrap(), "無視リストは影響しない");

        // 内容変更・ファイル追加は検知する (編集あり)。
        std::fs::write(a.join("package.yaml"), "title: 変更").unwrap();
        assert_ne!(ha, tree_hash(&a).unwrap(), "内容変更で変わる");

        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    /// 【SourceMeta の roundtrip】書いて読んで同値。壊れた JSON は None (手動配置扱い)。
    #[test]
    fn source_meta_roundtrip_and_corrupt_is_none() {
        let dir = temp_dir("meta");
        let meta = SourceMeta {
            site_url: "https://kataribe.outcasts.jp".into(),
            id: "550e8400-e29b-41d4-a716-446655440000".into(),
            version: Some("0.2".into()),
            content_hash: "abc".into(),
            tree_hash: "def".into(),
            installed_at_unix: 1_800_000_000,
        };
        write_source_meta(&dir, &meta).unwrap();
        assert_eq!(read_source_meta(&dir), Some(meta));

        std::fs::write(dir.join(SOURCE_META_FILE), "{ broken").unwrap();
        assert_eq!(read_source_meta(&dir), None, "壊れたメタは手動配置扱い");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 【残骸掃除の 3 分岐 (rev2 A-3)】tmp 削除 / bak 単独 → 復旧 / 両在 → bak 破棄。
    #[test]
    fn cleanup_leftovers_three_branches() {
        let root = temp_dir("cleanup");
        // ① 書きかけ展開の残骸。
        std::fs::create_dir_all(root.join(".update_tmp_xyz/inner")).unwrap();
        // ② スワップ中間クラッシュ: bak だけが残る。
        std::fs::create_dir_all(root.join("pkg_a.bak")).unwrap();
        std::fs::write(root.join("pkg_a.bak/package.yaml"), "title: a").unwrap();
        // ③ スワップ完了後の bak 削除失敗: 両方在る。
        std::fs::create_dir_all(root.join("pkg_b")).unwrap();
        std::fs::create_dir_all(root.join("pkg_b.bak")).unwrap();

        cleanup_leftovers(&root);

        assert!(!root.join(".update_tmp_xyz").exists(), "tmp は無条件削除");
        assert!(root.join("pkg_a").exists(), "bak 単独 → 旧に復旧");
        assert!(!root.join("pkg_a.bak").exists());
        assert!(
            root.join("pkg_a/package.yaml").exists(),
            "復旧は中身ごと (rename)"
        );
        assert!(root.join("pkg_b").exists(), "両在 → 新が生存");
        assert!(!root.join("pkg_b.bak").exists(), "両在 → bak 破棄");
        let _ = std::fs::remove_dir_all(&root);
    }
}
