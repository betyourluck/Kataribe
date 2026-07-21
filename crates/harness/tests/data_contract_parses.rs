//! 台帳 `data_contract.yaml` が **YAML として parse できる**ことの番人。
//!
//! 動機 (2026-07-21): この台帳は拡張子が `.yaml` なのに、`key:"value"` (コロン直後に
//! スペース無し) が 6 箇所あって長らく parse 不能だった。人間向け台帳ゆえ実害は
//! 出ていなかったが、**名前が「機械可読」を符号化しているのに実体が違う**状態は、
//! 将来この台帳を機械検証したくなった時 (契約と実装のドリフト検出等) に躓く。
//!
//! 直すだけでは同じ崩れが再発するので、番人を置いて構造的に止める。

#[test]
fn data_contract_is_valid_yaml() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../data_contract.yaml");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} を読めない: {e}", path.display()));

    let parsed: serde_yaml::Value = serde_yaml::from_str(&text).unwrap_or_else(|e| {
        panic!(
            "data_contract.yaml が YAML として壊れている: {e}\n\
             よくある原因: `key:\"value\"` (コロン直後にスペースが無い) — `key: \"value\"` に直す"
        )
    });

    let map = parsed.as_mapping().expect("トップレベルは mapping");
    assert!(map.len() > 20, "契約節が異様に少ない (parse は通ったが構造が壊れた疑い)");
}
