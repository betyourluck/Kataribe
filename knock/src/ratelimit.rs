//! 部屋作成の IP 単位 rate limit (契約 transport の運用防御②)。
//!
//! TURN クレデンシャルは create/join のたびに発行されるので、部屋作成を無制限にすると
//! ソースが public でも private でも同じだけ野良リレー化の入口になる。守りはここ (運用) に置く。

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// 素朴なスライディングウィンドウ (key = クライアント IP 文字列)。
/// 対象は友人卓の頻度 (毎分数回) なので、これで過剰・過小のどちらでもない。
pub struct RateLimiter {
    window: Duration,
    max_per_window: usize,
    hits: HashMap<String, VecDeque<Instant>>,
}

impl RateLimiter {
    pub fn new(window: Duration, max_per_window: usize) -> Self {
        Self {
            window,
            max_per_window,
            hits: HashMap::new(),
        }
    }

    /// この key の実行を許すか。許すなら計上する (check-and-consume)。
    pub fn allow(&mut self, key: &str, now: Instant) -> bool {
        let q = self.hits.entry(key.to_string()).or_default();
        while q.front().is_some_and(|t| now.duration_since(*t) >= self.window) {
            q.pop_front();
        }
        if q.len() >= self.max_per_window {
            return false;
        }
        q.push_back(now);
        true
    }

    /// 古い記録の掃除 (メモリの有界性。sweep タスクから呼ぶ)。
    pub fn sweep(&mut self, now: Instant) {
        self.hits.retain(|_, q| {
            while q.front().is_some_and(|t| now.duration_since(*t) >= self.window) {
                q.pop_front();
            }
            !q.is_empty()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 【運用防御②】窓内は max 回まで、超えたら拒否、窓が滑れば回復する。
    #[test]
    fn allows_up_to_max_then_blocks_until_window_slides() {
        let mut rl = RateLimiter::new(Duration::from_secs(60), 3);
        let t0 = Instant::now();
        assert!(rl.allow("1.2.3.4", t0));
        assert!(rl.allow("1.2.3.4", t0 + Duration::from_secs(1)));
        assert!(rl.allow("1.2.3.4", t0 + Duration::from_secs(2)));
        assert!(!rl.allow("1.2.3.4", t0 + Duration::from_secs(3)), "4 回目は拒否");
        // 別 IP は独立。
        assert!(rl.allow("5.6.7.8", t0 + Duration::from_secs(3)));
        // 最初の記録が窓から出れば 1 枠回復する。
        assert!(rl.allow("1.2.3.4", t0 + Duration::from_secs(61)));
        // sweep は空エントリを刈る (有界性)。
        rl.sweep(t0 + Duration::from_secs(1000));
        assert!(rl.hits.is_empty());
    }
}
