//! 配布サイト「Kataribe 書庫」との結線 — 純関数部 (spec 05 Phase C)。
//!
//! zip の検証 + ローカル展開。**サーバを信用しない二層**: サイト側 (outcast Spec 23) が
//! 受入時に検証していても、クライアント側でも zip slip / 構造 / 拒否拡張子を検査してから
//! 展開する。配布物はサーバが**フォルダ包み形 (Wrapped) に正規化して保存する契約**
//! (spec 05「zip 契約」) なので、展開は Wrapped のみ受理する — それ以外は書庫由来の
//! 配布物ではないか改竄されている。
//!
//! Tauri 非依存 (fs と zip だけ) なので cargo test で単体検証できる。

use std::collections::BTreeSet;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use zip::read::ZipArchive;

/// zip bomb 上限 — サーバ (outcast Spec 23 F7) と同値の鏡。
const MAX_ENTRIES: usize = 10_000;
const MAX_UNCOMPRESSED_TOTAL: u64 = 500 * 1024 * 1024;

/// 拒否拡張子 denylist — サーバ (outcast Spec 23 F8) と同値の鏡。二層目の防衛。
const DENIED_EXTENSIONS: &[&str] = &[
    "exe", "dll", "so", "dylib", "bat", "cmd", "com", "scr", "msi", "ps1", "vbs", "sh", "jar",
    "app", "js", "wasm",
];

/// zip 内フォルダ名 (top) に許さない文字。top は単一コンポーネントなので `/` は来ないが、
/// Windows のパス特殊文字と制御文字は展開先パスの安全のため拒否する (リネームでなく拒否 —
/// サーバが sanitize 済みの契約なので、ここに来る名前は改竄シグナル)。
fn top_name_is_safe(name: &str) -> bool {
    !name.is_empty()
        && !name
            .chars()
            .any(|c| matches!(c, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control())
        && name != "." && name != ".."
}

/// zip を検証して `dest_root` 直下に展開し、生まれたパッケージフォルダの絶対パスを返す。
///
/// 受理する構造は Wrapped のみ: zip 直下がディレクトリ 1 つで、その中に `package.yaml`。
/// 展開先が既に在る場合は `名前_2`, `名前_3`, … と衝突回避する (再取得で上書きしない —
/// 進行中のセーブが指す旧フォルダを壊さない)。
pub fn extract_package_zip(zip_path: &Path, dest_root: &Path) -> Result<PathBuf, String> {
    let file = File::open(zip_path).map_err(|e| format!("ダウンロードファイルを開けません: {e}"))?;
    let mut archive = ZipArchive::new(BufReader::new(file))
        .map_err(|_| "zip として読み込めません".to_string())?;

    if archive.len() > MAX_ENTRIES {
        return Err(format!("エントリ数が上限 ({MAX_ENTRIES}) を超えています"));
    }

    // --- 検証パス (central directory の走査のみ、展開しない) ---
    let mut names: Vec<String> = Vec::with_capacity(archive.len());
    let mut total_uncompressed: u64 = 0;
    for i in 0..archive.len() {
        let entry = archive
            .by_index_raw(i)
            .map_err(|e| format!("zip の読み取りに失敗しました: {e}"))?;
        let name = entry.name().to_string();
        if entry.encrypted() {
            return Err(format!("暗号化された zip は受け付けられません: {name}"));
        }
        // zip slip: `..` / 絶対パス / ドライブレターを enclosed_name で一括排除。
        if entry.enclosed_name().is_none() {
            return Err(format!("不正なエントリパスが含まれています: {name}"));
        }
        total_uncompressed = total_uncompressed.saturating_add(entry.size());
        if total_uncompressed > MAX_UNCOMPRESSED_TOTAL {
            return Err("展開後サイズが上限 (500MB) を超えています".to_string());
        }
        if !entry.is_dir() {
            if let Some(ext) = name.rsplit('.').next() {
                if name.contains('.') && DENIED_EXTENSIONS.contains(&ext.to_lowercase().as_str()) {
                    return Err(format!("実行ファイル・スクリプトは同梱できません: {name}"));
                }
            }
        }
        names.push(name);
    }

    // --- Wrapped 構造の確定 (spec 05 zip 契約: 配布物は常にフォルダ包み形) ---
    if names.iter().any(|n| !n.contains('/')) {
        return Err("配布物の構造が不正です (フォルダ包み形ではありません)".to_string());
    }
    let tops: BTreeSet<&str> = names
        .iter()
        .filter_map(|n| n.split('/').next())
        .filter(|s| !s.is_empty())
        .collect();
    let top = match tops.len() {
        1 => tops.into_iter().next().unwrap().to_string(),
        _ => return Err("配布物の構造が不正です (トップフォルダが 1 つではありません)".to_string()),
    };
    if !top_name_is_safe(&top) {
        return Err(format!("フォルダ名に使えない文字が含まれています: {top}"));
    }
    if !names.iter().any(|n| n == &format!("{top}/package.yaml")) {
        return Err("package.yaml が見つかりません (パッケージ zip ではありません)".to_string());
    }

    // --- 展開先の衝突回避 ---
    let dest = unique_dir(dest_root, &top);
    std::fs::create_dir_all(&dest).map_err(|e| format!("展開先を作成できません: {e}"))?;

    // --- 展開パス (検証済みエントリのみ。top を剥がして dest 配下へ) ---
    let mut extract = || -> Result<(), String> {
        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| format!("zip の展開に失敗しました: {e}"))?;
            // enclosed_name 済み = 正規化された相対パス。先頭 (top) を剥がす。
            let rel: PathBuf = entry
                .enclosed_name()
                .ok_or_else(|| "不正なエントリパス".to_string())?
                .components()
                .skip(1)
                .collect();
            if rel.as_os_str().is_empty() {
                continue; // top ディレクトリ自身のエントリ
            }
            // spec 17: 出所メタの混入は展開しない (作者が更新済みフォルダを再 zip して
            // 納本した場合の対策 — メタは常に受領側クライアントが書いた値だけが存在する)。
            if rel.as_os_str() == crate::update::SOURCE_META_FILE {
                continue;
            }
            let out_path = dest.join(&rel);
            if entry.is_dir() {
                std::fs::create_dir_all(&out_path)
                    .map_err(|e| format!("フォルダを作成できません: {e}"))?;
            } else {
                if let Some(parent) = out_path.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("フォルダを作成できません: {e}"))?;
                }
                let mut out = File::create(&out_path)
                    .map_err(|e| format!("ファイルを書き出せません: {e}"))?;
                std::io::copy(&mut entry, &mut out)
                    .map_err(|e| format!("ファイルを書き出せません: {e}"))?;
            }
        }
        Ok(())
    };
    if let Err(e) = extract() {
        // 書きかけの展開先を残さない (中途半端なパッケージは読込エラーの温床)。
        let _ = std::fs::remove_dir_all(&dest);
        return Err(e);
    }
    Ok(dest)
}

/// `root/name` が空いていればそのまま、既に在れば `name_2`, `name_3`, … を返す。
fn unique_dir(root: &Path, name: &str) -> PathBuf {
    let base = root.join(name);
    if !base.exists() {
        return base;
    }
    for i in 2u32.. {
        let p = root.join(format!("{name}_{i}"));
        if !p.exists() {
            return p;
        }
    }
    unreachable!("u32 の枯渇")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::{SimpleFileOptions, ZipWriter};

    /// (エントリ名, 中身) のリストから一時 zip を作る (outcast zip_check テストと同型)。
    fn make_zip(entries: &[(&str, &str)]) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "kataribe_site_test_{}.zip",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut writer = ZipWriter::new(File::create(&path).unwrap());
        let opts = SimpleFileOptions::default();
        for (name, body) in entries {
            if name.ends_with('/') {
                writer.add_directory(name.trim_end_matches('/'), opts).unwrap();
            } else {
                writer.start_file(*name, opts).unwrap();
                writer.write_all(body.as_bytes()).unwrap();
            }
        }
        writer.finish().unwrap();
        path
    }

    fn temp_dest() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "kataribe_site_dest_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 【正常系】Wrapped zip が展開され、package.yaml を含むフォルダが返る。
    #[test]
    fn wrapped_zip_extracts_package_folder() {
        let zip = make_zip(&[
            ("MyPack/package.yaml", "title: t\nentry: scenarios/main.yaml"),
            ("MyPack/scenarios/main.yaml", "title: s\nstart: a"),
        ]);
        let dest = temp_dest();
        let out = extract_package_zip(&zip, &dest).unwrap();
        assert_eq!(out, dest.join("MyPack"));
        assert!(out.join("package.yaml").is_file(), "package.yaml が展開される");
        assert!(out.join("scenarios/main.yaml").is_file(), "入れ子も展開される");
    }

    /// 【zip slip 遮断】`..` を含むエントリは展開前に拒否され、展開先に何も生まれない。
    #[test]
    fn zip_slip_entry_is_rejected() {
        let zip = make_zip(&[
            ("MyPack/package.yaml", "title: t"),
            ("../evil.yaml", "boom"),
        ]);
        let dest = temp_dest();
        let err = extract_package_zip(&zip, &dest).unwrap_err();
        assert!(err.contains("不正なエントリパス"), "{err}");
        assert!(
            std::fs::read_dir(&dest).unwrap().next().is_none(),
            "展開先は無傷 (何も書かれない)"
        );
    }

    /// 【Wrapped 契約】直下形 (Flat) はサーバが正規化する契約ゆえクライアントは拒否する。
    #[test]
    fn flat_zip_is_rejected() {
        let zip = make_zip(&[
            ("package.yaml", "title: t"),
            ("scenarios/main.yaml", "title: s"),
        ]);
        let err = extract_package_zip(&zip, &temp_dest()).unwrap_err();
        assert!(err.contains("フォルダ包み形ではありません"), "{err}");
    }

    /// 【Wrapped 契約】トップフォルダが複数の zip も拒否する。
    #[test]
    fn multi_top_zip_is_rejected() {
        let zip = make_zip(&[
            ("A/package.yaml", "title: a"),
            ("B/other.yaml", "x: 1"),
        ]);
        let err = extract_package_zip(&zip, &temp_dest()).unwrap_err();
        assert!(err.contains("トップフォルダが 1 つではありません"), "{err}");
    }

    /// 【出所メタの混入 skip (spec 17)】zip に `.kataribe_source.json` が入っていても
    /// 展開されない — メタは常に受領側クライアントが書いた値だけが存在する。
    #[test]
    fn bundled_source_meta_is_skipped_on_extract() {
        let zip = make_zip(&[
            ("MyPack/package.yaml", "title: t"),
            ("MyPack/.kataribe_source.json", "{\"site_url\":\"https://evil.example\"}"),
            ("MyPack/scenarios/main.yaml", "start: r"),
        ]);
        let dest = temp_dest();
        let installed = extract_package_zip(&zip, &dest).unwrap();
        assert!(installed.join("package.yaml").exists(), "本体は展開される");
        assert!(installed.join("scenarios/main.yaml").exists());
        assert!(
            !installed.join(".kataribe_source.json").exists(),
            "混入メタは展開しない (細工 site_url を持ち込ませない)"
        );
        let _ = std::fs::remove_dir_all(&dest);
        let _ = std::fs::remove_file(&zip);
    }

    /// 【拒否拡張子】サーバをすり抜けても (自前サーバ等) クライアントで exe/js を弾く。
    #[test]
    fn denied_extension_is_rejected() {
        for bad in ["MyPack/tool.exe", "MyPack/run.js", "MyPack/UPPER.EXE"] {
            let zip = make_zip(&[("MyPack/package.yaml", "title: t"), (bad, "x")]);
            let err = extract_package_zip(&zip, &temp_dest()).unwrap_err();
            assert!(err.contains("実行ファイル・スクリプト"), "{bad}: {err}");
        }
    }

    /// 【package.yaml 必須】包みの中に package.yaml が無い zip は拒否する。
    #[test]
    fn missing_package_yaml_is_rejected() {
        let zip = make_zip(&[("MyPack/scenarios/main.yaml", "title: s")]);
        let err = extract_package_zip(&zip, &temp_dest()).unwrap_err();
        assert!(err.contains("package.yaml が見つかりません"), "{err}");
    }

    /// 【衝突回避】同名フォルダが既に在れば `_2` で展開する (旧フォルダを上書きしない)。
    #[test]
    fn existing_folder_is_uniquified() {
        let zip = make_zip(&[("MyPack/package.yaml", "title: t")]);
        let dest = temp_dest();
        let first = extract_package_zip(&zip, &dest).unwrap();
        let second = extract_package_zip(&zip, &dest).unwrap();
        assert_eq!(first, dest.join("MyPack"));
        assert_eq!(second, dest.join("MyPack_2"), "再取得は別フォルダへ");
        assert!(second.join("package.yaml").is_file());
    }
}
