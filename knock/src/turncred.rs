//! TURN 一時クレデンシャル (TURN REST API 方式 = coturn `use-auth-secret`)。
//!
//! 静的パスワードを配ると VPS が野良リレー化する (契約 transport) — 代わりに
//! username = 失効時刻 (unix 秒)、credential = base64(HMAC-SHA1(secret, username)) を
//! knock のたびに発行する。coturn は同じ secret で HMAC を再計算して照合し、
//! username の時刻が過ぎていれば拒否する (サーバー側に状態は要らない)。

use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha1::Sha1;

use crate::msg::TurnCred;

/// HMAC-SHA1 を base64 で返す (coturn の照合形式)。
pub fn hmac_sha1_b64(secret: &str, msg: &str) -> String {
    let mut mac =
        Hmac::<Sha1>::new_from_slice(secret.as_bytes()).expect("HMAC は任意長キーを受ける");
    mac.update(msg.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

/// 期限付きクレデンシャルを発行する。`now_unix` は注入 (テスト可能・時計非依存の純関数)。
pub fn turn_credential(secret: &str, urls: &[String], ttl_secs: u64, now_unix: u64) -> TurnCred {
    let username = (now_unix + ttl_secs).to_string();
    let credential = hmac_sha1_b64(secret, &username);
    TurnCred {
        urls: urls.to_vec(),
        username,
        credential,
        ttl_secs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 【HMAC-SHA1 の接地】RFC 2202 の公開テストベクタと一致する (自前実装の自己参照でなく
    /// 外部の正解に接地する)。key="key", msg="The quick brown fox jumps over the lazy dog"
    /// → de7c9b85b8b78aa6bc8a7a36f70a90701c9db4d9 = base64 "3nybhbi3iqa8ino29wqQcBydtNk="。
    #[test]
    fn hmac_sha1_matches_public_test_vector() {
        assert_eq!(
            hmac_sha1_b64("key", "The quick brown fox jumps over the lazy dog"),
            "3nybhbi3iqa8ino29wqQcBydtNk="
        );
    }

    /// 【TURN REST 形式】username = now+ttl の unix 秒、credential = HMAC(secret, username)。
    /// coturn が照合するのは username 文字列そのものなので、この対応が崩れると繋がらない。
    #[test]
    fn credential_binds_expiry_to_hmac() {
        let urls = vec!["turns:turn.example.jp:5349?transport=tcp".to_string()];
        let c = turn_credential("s3cret", &urls, 300, 1_800_000_000);
        assert_eq!(c.username, "1800000300", "username は失効 unix 秒");
        assert_eq!(c.credential, hmac_sha1_b64("s3cret", "1800000300"));
        assert_eq!(c.ttl_secs, 300);
        assert_eq!(c.urls, urls);
    }
}
