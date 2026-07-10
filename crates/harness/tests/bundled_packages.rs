//! 同梱パッケージ (配布 dogfood) が常にロード + validate を通ることの回帰ガード。
//! content の YAML を手で直した時の幻フラグ/幻 goal/参照切れを CI 段階で検出する。
//! ユーザーがローカルに置く自作パッケージ (untracked) は対象外。

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

#[test]
fn bundled_packages_load_and_validate() {
    // 同梱パッケージは escape (campaign-entry) のみ。2026-07-10 に houkago は
    // harness の統合テスト fixture へ移設、他 (fantasy/promise_demo/sealed_shrine/
    // gnosia_village) は配布から削除した。houkago fixture は loader/package テストが検証する。
    harness::load_campaign_package(&repo_root().join("packages").join("escape"))
        .unwrap_or_else(|e| panic!("escape: {e}"));
}
