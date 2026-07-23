//! 卓の一時パッケージ中継 (spec 23 契約 `package_relay`) — 純関数部。
//!
//! **正はホストの実ファイル**。ホストは書庫の標準版でなく「この卓のために書き換えた
//! 改変版」で遊ぶことがあり、ゲストが書庫から得られるのは標準版だけなので、手持ち照合
//! 方式では改変卓が構造的に成立しない。ゆえに卓を開くときホストがパッケージを zip に
//! 固めてサーバへ**一時アップロード**し、ゲストは部屋コード (= bearer capability) で
//! 落とす。書庫への納本とは完全に別経路で、一覧に出ず恒久保存もしない
//! (サーバ側の契約は outcast `specs/30_kataribe_package_relay.md` / C-019)。
//!
//! ここに置くのは Tauri 非依存の部分だけ (zip 化・形式検査) = `cargo test` で回せる。
//! HTTP と置き場の解決は [`crate::lib`] 側のコマンド。

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use zip::write::{SimpleFileOptions, ZipWriter};

/// サーバの受入上限 (outcast Spec 30 `MAX_PACKAGE_BYTES`) の鏡。超過は手元で弾く —
/// 100MB を投げてから 413 で返されるより、固めた時点で名指しするほうが直せる。
pub const MAX_RELAY_BYTES: u64 = 100 * 1024 * 1024;

/// zip bomb 上限 — [`crate::site`] (= サーバ) と同値の鏡。
const MAX_ENTRIES: usize = 10_000;
const MAX_UNCOMPRESSED_TOTAL: u64 = 500 * 1024 * 1024;

/// zip に入れない basename。`.kataribe_source.json` は**ホスト固有の出所メタ**で、
/// ゲストのコピーの出所ではない (展開側 `site::extract_package_zip` も skip する)。
/// 残りは OS が撒くゴミ。
const EXCLUDE_NAMES: &[&str] = &[
    crate::update::SOURCE_META_FILE,
    ".DS_Store",
    "Thumbs.db",
    "desktop.ini",
];

/// 部屋コードの形式 (knock 発行の base62 22 桁 ≈131bit)。**capability そのもの**を
/// URL 片に載せる前にここで弾く。英数字だけなので、通ればパス経路も汚さない
/// (`.` `/` `\` が構造的に入り得ない = ファイル名に直接使える)。
pub fn valid_room_code(code: &str) -> bool {
    code.len() == 22 && code.chars().all(|c| c.is_ascii_alphanumeric())
}

/// 小文字 hex 64 桁か。**ネットワーク越し (ホストの hello) に来た値をキャッシュの
/// パス成分に使う**ので、経路汚染の遮断としてここを通す (遠隔から `..` を差し込ませない)。
pub fn valid_sha256(hex: &str) -> bool {
    hex.len() == 64 && hex.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// 中継 endpoint の URL を組む (`base` は正規化済みのサイト URL)。
pub fn relay_url(base: &str, room_code: &str) -> Result<String, String> {
    if !valid_room_code(room_code) {
        return Err("部屋コードの形式が不正です (英数 22 桁)".to_string());
    }
    Ok(format!("{base}/api/table/{room_code}/package"))
}

/// パッケージフォルダを **フォルダ包み形 (Wrapped)** の zip に固め、書けたバイト数を返す。
///
/// 受領側 (`site::extract_package_zip_to`) が Wrapped しか受理しないので、ここも
/// トップフォルダ 1 枚で包む。**エントリは相対パスの辞書順・タイムスタンプは zip crate の
/// 既定 (1980-01-01 固定)** ＝ 内容が同じなら**バイト列も同じ**になる。これが
/// 「同一版の再 join・翌週の同卓は再 DL なし」(sha256 キーのキャッシュ) の前提。
pub fn zip_package_folder(pkg_dir: &Path, out_zip: &Path) -> Result<u64, String> {
    match build_zip(pkg_dir, out_zip) {
        Ok(size) => Ok(size),
        Err(e) => {
            // 書きかけの zip を残さない (次回の再送で古い残骸を送らないため)。
            let _ = std::fs::remove_file(out_zip);
            Err(e)
        }
    }
}

fn build_zip(pkg_dir: &Path, out_zip: &Path) -> Result<u64, String> {
    if !pkg_dir.join("package.yaml").is_file() {
        return Err("package.yaml が見つかりません (パッケージフォルダではありません)".to_string());
    }
    let top = safe_top_name(pkg_dir);

    let mut files: Vec<(String, PathBuf)> = Vec::new();
    collect_files(pkg_dir, pkg_dir, &mut files)?;
    if files.is_empty() {
        return Err("パッケージが空です".to_string());
    }
    if files.len() > MAX_ENTRIES {
        return Err(format!("ファイル数が上限 ({MAX_ENTRIES}) を超えています"));
    }
    // 相対パスの辞書順 (バイト列比較) — walk の順序に依存しない決定論 (update::tree_hash と同型)。
    files.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));

    // 拒否拡張子の先回り検査。サーバも同じ denylist で 400 にするが、手元で名指しできれば
    // 作者は直せる (二層の鏡は `site.rs` と単一定義を共有する)。
    for (rel, _) in &files {
        if crate::site::is_denied_name(rel) {
            return Err(format!("実行ファイル・スクリプトは同梱できません: {rel}"));
        }
    }

    let file = File::create(out_zip).map_err(|e| format!("一時 zip を作成できません: {e}"))?;
    let mut zw = ZipWriter::new(BufWriter::new(file));
    let opts = SimpleFileOptions::default();
    let mut total: u64 = 0;
    for (rel, path) in &files {
        let mut src = File::open(path).map_err(|e| format!("ファイルを開けません ({rel}): {e}"))?;
        zw.start_file(format!("{top}/{rel}"), opts)
            .map_err(|e| format!("zip への書き込みに失敗 ({rel}): {e}"))?;
        let n = std::io::copy(&mut src, &mut zw)
            .map_err(|e| format!("zip への書き込みに失敗 ({rel}): {e}"))?;
        total = total.saturating_add(n);
        if total > MAX_UNCOMPRESSED_TOTAL {
            return Err("パッケージが大きすぎます (展開後 500MB 超)".to_string());
        }
    }
    zw.finish().map_err(|e| format!("zip を閉じられません: {e}"))?;

    let size = std::fs::metadata(out_zip)
        .map_err(|e| format!("一時 zip を確認できません: {e}"))?
        .len();
    if size > MAX_RELAY_BYTES {
        return Err(format!(
            "パッケージが大きすぎます ({} MB — 上限 100MB)。画像を WebP・音声を Ogg にすると縮みます",
            size / (1024 * 1024)
        ));
    }
    Ok(size)
}

/// zip のトップフォルダ名。展開側は top を剥がすので**見た目だけ**の値だが、受領側の
/// 名前検査 (`site::extract_package_zip`) に落ちない字種へ寄せておく。
fn safe_top_name(pkg_dir: &Path) -> String {
    let raw = pkg_dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "package".to_string()
    } else {
        cleaned
    }
}

/// `dir` 以下の通常ファイルを (相対パス '/' 区切り, 実パス) で集める。symlink は無視
/// (展開側が symlink を作らないことと整合)。
fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("フォルダを走査できません: {e}"))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("フォルダを走査できません: {e}"))?;
        let ft = entry
            .file_type()
            .map_err(|e| format!("種別を判定できません: {e}"))?;
        if ft.is_symlink() {
            continue;
        }
        let path = entry.path();
        if ft.is_dir() {
            collect_files(root, &path, out)?;
            continue;
        }
        if !ft.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if EXCLUDE_NAMES.contains(&name.as_ref()) {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .map_err(|_| "相対パスを解決できません".to_string())?
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        out.push((rel, path));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "kataribe_relay_{tag}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 最小のパッケージフォルダを作る。
    fn make_package(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(dir.join("scenarios")).unwrap();
        std::fs::create_dir_all(dir.join("images")).unwrap();
        std::fs::write(dir.join("package.yaml"), "title: t\nentry: scenarios/main.yaml\n").unwrap();
        std::fs::write(dir.join("scenarios/main.yaml"), "title: s\nstart: a\n").unwrap();
        std::fs::write(dir.join("images/bg.webp"), b"\x00binary").unwrap();
        dir
    }

    /// 【中核】固めた zip は Wrapped で、**受領側の展開器がそのまま受理する**
    /// (ホストが固める → ゲストが展開する の結合の表明)。加えて同じ内容なら
    /// **バイト列が同一** = sha256 キャッシュが翌週も効く。
    #[test]
    fn zip_is_wrapped_deterministic_and_extractable() {
        let root = temp_dir("zip");
        let pkg = make_package(&root, "MyPack");
        let a = root.join("a.zip");
        let b = root.join("b.zip");
        let size = zip_package_folder(&pkg, &a).unwrap();
        zip_package_folder(&pkg, &b).unwrap();
        assert!(size > 0);
        assert_eq!(
            std::fs::read(&a).unwrap(),
            std::fs::read(&b).unwrap(),
            "同じ内容なら同じバイト列 (キャッシュ鍵が安定する)"
        );

        // 受領側 (ゲスト) の展開器を実際に通す。
        let dest = root.join("cache/deadbeef");
        let out = crate::site::extract_package_zip_to(&a, &dest).unwrap();
        assert_eq!(out, dest, "指定パス自身が package root (二重構造にならない)");
        assert!(out.join("package.yaml").is_file());
        assert!(out.join("scenarios/main.yaml").is_file());
        assert!(out.join("images/bg.webp").is_file(), "バイナリも入る");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 【ホスト固有メタの除外】`.kataribe_source.json` はホストの出所であってゲストの
    /// 出所ではない — zip に入れない (展開側の skip と二層)。
    #[test]
    fn source_meta_is_not_bundled() {
        let root = temp_dir("meta");
        let pkg = make_package(&root, "MyPack");
        std::fs::write(
            pkg.join(crate::update::SOURCE_META_FILE),
            "{\"site_url\":\"https://example\"}",
        )
        .unwrap();
        let zip = root.join("p.zip");
        zip_package_folder(&pkg, &zip).unwrap();

        let dest = root.join("out");
        let out = crate::site::extract_package_zip_to(&zip, &dest).unwrap();
        assert!(
            !out.join(crate::update::SOURCE_META_FILE).exists(),
            "ホストの出所メタは渡さない"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 【拒否拡張子】サーバに投げる前に手元で名指しして弾く (二層の鏡)。
    #[test]
    fn denied_extension_is_rejected_before_upload() {
        let root = temp_dir("deny");
        let pkg = make_package(&root, "MyPack");
        std::fs::write(pkg.join("tool.exe"), b"x").unwrap();
        let zip = root.join("p.zip");
        let err = zip_package_folder(&pkg, &zip).unwrap_err();
        assert!(err.contains("実行ファイル"), "{err}");
        assert!(!zip.exists(), "書きかけの zip を残さない");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 【capability の形式検査】部屋コードと sha256 は URL 片・キャッシュのパス成分に
    /// なるので、遠隔由来の値をここで止める。
    #[test]
    fn room_code_and_hash_formats_are_enforced() {
        assert!(valid_room_code("aB3xY9zQ1wE5rT7yU2iO4p"));
        for bad in [
            "",
            "short",
            "aB3xY9zQ1wE5rT7yU2iO4",   // 21 桁
            "aB3xY9zQ1wE5rT7yU2iO4pQ", // 23 桁
            "aB3xY9zQ1wE5rT7yU2iO4.",  // 記号
            "../../etc/passwd______",
        ] {
            assert!(!valid_room_code(bad), "拒否されるべき: {bad}");
            assert!(relay_url("https://x", bad).is_err());
        }
        assert_eq!(
            relay_url("https://kataribe.example", "aB3xY9zQ1wE5rT7yU2iO4p").unwrap(),
            "https://kataribe.example/api/table/aB3xY9zQ1wE5rT7yU2iO4p/package"
        );

        assert!(valid_sha256(&"a".repeat(64)));
        assert!(valid_sha256(
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        ));
        for bad in [
            "",
            &"a".repeat(63),
            &"a".repeat(65),
            &"A".repeat(64), // 大文字は正規形でない
            "../../../../../../../../../../../../../../../../../../../../../..",
        ] {
            assert!(!valid_sha256(bad), "拒否されるべき: {bad}");
        }
    }
}
