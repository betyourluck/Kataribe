//! パッケージ同梱アセット (画像/音声) の解決。**ここが ID→パスの唯一の関門**。
//!
//! gm_core は不透明 string (アセット ID) を運ぶだけ。harness が package root を起点に
//! `root/{kind}/{id}` へ解決する。ID は厳格にサニタイズ — `/` や `..` を遮断し、
//! ディレクトリトラバーサルでパッケージ外を読ませない (spec 01 #5)。

use std::path::{Path, PathBuf};

/// アセットの種別 = パッケージ内のサブフォルダ。将来 voice 等へ拡張可。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetKind {
    Images,
    Audios,
}

impl AssetKind {
    /// パッケージ内のサブフォルダ名。
    pub fn dir(self) -> &'static str {
        match self {
            AssetKind::Images => "images",
            AssetKind::Audios => "audios",
        }
    }
}

/// アセット ID が安全か (`^[A-Za-z0-9._-]{1,64}$` かつ `.`/`..` 単体でない)。
/// charset が `/` を除くので ID は常に単一パス成分。`.`/`..` だけ別途弾けばトラバーサル不能。
pub fn is_valid_asset_id(id: &str) -> bool {
    let len_ok = (1..=64).contains(&id.chars().count());
    let charset_ok = id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    len_ok && charset_ok && id != "." && id != ".."
}

/// `root/{kind}/{id}` を解決する。ID 不正・ファイル不在なら `None` (寛容: 描画しないだけ)。
///
/// 戻り値は**実在する**絶対(=root 基準)パス。提示層がこれを `convertFileSrc` で URL 化する。
pub fn resolve_asset(root: &Path, kind: AssetKind, id: &str) -> Option<PathBuf> {
    if !is_valid_asset_id(id) {
        return None;
    }
    let path = root.join(kind.dir()).join(id);
    path.is_file().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 【トラバーサル遮断】`..` / `/` 含み / 空 / 長すぎる ID は無効。正常 ID だけ通す。
    #[test]
    fn asset_id_validation_blocks_traversal() {
        assert!(is_valid_asset_id("gate.png"));
        assert!(is_valid_asset_id("moka_icon-01.webp"));
        assert!(!is_valid_asset_id(".."), "親ディレクトリ参照を遮断");
        assert!(!is_valid_asset_id("."), "カレント参照を遮断");
        assert!(!is_valid_asset_id("../secret.png"), "/ を含む = 遮断 (charset 外)");
        assert!(!is_valid_asset_id("a/b.png"), "サブパスを遮断");
        assert!(!is_valid_asset_id(""), "空を遮断");
        assert!(!is_valid_asset_id(&"x".repeat(65)), "65 文字は遮断");
        assert!(is_valid_asset_id(&"x".repeat(64)), "64 文字は許可");
    }

    /// 【解決】実在する images/ のファイルだけ Some。不正 ID・不在は None。
    #[test]
    fn resolve_asset_returns_existing_files_only() {
        let dir = std::env::temp_dir().join("kataribe_asset_test_pkg");
        let images = dir.join("images");
        std::fs::create_dir_all(&images).unwrap();
        let f = images.join("bg.png");
        std::fs::write(&f, b"x").unwrap();

        assert_eq!(resolve_asset(&dir, AssetKind::Images, "bg.png"), Some(f.clone()));
        assert_eq!(resolve_asset(&dir, AssetKind::Images, "missing.png"), None, "不在は None");
        assert_eq!(resolve_asset(&dir, AssetKind::Images, ".."), None, "トラバーサルは None");
        assert_eq!(resolve_asset(&dir, AssetKind::Audios, "bg.png"), None, "別 kind は別フォルダ");

        std::fs::remove_dir_all(&dir).ok();
    }
}
