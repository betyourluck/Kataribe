//! 同梱パッケージ (配布 dogfood) が常にロード + validate を通ることの回帰ガード。
//! content の YAML を手で直した時の幻フラグ/幻 goal/参照切れを CI 段階で検出する。
//! ユーザーがローカルに置く自作パッケージ (untracked) は対象外。

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

#[test]
fn bundled_packages_load_and_validate() {
    for name in ["houkago", "promise_demo", "sealed_shrine", "gnosia_village"] {
        harness::load_package(&repo_root().join("packages").join(name))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    // escape は campaign-entry (開始モジュールを注入込みで検証)。
    harness::load_campaign_package(&repo_root().join("packages").join("escape"))
        .unwrap_or_else(|e| panic!("escape: {e}"));
}
