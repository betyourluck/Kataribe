//! エピローグ (spec 11) — goal 到達時の engine 主導の締めくくり語り。
//!
//! **「いつ」は engine、「何を」は LLM。** 到達判定は `reached_goal()` の専権で、GM に
//! 「終わったと思ったら」を判断させない (弱モデルの不発・早すぎる幕引きを構造で断つ)。
//! 発火可否 (到達 + **終端** = campaign の advance 辺なし) の最終判定は呼び出し側 (app/CLI)
//! の責務 — 地図は harness/app 層の持ち物で gm_core は知らない。
//!
//! 生成はプレーン 1 回 ([`crate::Summarizer`] と同じ線引き = ops/ダイス不要)。素材は
//! spec 10 の synopsis + 未圧縮 chronicle = 「今回の旅路」そのもの。**予算を通常ターンの
//! 注入量と同等に抑える** (終端 = コンテキスト最大の瞬間にタイムアウト率を悪化させない)。
//! 規律は synopsis (#47 防衛) と線引きが違う: **起きたことは記録のとおりに・これから
//! 起きること (後日談) は自由に** — 後日談の想像こそエピローグの価値 (終幕なので以後の
//! ターンを汚染する経路も無い)。生成失敗は skip して従来表示 (結末文 + バナー) へ
//! フォールバック — `GoalDef.narration` が土台なので幕が裸にならない。

use gm_core::{GoalDef, Scenario};
use llm_client::{ChatMessage, LlmClient};

use crate::error::HarnessError;
use crate::synopsis::SynopsisEntry;
use crate::turn::TurnLog;

/// エピローグ生成のタイムアウト (秒)。Summarizer (15 秒) より長め = 見せ場、
/// request_timeout (既定 120 秒) より短い = 終幕を API エラーの人質にしない。
pub const EPILOGUE_TIMEOUT_SECS: u64 = 30;
/// 素材のあらすじ予算 (文字)。`synopsis_note` と同値 = GM が毎ターン読んでいる量を超えない。
const SYNOPSIS_BUDGET: usize = 2000;
/// 素材の経緯予算 (文字)。`history_note` と同値。
const CHRONICLE_BUDGET: usize = 2400;

/// エピローグ生成への入力 (接地素材 + authored 指示)。prompt の文面はここに集約する —
/// 規律 (記録矛盾禁止・後日談許可) を Phase B でテスト可能にし、生成 helper は文面を持たない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpilogueRequest {
    /// 到達 goal の表示名 (title、無ければ id)。
    pub goal_label: String,
    /// authored 結末文 (`GoalDef.narration`) — 結末の**意味**の錨。
    pub goal_narration: String,
    /// authored 演出指示 (`GoalDef.epilogue_prompt`)。
    pub instruction: String,
    /// 世界観 (語りのトーン)。空なら節ごと省く。
    pub world: String,
    /// 主人公 (name — profile)。空なら節ごと省く。
    pub protagonist: String,
    /// 旅路のあらすじ (章ブロック・古い順・予算 [`SYNOPSIS_BUDGET`] 適用済み)。
    pub synopsis: Vec<String>,
    /// 終盤の経緯 (未圧縮 chronicle の行・古い順・予算 [`CHRONICLE_BUDGET`] 適用済み)。
    pub chronicle: Vec<String>,
    /// 最後の場面 (last_narration)。空なら節ごと省く。
    pub last_narration: String,
}

impl EpilogueRequest {
    /// 語り手への規律 (system)。synopsis と違い**後日談は明示的に許可**する
    /// (起きたことは記録のとおりに・これから起きることは自由に)。
    pub fn system_prompt(&self) -> String {
        "あなたはこの TRPG セッションのナレーターです。物語は結末に到達しました。\
         旅路の記録をもとに、締めくくりのエピローグを書いてください。\n\
         規律:\n\
         - 起きたことは記録のとおりに — 記録と矛盾する過去を書かない。\
         固有名 (人物・場所・アイテム) は記録の表記のまま使う。\n\
         - これから起きること (後日談) は、結末と演出指示に沿って自由に想像してよい。\n\
         - 長さは 600 字程度を目安に。\n\
         - 出力はエピローグ本文のみ (前置き・見出しを付けない)。"
            .to_string()
    }

    /// 素材 (user)。空の節は省く (ノイズを足さない)。
    pub fn user_prompt(&self) -> String {
        let mut s = String::new();
        if !self.world.is_empty() {
            s.push_str(&format!("# 世界観\n{}\n\n", self.world));
        }
        if !self.protagonist.is_empty() {
            s.push_str(&format!("# 主人公\n{}\n\n", self.protagonist));
        }
        s.push_str(&format!("# 到達した結末「{}」\n{}\n\n", self.goal_label, self.goal_narration));
        if !self.instruction.is_empty() {
            s.push_str(&format!("# 演出指示 (作者から)\n{}\n\n", self.instruction));
        }
        if !self.synopsis.is_empty() {
            s.push_str(&format!("# 旅路のあらすじ (古い順)\n{}\n\n", self.synopsis.join("\n")));
        }
        if !self.chronicle.is_empty() {
            s.push_str(&format!("# 終盤の経緯 (古い順)\n{}\n\n", self.chronicle.join("\n")));
        }
        if !self.last_narration.is_empty() {
            s.push_str(&format!("# 最後の場面\n{}\n\n", self.last_narration));
        }
        s.push_str("以上の記録をもとに、エピローグを書いてください。");
        s
    }
}

/// 到達 goal + 旅路の記録から生成リクエストを組む (純粋)。
/// 予算は**新しい方優先**で拾い、提示は古い順 — `synopsis_note`/`history_note` と同じ流儀。
pub fn build_epilogue_request(
    scenario: &Scenario,
    goal: &GoalDef,
    synopsis: &[SynopsisEntry],
    history: &[TurnLog],
    last_narration: &str,
) -> EpilogueRequest {
    let goal_label =
        if goal.title.trim().is_empty() { goal.id.clone() } else { goal.title.clone() };
    let protagonist = {
        let name = scenario.protagonist.name.trim();
        let profile = scenario.protagonist.profile.trim();
        [name, profile].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" — ")
    };
    // あらすじ: 章ブロックを新しい方から予算まで拾い、古い順に戻す。
    let syn_blocks: Vec<String> =
        synopsis.iter().map(|e| format!("## {}\n{}", e.title, e.text)).collect();
    // 経緯: 未圧縮 tail (turn > 圧縮済み境界) だけを対象に同予算処理。
    let compressed_upto = synopsis.last().map(|e| e.upto_turn).unwrap_or(0);
    let chron_lines: Vec<String> = history
        .iter()
        .filter(|l| l.turn > compressed_upto)
        .map(|l| format!("T{} {}", l.turn, l.summary))
        .collect();
    EpilogueRequest {
        goal_label,
        goal_narration: goal.narration.trim().to_string(),
        instruction: goal.epilogue_prompt.as_deref().unwrap_or("").trim().to_string(),
        world: scenario.world.trim().to_string(),
        protagonist,
        synopsis: budget_newest(&syn_blocks, SYNOPSIS_BUDGET),
        chronicle: budget_newest(&chron_lines, CHRONICLE_BUDGET),
        last_narration: last_narration.trim().to_string(),
    }
}

/// リクエストを LLM messages に組む (純粋・テスト可)。tools/schema 無し =
/// no-tools サーバでもそのまま動く ([`crate::summarize_messages`] と同型)。
pub fn epilogue_messages(request: &EpilogueRequest) -> Vec<ChatMessage> {
    vec![
        ChatMessage::system(request.system_prompt()),
        ChatMessage::user(request.user_prompt()),
    ]
}

/// エピローグを生成する (ネットワーク経路・単体テスト対象外 = Summarizer 実装と同じ線引き)。
/// GM の client を使う (SUMMARY_LLM_* は使わない — 見せ場はナレーターの声で語られるべき)。
/// [`EPILOGUE_TIMEOUT_SECS`] + CoT 除去 (#30) + 空応答 Err。失敗は呼び出し側が skip
/// (結末文 + バナーの従来表示へフォールバック、非致命)。
pub async fn generate_epilogue(
    client: &LlmClient,
    request: &EpilogueRequest,
) -> Result<String, HarnessError> {
    let fut = client.generate(epilogue_messages(request));
    let text = tokio::time::timeout(std::time::Duration::from_secs(EPILOGUE_TIMEOUT_SECS), fut)
        .await
        .map_err(|_| {
            HarnessError::Summarize(format!("エピローグがタイムアウト ({EPILOGUE_TIMEOUT_SECS} 秒)"))
        })?
        .map_err(|e| HarnessError::Summarize(e.to_string()))?;
    let text = llm_client::strip_reasoning_blocks(&text).trim().to_string();
    if text.is_empty() {
        return Err(HarnessError::Summarize("エピローグが空の応答".into()));
    }
    Ok(text)
}

/// 列を**新しい方から**予算まで拾い、古い順で返す (溢れた古い方は落ちる)。
fn budget_newest(items: &[String], budget: usize) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    let mut used = 0usize;
    for item in items.iter().rev() {
        let cost = item.chars().count();
        if used + cost > budget {
            break;
        }
        used += cost;
        kept.push(item.clone());
    }
    kept.reverse();
    kept
}

// =============================================================================
// PoC (spec 11 Phase B、Red→Green)
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    fn scenario_with_goal() -> (Scenario, GoalDef) {
        let yaml = r#"
title: 封印の祠
start: shrine
world: 剣と魔法のファンタジー
protagonist: { name: アルト, profile: 見習い剣士 }
goals:
  - id: sealed
    title: 封印エンド
    when: { kind: always }
    narration: 魔は祠の奥へ封じられた。
    epilogue_prompt: 村に戻った主人公のその後を、季節の移ろいとともに。
locations:
  shrine: { description: d, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        let goal = sc.goals[0].clone();
        (sc, goal)
    }

    /// 【リクエスト形状 + 規律】system は「記録のとおり・後日談は自由・目安 600 字・本文のみ」、
    /// user は結末文 (意味の錨)・演出指示・あらすじ・経緯・最後の場面を運ぶ。
    /// tools/schema 無しの 2 メッセージ (no-tools サーバ対応)。
    #[test]
    fn epilogue_messages_ground_record_and_allow_aftermath() {
        let (sc, goal) = scenario_with_goal();
        let synopsis = vec![SynopsisEntry {
            upto_turn: 15,
            title: "旅立ちの章".into(),
            text: "アルトは祠の鍵を見つけ、聖剣に選ばれた。".into(),
        }];
        let history = vec![
            TurnLog { turn: 15, summary: "圧縮済みの行 (出ないはず)".into(), ..Default::default() },
            TurnLog { turn: 16, summary: "魔と対峙し、聖剣を掲げた".into(), ..Default::default() },
        ];
        let req = build_epilogue_request(&sc, &goal, &synopsis, &history, "光が満ちた。");

        let msgs = epilogue_messages(&req);
        assert_eq!(msgs.len(), 2, "system + user のみ (tools 無し)");
        let sys = &msgs[0].content;
        assert!(sys.contains("記録のとおり"), "過去の矛盾禁止: {sys}");
        assert!(sys.contains("後日談") && sys.contains("自由に想像"), "後日談の明示許可: {sys}");
        assert!(sys.contains("600 字"), "長さの目安");
        assert!(sys.contains("本文のみ"), "前置き禁止");

        let user = &msgs[1].content;
        assert!(user.contains("封印エンド"), "goal は title 優先で label 化");
        assert!(user.contains("魔は祠の奥へ封じられた"), "結末文 = 意味の錨: {user}");
        assert!(user.contains("季節の移ろい"), "authored 演出指示");
        assert!(user.contains("聖剣に選ばれた"), "あらすじ (旅路)");
        assert!(user.contains("T16 魔と対峙し"), "未圧縮 chronicle の tail");
        assert!(!user.contains("圧縮済みの行"), "圧縮済み範囲 (turn <= upto) は経緯に入れない");
        assert!(user.contains("光が満ちた"), "最後の場面");
        assert!(user.contains("剣と魔法のファンタジー"), "世界観");
        assert!(user.contains("アルト — 見習い剣士"), "主人公");
    }

    /// 【素材予算】経緯は新しい方優先で 2400 字に収め、溢れた古い方は落ちる
    /// (終端 = コンテキスト最大の瞬間にタイムアウト率を悪化させない)。空 synopsis でも動く。
    #[test]
    fn epilogue_material_respects_budgets_dropping_oldest() {
        let (sc, goal) = scenario_with_goal();
        let history: Vec<TurnLog> = (1..=200)
            .map(|i| TurnLog {
                turn: i,
                summary: format!("ターン{i}の出来事。廊下を歩き、扉を確かめ、灯りを整えた。"),
                ..Default::default()
            })
            .collect();
        let req = build_epilogue_request(&sc, &goal, &[], &history, "");

        let total: usize = req.chronicle.iter().map(|l| l.chars().count()).sum();
        assert!(total <= 2400, "経緯は予算内: {total}");
        assert!(req.chronicle.last().unwrap().contains("ターン200"), "最新は必ず残る");
        assert!(!req.chronicle.iter().any(|l| l.contains("ターン1の")), "最古は予算で落ちる");
        assert!(req.synopsis.is_empty(), "あらすじ無し (短編) でも動く");
        let user = req.user_prompt();
        assert!(!user.contains("# 旅路のあらすじ"), "空の節は省く");
        assert!(!user.contains("# 最後の場面"), "空の節は省く");
    }
}
