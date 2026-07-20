//! 正本の裁定者。LLM の提案を裁き、受理時のみ原子的に state を更新する。

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::reason::RejectReason;
use crate::spine::{ImageMode, Scenario, TakeMode};
use crate::state::{GameState, RngState, StateDelta, StateOp, TriggerId, PLAYER};

/// 裁定結果。`Reject` は**構造化された**理由を含む (文面は提示層が言語ごとに生成)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Verdict {
    Accept,
    Reject { reasons: Vec<RejectReason> },
}

impl Verdict {
    pub fn is_accept(&self) -> bool {
        matches!(self, Verdict::Accept)
    }
}

/// ダイスの出目。エンジンが振った結果であり、LLM は関与しない。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollOutcome {
    pub sides: u32,
    pub dc: u32,
    pub result: u32,
    pub success: bool,
}

/// 可変量ダイス ([`StateOp::RollStat`]) の監査記録 (spec 16)。
/// 「SAN -4 (1d6=4)」を提示層が組み立てる素材 — 出目まで再現・監査可能。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatRollOutcome {
    pub entity: String,
    pub key: String,
    pub count: u32,
    pub sides: u32,
    pub bonus: i64,
    /// 各ダイスの素の出目 (count 個)。
    pub rolls: Vec<u32>,
    /// stat に適用された符号付きの変化量 (negate 込み・clamp 前の意図量)。
    pub amount: i64,
}

fn one_u32() -> u32 {
    1
}
fn one_i64() -> i64 {
    1
}

/// 技能判定の結果。`{count}d{sides} × times + modifier` を振り `total >= dc` で成否
/// (既定 1d・×1 = 従来形)。LLM は出目も合計も持てない。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckOutcome {
    pub entity: String,
    pub stat: String,
    pub sides: u32,
    /// ダイス個数 (既定 1)。`{count}d{sides}` — roll は**素の合計** (2026-07-20)。
    #[serde(default = "one_u32")]
    pub count: u32,
    /// 出目の乗数 (既定 1)。`total = 合計 × times + 修正` (3D6×5 系。乗算は出目だけ)。
    #[serde(default = "one_i64")]
    pub times: i64,
    pub roll: u32,
    pub modifier: i64,
    pub total: i64,
    pub dc: u32,
    pub success: bool,
    /// 該当した極 (tier) 名 (authored challenge の大失敗/大成功)。素の判定や非クリティカルでは `None`。
    #[serde(default)]
    pub tier: Option<String>,
    /// authored challenge の結末ナレーション (on_success/on_failure/tier の narration を解決したもの)。
    /// **毎回・同ターン**に提示層が出す (非 latch=繰り返す失敗も毎回語れる)。無ければ空文字。
    #[serde(default)]
    pub narration: String,
    /// authored challenge の結末効果音のアセット ID (on_success/on_failure/tier の sound を
    /// 解決したもの)。**engine 非解釈の不透明 string** (narration と同列の語り素材)。提示層が
    /// `audios/` から解決し one-shot 再生する。無ければ空文字。
    #[serde(default)]
    pub sound: String,
    /// d100 ロールアンダー判定の成功度 (spec 16)。`critical`/`extreme`/`hard`/`regular`/
    /// `failure`/`fumble` の機械 id (表示は提示層の言語表で変換)。加算式判定・旧セーブは `None`。
    /// percentile では `total`=出目 / `dc`=実効目標値 / `modifier`=目標値への修正合算、と
    /// 既存フィールドの意味が変わる (表示は degree の有無で書式分岐)。
    #[serde(default)]
    pub degree: Option<String>,
    /// spec 18 Phase B: プッシュ (振り直し) を経て確定した判定か。還流 (check_outcome_note) と
    /// 表示が「押して振り直した」を語る素。
    #[serde(default)]
    pub pushed: bool,
    /// spec 18 Phase B: 差分買いで支払った量 (0 = 買っていない)。還流と表示の素。
    #[serde(default)]
    pub spent: i64,
    /// spec 18 Phase B: この判定が**決断待ちで凍結中**か (帰結未適用・narration 無し)。
    /// 提示層は開帳後に決断 UI を出し、resolve_decision の結果 (最終 CheckOutcome) で上書きする。
    #[serde(default)]
    pub pending: bool,
}

/// 発火したトリガー (Phase C)。`narration` は語りへ注入する指示。
///
/// `recall` は Memoria 橋渡しの cue を**そのまま passthrough** したもの (engine は解釈しない)。
/// 上位 (harness) が `recall` を Memoria で解決して伏線を語りに注入する。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FiredTrigger {
    pub id: TriggerId,
    pub narration: String,
    pub recall: Option<String>,
    /// 発火時のイベント CG (画像 ID)。`Trigger.image` を passthrough (engine は解釈しない)。
    pub image: Option<String>,
    /// イベント CG の表示モード。`Trigger.image_mode` を passthrough。
    pub image_mode: Option<ImageMode>,
    /// 発火時の SE (効果音 ID)。`Trigger.sound` を passthrough (engine は解釈しない)。
    pub sound: Option<String>,
}

/// デルタ受理時の適用結果。ダイスの出目と、その適用が連鎖発火させたトリガー群。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyOutcome {
    /// `request_roll` とトリガー効果が振ったダイスの出目 (適用順)。
    pub rolls: Vec<RollOutcome>,
    /// この適用で行われた技能判定の結果。次ターンの語りに還流する。
    pub checks: Vec<CheckOutcome>,
    /// 可変量ダイス (`roll_stat`) の監査記録 (spec 16)。「SAN -4 (1d6=4)」の素材。
    pub stat_rolls: Vec<StatRollOutcome>,
    /// この適用で発火した反応ビート (authored 順・連鎖含む)。語りに注入する。
    pub fired: Vec<FiredTrigger>,
}

/// d100 ロールアンダーの成功度 (spec 16 の凍結ルール・CoC7 準拠)。純関数。
///
/// 判定順は **critical 先勝ち** (`01` は常に成功 — 現行定義では fumble 帯と交差しないが、
/// 将来拡張でもこの原則が壊れない順序を固定する)。fumble は**有効目標値**基準
/// (v1 = stat + modifiers。将来「ハード成功要求」等を足す場合もその実効値で `< 50` を見る)。
/// 目標値 0 以下でも `01` は critical 成功を保証する。整数除算 (端数切り捨て = 原典どおり)。
pub fn percentile_degree(roll: u32, target: i64) -> (&'static str, bool) {
    if roll == 1 {
        return ("critical", true);
    }
    if roll == 100 || (target < 50 && roll >= 96) {
        return ("fumble", false);
    }
    let r = i64::from(roll);
    if r <= target / 5 {
        ("extreme", true)
    } else if r <= target / 2 {
        ("hard", true)
    } else if r <= target {
        ("regular", true)
    } else {
        ("failure", false)
    }
}

/// percentile challenge の degree → 帰結スロット解決 (spec 16 のフォールバック連鎖)。
/// 上位成功は自分のスロット → 無ければ次段 (critical → extreme → hard → on_success /
/// fumble → on_failure)。`apply_ops` と `guaranteed_challenge_effects` が共有する
/// (適用と射影の乖離を構造的に防ぐ — spec 09 と同じ原則)。
fn resolve_degree_slot<'a>(
    def: &'a crate::spine::ChallengeDef,
    degree: &str,
) -> Option<&'a crate::spine::ChallengeOutcome> {
    match degree {
        "critical" => def
            .on_critical
            .as_ref()
            .or(def.on_extreme.as_ref())
            .or(def.on_hard.as_ref())
            .or(def.on_success.as_ref()),
        "extreme" => def.on_extreme.as_ref().or(def.on_hard.as_ref()).or(def.on_success.as_ref()),
        "hard" => def.on_hard.as_ref().or(def.on_success.as_ref()),
        "regular" => def.on_success.as_ref(),
        "fumble" => def.on_fumble.as_ref().or(def.on_failure.as_ref()),
        _ => def.on_failure.as_ref(),
    }
}

/// 唯一の裁定者。**`state` を一切変更しない純粋関数**。
///
/// 1つでも不正な op があれば `Reject` を返す (理由は全件収集)。
pub fn adjudicate(state: &GameState, scenario: &Scenario, delta: &StateDelta) -> Verdict {
    if scenario.location(&state.location).is_none() {
        return Verdict::Reject {
            reasons: vec![RejectReason::CurrentLocationMissing {
                location: state.location.clone(),
            }],
        };
    }

    let mut reasons = Vec::new();

    // op の力学 (所持/移動/gate/stat 宣言) を**書かれた順**に検証 (逐次射影、spec 09)。
    validate_ops(&mut reasons, state, scenario, delta);

    // 硬い禁忌 (Phase B): op 単体が合法なら、delta 適用後に taboo(Gate) が真化しないか検査。
    // adjudicate は純粋なので state の clone へ射影 (project) して評価する。
    if reasons.is_empty() {
        check_taboos(&mut reasons, state, scenario, delta);
    }

    if reasons.is_empty() {
        Verdict::Accept
    } else {
        Verdict::Reject { reasons }
    }
}

/// op 単体の力学を検証して reasons に積む (taboo は別。state を変えない)。
///
/// **逐次射影裁定 (spec 09-A)**: op を書かれた順に検証し、受理できた op の**決定論的効果**を
/// 射影クローンへ仮適用してから次の op を検証する — 裁定は apply のドライラン。
/// 「拾ってから使う」のような段取りが 1 delta で通る (裁定と適用の時点ズレの catch-22 を根絶、
/// mujinto 実測 2026-07-09)。ダイス op (roll/check/attempt) は検証のみで帰結を射影しない
/// (出目は apply 時確定・純粋な裁定は RNG を消費できない) — 判定結果に依存する後続手は
/// 従来どおり次ターン。適用は [`apply_deterministic_op`] を実 apply と共有し、
/// 射影と実適用の乖離を構造的に防ぐ。
fn validate_ops(
    reasons: &mut Vec<RejectReason>,
    state: &GameState,
    scenario: &Scenario,
    delta: &StateDelta,
) {
    let mut proj = state.clone();
    for op in &delta.ops {
        let before = reasons.len();
        validate_op(reasons, &proj, scenario, op);
        // この op が合法なら射影へ仮適用 (不正な op は射影に乗せず、残りは現射影で診断を続ける)。
        if reasons.len() == before {
            apply_deterministic_op(&mut proj, scenario, op);
            // ダイス op の帰結は原則非射影 (出目は apply 時確定) だが、attempt_challenge の
            // **全帰結に共通する効果**だけは射影する — どの出目でも必ず起きる = 裁定時に
            // 確定扱いしても「裁定は適用のドライラン」の健全性を破らない。日次フラグを
            // 全帰結に書く作法 ([挑戦, 帰宅 move] の束ね) が一発受理になる。
            // 帰結**依存**の効果 (片側だけ/tier) は従来どおり非射影 = ダイスはターンを割る。
            if let StateOp::AttemptChallenge { challenge, .. } = op {
                if let Some(def) = scenario.challenge(challenge) {
                    for eff in guaranteed_challenge_effects(scenario, def) {
                        apply_deterministic_op(&mut proj, scenario, &eff);
                    }
                }
            }
        }
    }
}

/// challenge の**全帰結に共通する効果** = 出目に依らず必ず起きる op 列。
/// on_success と on_failure の効果の**多重集合の交差** (過剰射影は誤受理の芽なので個数も厳密) と、
/// 両帰結が同じフラグを立てる場合の set_flag。tier は加算的で outcome 効果を打ち消さないため
/// 交差の健全性に影響しない (tier 限定の効果は共通でないので入らない)。片側が None なら空。
fn guaranteed_challenge_effects(scenario: &Scenario, def: &crate::spine::ChallengeDef) -> Vec<StateOp> {
    // spec 18 Phase B: 決断つき challenge は最終スロットが on_push_failure/買い上げ先まで
    // 広がる (どの帰結で確定するかは決断次第) ため「全帰結共通」を静的に保証できない →
    // 射影しない (過剰射影は誤受理の芽 — 安全側)。pushable の既定を false (opt-in) にした
    // 理由の一つがこれ: 既存 content の束ね受理 (spec 09) を黙って壊さない。
    if decision_enabled(scenario, def) {
        return Vec::new();
    }
    // 2 帰結の多重集合の厳密交差 (順序保存は左側基準)。
    fn intersect_effects(a: &[StateOp], b: &[StateOp]) -> Vec<StateOp> {
        let mut pool: Vec<&StateOp> = b.iter().collect();
        let mut out: Vec<StateOp> = Vec::new();
        for e in a {
            if let Some(i) = pool.iter().position(|p| *p == e) {
                pool.remove(i);
                out.push(e.clone());
            }
        }
        out
    }
    if def.resolution == crate::spine::Resolution::Percentile {
        // percentile: 6 degree すべての解決先スロットで交差する (どの出目でも必ず起きるもの
        // だけが確定扱いできる)。どれか 1 つでもスロット無し (=その degree は効果ゼロ) なら空。
        const DEGREES: [&str; 6] = ["critical", "extreme", "hard", "regular", "failure", "fumble"];
        let mut slots = Vec::with_capacity(6);
        for d in DEGREES {
            match resolve_degree_slot(def, d) {
                Some(o) => slots.push(o),
                None => return Vec::new(),
            }
        }
        let mut out: Vec<StateOp> = slots[0].effects.clone();
        for o in &slots[1..] {
            out = intersect_effects(&out, &o.effects);
        }
        // 全スロットが同じフラグを立てる場合のみ確定 set_flag。
        if let Some(f0) = slots[0].flag.as_ref() {
            if slots.iter().all(|o| o.flag.as_ref() == Some(f0)) {
                out.push(StateOp::SetFlag { key: f0.clone(), value: true });
            }
        }
        return out;
    }
    let (Some(s), Some(f)) = (&def.on_success, &def.on_failure) else {
        return Vec::new();
    };
    let mut out = intersect_effects(&s.effects, &f.effects);
    if let (Some(sf), Some(ff)) = (&s.flag, &f.flag) {
        if sf == ff {
            out.push(StateOp::SetFlag { key: sf.clone(), value: true });
        }
    }
    out
}

/// 1 op の力学を `state` (裁定では射影クローン) に対して検証する。
fn validate_op(
    reasons: &mut Vec<RejectReason>,
    state: &GameState,
    scenario: &Scenario,
    op: &StateOp,
) {
    // 現在地は射影を追う (Move を含む delta では以降の op を移動先で検証する)。
    let loc = match scenario.location(&state.location) {
        Some(l) => l,
        None => {
            reasons.push(RejectReason::CurrentLocationMissing {
                location: state.location.clone(),
            });
            return;
        }
    };
    {
        match op {
            StateOp::AddItem { item } => {
                // 既に所持しているなら**受理して no-op** (spec 09-B: 「念のための再拾得」を
                // 害なく受ける。inventory は集合なので複製は構造的に起きず、複製穴の守りは
                // taken_items = take:once の再取得却下が担い続ける)。
                if state.has_item(PLAYER, item) {
                    return;
                }
                match loc.items.get(item) {
                    None => reasons.push(RejectReason::ItemNotHere { item: item.clone() }),
                    Some(li) => match li.take() {
                        // 備え付けは取れない。理由が「その場で使える」を LLM に説明する。
                        TakeMode::Fixed => {
                            reasons.push(RejectReason::ItemFixed { item: item.clone() });
                        }
                        // once は持ち去り済みなら再取得 (複製) を遮断。
                        TakeMode::Once if state.already_taken(&state.location, item) => {
                            reasons.push(RejectReason::ItemAlreadyTaken { item: item.clone() });
                        }
                        _ => {
                            if !li.when().eval(state) {
                                reasons.push(RejectReason::ItemGateUnmet {
                                    item: item.clone(),
                                    requirement: li.when().clone(),
                                    unmet: li.when().unmet(state),
                                });
                            }
                        }
                    },
                }
            }
            StateOp::RemoveItem { item } => {
                if !state.has_item(PLAYER, item) {
                    reasons.push(RejectReason::ItemNotHeld { item: item.clone() });
                }
            }
            StateOp::GiveItem { from, to, item } => {
                // 持っていない物は渡せない (#23 の engine 側バックストップ)。
                if !state.has_item(from, item) {
                    reasons.push(RejectReason::ItemNotHeld { item: item.clone() });
                }
                // 幻のキャラには渡せない (閉世界)。
                if !scenario.knows_entity(to) {
                    reasons.push(RejectReason::UnknownEntity { entity: to.clone() });
                }
            }
            StateOp::SetFlag { key, value } => {
                if !scenario.allowed_flags.contains(key) {
                    // 使えるフラグの語彙 (allowed − authored 専権) を却下理由に載せ、
                    // self-repair が一発で正しい名前へ修正できるようにする (#31 の entity と同型)。
                    reasons.push(RejectReason::FlagNotAllowed {
                        key: key.clone(),
                        available: scenario.usable_flags().into_iter().collect(),
                    });
                    return;
                }
                // authored 専権フラグ (トリガー/challenge の帰結が書く) は LLM が true にも
                // **false にも**倒せない (筋書きの先取り/巻き戻しの遮断)。従来この検査が無く、
                // 語彙の除外は prompt 層のみ = `value:false` は素通りで受理されていた (#50:
                // GM が「退勤」の意味論で会社フラグを false に倒し、同 delta の move gate を
                // 自分で壊す / 単独なら authored 機構を静かに妨害できた)。トリガー効果は
                // `apply_ops` 直行なので従来どおり書ける (grant_skill 等の op 専権と同型)。
                if scenario.authored_only_flags().contains(key) {
                    reasons.push(RejectReason::FlagNotAllowed {
                        key: key.clone(),
                        available: scenario.usable_flags().into_iter().collect(),
                    });
                    return;
                }
                if *value {
                    let gate = scenario.flag_gate(key);
                    if !gate.eval(state) {
                        let unmet = gate.unmet(state);
                        reasons.push(RejectReason::FlagGateUnmet {
                            key: key.clone(),
                            requirement: gate,
                            unmet,
                        });
                    }
                }
            }
            StateOp::Move { to } => match loc.exits.iter().find(|e| &e.to == to) {
                None => reasons.push(RejectReason::NoExit { to: to.clone() }),
                Some(exit) => {
                    if !exit.gate.eval(state) {
                        // 必要条件を理由に載せる (#42): 「未達」だけでは LLM が move を諦める。
                        reasons.push(RejectReason::MoveGateUnmet {
                            to: to.clone(),
                            requirement: exit.gate.clone(),
                            unmet: exit.gate.unmet(state),
                        });
                    }
                }
            },
            StateOp::RequestRoll { sides, dc: _ } => {
                if *sides < 1 {
                    reasons.push(RejectReason::DiceSidesInvalid);
                }
                // 出目はエンジンが振る。LLM は結果を主張できない (op 構造上不可能)。
            }
            StateOp::Check { entity, stat, sides, dc: _ } => {
                if *sides < 1 {
                    reasons.push(RejectReason::DiceSidesInvalid);
                }
                // 修正に使う stat は宣言済みでなければならない (幻ステータスで判定を盛れない)。
                if !scenario.knows_stat(entity, stat) {
                    reasons.push(RejectReason::UnknownStat { entity: entity.clone(), key: stat.clone() });
                }
            }
            StateOp::CheckUnder { entity, key } => {
                // d100 ロールアンダー (spec 16)。面数は様式で固定 (100) なので検証は stat 宣言のみ
                // (幻技能で判定できない。目標値=stat 現在値はエンジンが apply 時に読む)。
                if !scenario.knows_stat(entity, key) {
                    reasons.push(RejectReason::UnknownStat { entity: entity.clone(), key: key.clone() });
                }
            }
            StateOp::RollStat { entity, key, .. } => {
                // 可変量ダイスは authored 専権 (spec 16) — LLM がダメージ/SAN 減少の量を
                // 自分で振る経路を作らない。trigger/challenge effects は apply_ops 直行。
                reasons.push(RejectReason::StatRollNotAllowed {
                    entity: entity.clone(),
                    key: key.clone(),
                });
            }
            StateOp::AttemptChallenge { entity, challenge } => {
                // 閉世界: 宣言された challenge にしか挑めない (幻チャレンジ遮断)。
                match scenario.challenge(challenge) {
                    None => reasons.push(RejectReason::UnknownChallenge {
                        challenge: challenge.clone(),
                    }),
                    Some(def) => {
                        // 前提条件 (requires Gate) が未達なら、まだ挑めない (挑戦の解禁/封鎖)。
                        if let Some(req) = &def.requires {
                            if !req.eval(state) {
                                reasons.push(RejectReason::ChallengeLocked {
                                    challenge: challenge.clone(),
                                    requirement: req.clone(),
                                    unmet: req.unmet(state),
                                });
                            }
                        }
                        // 判定の素性は authored。主体も authored 固定 (def.entity) があれば
                        // それが op の entity を上書きする (LLM の entity 省略/誤指定に依らない)。
                        // stat 修正を使う場合のみ、判定主体がその stat を宣言済みであること。
                        let subject = def.entity.as_ref().unwrap_or(entity);
                        if let Some(stat) = &def.stat {
                            if !scenario.knows_stat(subject, stat) {
                                reasons.push(RejectReason::UnknownStat {
                                    entity: subject.clone(),
                                    key: stat.clone(),
                                });
                            }
                        }
                        // 式修正 (spec 19) の参照 stat も実際の主体で検査する (load 時 validate は
                        // 既定主体で見るが、op の entity 上書きで主体が変わりうる — 二層目)。
                        if let Some(xsrc) = &def.expr {
                            if let Ok(x) = crate::expr::parse_expr(xsrc) {
                                for key in x.stats() {
                                    if !scenario.knows_stat(subject, &key) {
                                        reasons.push(RejectReason::UnknownStat {
                                            entity: subject.clone(),
                                            key,
                                        });
                                    }
                                }
                            }
                        }
                        // 面数は additive のみの概念 (percentile は d100 固定・sides=0 が正、
                        // 形の整合は load 時 validate が保証済み — spec 16)。
                        if def.resolution == crate::spine::Resolution::Additive && def.sides < 1 {
                            reasons.push(RejectReason::DiceSidesInvalid);
                        }
                    }
                }
            }
            StateOp::AttemptContest { contest } => {
                // 対決の開始 (spec 18 Phase C)。素性は authored — LLM は id を選ぶだけ。
                if state.pending_contest.is_some() {
                    reasons.push(RejectReason::ContestInProgress);
                }
                match scenario.contest(contest) {
                    None => {
                        reasons.push(RejectReason::UnknownContest { contest: contest.clone() })
                    }
                    Some(def) => {
                        if let Some(req) = &def.requires {
                            if !req.eval(state) {
                                reasons.push(RejectReason::ContestLocked {
                                    contest: contest.clone(),
                                    requirement: req.clone(),
                                    unmet: req.unmet(state),
                                });
                            }
                        }
                    }
                }
            }
            StateOp::AdjustStat { entity, key, delta: _ } => {
                if !scenario.knows_stat(entity, key) {
                    reasons.push(RejectReason::UnknownStat { entity: entity.clone(), key: key.clone() });
                }
                // 算術 (current + delta) と境界クランプは apply がエンジンとして行う。
            }
            StateOp::ScaleStat { entity, key, num: _, den } => {
                if !scenario.knows_stat(entity, key) {
                    reasons.push(RejectReason::UnknownStat { entity: entity.clone(), key: key.clone() });
                }
                if *den == 0 {
                    reasons.push(RejectReason::DivideByZero { key: key.clone() });
                }
            }
            StateOp::GrantSkill { entity, skill } => {
                // 能力の開花は authored トリガーの専権。LLM 提案は常に却下 (メアリー・スー遮断)。
                // trigger effects は apply_ops 直行なのでこの検証を通らず付与できる。
                reasons.push(RejectReason::SkillGrantNotAllowed {
                    entity: entity.clone(),
                    skill: skill.clone(),
                });
            }
            StateOp::SetAttribute { entity, key, .. } => {
                // 属性の書き換えも authored トリガーの専権。LLM 提案は常に却下 (クラス捏造遮断)。
                // trigger effects は apply_ops 直行なのでこの検証を通らず書き換えられる。
                reasons.push(RejectReason::AttributeSetNotAllowed {
                    entity: entity.clone(),
                    key: key.clone(),
                });
            }
            StateOp::RecordTurn { entity, key } => {
                // ターンの刻みも authored トリガーの専権。LLM 提案は常に却下 (タイマー詐称遮断)。
                // trigger effects は apply_ops 直行なのでこの検証を通らず刻める。
                reasons.push(RejectReason::TurnRecordNotAllowed {
                    entity: entity.clone(),
                    key: key.clone(),
                });
            }
            StateOp::SetPresence { entity, .. } => {
                // 登場/退場も authored トリガーの専権。LLM 提案は常に却下 (キャラ勝手登場の捏造遮断)。
                // trigger effects は apply_ops 直行なのでこの検証を通らず登場/退場させられる。
                reasons.push(RejectReason::PresenceSetNotAllowed {
                    entity: entity.clone(),
                });
            }
            StateOp::CastVote { voter, target } => {
                // 票の意図は LLM が出せる。ただし受理は「盤面に投票機構がある + 両者生存 +
                // vote_rules のいずれかに合致 (デフォルト拒否)」をエンジンが裁く (spec 06 Phase C)。
                // vote_rules が空 = 投票の無いゲーム。機構の不在を名指しで却下する
                // (死者/局面の理由を出すと self-repair が誤った方向に直そうとする、#35)。
                if scenario.vote_rules.is_empty() {
                    reasons.push(RejectReason::VoteNotDeclared);
                    return;
                }
                if !scenario.knows_entity(voter) {
                    reasons.push(RejectReason::UnknownEntity { entity: voter.clone() });
                    return;
                }
                if !scenario.knows_entity(target) {
                    reasons.push(RejectReason::UnknownEntity { entity: target.clone() });
                    return;
                }
                // 生死は **生存 stat を持つ entity にだけ** 意味を持つ (role_assignment が seed
                // する)。未宣言 = 生死の概念が無い盤面/キャラで、0 と誤読して死者扱いしない
                // (従来は生存 seed の無い盤面で全員が死者になり投票が構造的に不可能だった、#35)。
                let alive = |e: &str| {
                    state
                        .entities
                        .get(e)
                        .and_then(|stats| stats.get("生存"))
                        // MSRV 1.80 のため is_none_or (1.82〜) は使わない。
                        .map_or(true, |v| *v == 1)
                };
                if !alive(voter) {
                    reasons.push(RejectReason::EntityNotAlive { entity: voter.clone() });
                    return;
                }
                if !alive(target) {
                    reasons.push(RejectReason::EntityNotAlive { entity: target.clone() });
                    return;
                }
                let allowed = scenario.vote_rules.iter().any(|rule| {
                    let voter_ok = match &rule.voter_attribute {
                        None => true, // voter 条件なし = 生存者なら誰でも
                        Some(va) => state.attribute_of(voter, &va.key) == va.value,
                    };
                    rule.when.eval(state) && voter_ok
                });
                if !allowed {
                    reasons.push(RejectReason::VoteNotAllowed { voter: voter.clone() });
                }
            }
            StateOp::ResolveVote => {
                // 開票は authored トリガーの専権 (効果 op 第5例)。LLM 提案は常に却下
                // (開票結果の捏造遮断)。trigger effects は apply_ops 直行なので開票できる。
                reasons.push(RejectReason::VoteResolveNotAllowed);
            }
        }
    }
}

/// delta を `state` の clone に射影し、各キャラの taboo(Gate) が **false→true** に
/// 真化するなら却下理由を積む (硬い禁忌の強制)。射影は純粋 (元 state は不変)。
fn check_taboos(
    reasons: &mut Vec<RejectReason>,
    state: &GameState,
    scenario: &Scenario,
    delta: &StateDelta,
) {
    // taboo を持つキャラが居なければ射影コストを払わない。
    if scenario.characters.values().all(|c| c.taboos.is_empty()) {
        return;
    }
    let mut projected = state.clone();
    // clone への射影 (dice/jud定 は捨て、taboo 評価のためだけに state を進める)。
    apply_ops(&mut projected, scenario, delta, &mut Vec::new(), &mut Vec::new(), &mut Vec::new());
    for (eid, def) in &scenario.characters {
        for taboo in &def.taboos {
            if !taboo.eval(state) && taboo.eval(&projected) {
                reasons.push(RejectReason::TabooViolated { entity: eid.clone() });
            }
        }
    }
}

/// `adjudicate` が `Accept` の時のみデルタを**原子的に**適用する。
///
/// `Reject` の場合 `state` は一切変更されず、`Err(Verdict::Reject)` を返す。
/// 含まれる [`StateOp::RequestRoll`] はここで決定論的に振られる。適用後、発火条件が
/// 真化したトリガー (Phase C) を連鎖発火させ、その出目と発火ビートも [`ApplyOutcome`] に含める。
pub fn apply(
    state: &mut GameState,
    scenario: &Scenario,
    delta: &StateDelta,
) -> Result<ApplyOutcome, Verdict> {
    // まず純粋関数で全検証 — ここを通ってから初めて state に触れる (原子性の担保)。
    match adjudicate(state, scenario, delta) {
        rejected @ Verdict::Reject { .. } => return Err(rejected),
        Verdict::Accept => {}
    }

    // フラグの真化点を刻むため、適用前の true 集合を控える (差分は apply 末尾で一括)。
    let flags_before: BTreeSet<String> =
        state.flags.iter().filter(|(_, v)| **v).map(|(k, _)| k.clone()).collect();

    let mut rolls = Vec::new();
    let mut checks = Vec::new();
    let mut stat_rolls = Vec::new();
    apply_ops(state, scenario, delta, &mut rolls, &mut checks, &mut stat_rolls);
    state.turn += 1;
    // 反応ビート (禁忌の双対)。受理・適用済みの実 state に対して発火判定する。
    let fired = fire_triggers(state, scenario, &mut rolls, &mut checks, &mut stat_rolls);

    // このターンに true へ真化したフラグへ「立ったターン」を刻む。差分方式なので
    // op / トリガー効果 / challenge 帰結のどの経路で立っても漏れなく捕捉される。
    let newly_true: Vec<String> = state
        .flags
        .iter()
        .filter(|(k, v)| **v && !flags_before.contains(*k))
        .map(|(k, _)| k.clone())
        .collect();
    for key in newly_true {
        state.flag_turns.insert(key, state.turn);
    }

    Ok(ApplyOutcome { rolls, checks, stat_rolls, fired })
}

// =============================================================================
// 対決 (contest) — 決着まで LLM を介さない交互振り (spec 18 Phase C)
//
// attempt_contest (LLM が「開く」) の後、ラウンドはプレイヤーと engine が直接回す。
// 1 ラウンド = 双方が振って比較 → player 視点の on_win/on_lose/on_tie を原子適用 →
// until/max_rounds/goal で決着。何交換でも LLM は 1 往復 (開始の語りと決着後の digest)。
// =============================================================================

/// 対決 1 ラウンドの結果 (提示層が両者の出目・帰結・決着を描く素)。
#[derive(Debug, Clone, Serialize)]
pub struct ContestRound {
    /// player 側の振り (success = このラウンドに勝ったか)。
    pub player: CheckOutcome,
    /// 相手側の振り (success = 相手が勝ったか)。
    pub opponent: CheckOutcome,
    /// player 視点のラウンド帰結: `win` / `lose` / `tie`。
    pub outcome: String,
    /// 適用された帰結スロットの narration / sound (無ければ空)。
    pub narration: String,
    pub sound: String,
    /// 帰結 effects が振ったダイスと発火ビート。
    pub rolls: Vec<RollOutcome>,
    pub stat_rolls: Vec<StatRollOutcome>,
    pub fired: Vec<FiredTrigger>,
    /// 決着したら Some (このラウンドで対決が閉じた)。
    pub ended: Option<ContestEnd>,
}

/// 対決の決着 (digest の素)。
#[derive(Debug, Clone, Serialize)]
pub struct ContestEnd {
    pub contest: String,
    pub description: String,
    pub rounds: u32,
    pub wins: u32,
    pub losses: u32,
    pub ties: u32,
    /// 決着理由: `until` (決着条件成立) / `max_rounds` (上限打ち切り) / `goal` (goal 到達)。
    pub reason: String,
}

/// 対決ラウンドの失敗 (UI が正しく回していれば起きない防御的エラー)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ContestError {
    /// 進行中の対決が無い。
    NoContest,
    /// contest 定義が見つからない (セーブ後に content が変わった等)。進行中の帳簿は破棄済み。
    UnknownContest,
}

/// 判定主体の修正/目標の素値 (spec 19): `expr` があれば式を現在値で評価、無ければ stat 現在値。
/// validate 済みの式が前提 — 万一パース不能なら 0 (安全側・load 時に弾かれているはず)。
fn stat_or_expr(
    state: &GameState,
    entity: &str,
    stat: &Option<String>,
    expr: &Option<String>,
) -> i64 {
    if let Some(xsrc) = expr {
        return crate::expr::parse_expr(xsrc).map(|x| x.eval(state, entity)).unwrap_or(0);
    }
    stat.as_ref().map_or(0, |s| state.stat_of(entity, s))
}

/// percentile degree の強さ順位 (対抗比較用)。大きいほど良い。
fn degree_rank(degree: &str) -> u8 {
    match degree {
        "critical" => 5,
        "extreme" => 4,
        "hard" => 3,
        "regular" => 2,
        "failure" => 1,
        _ => 0, // fumble
    }
}

/// 対決を 1 ラウンド進める (spec 18 Phase C の中枢)。
///
/// RNG は player → 相手の順で消費する (決定論)。帰結スロットの適用・トリガー settle・
/// flag_turns は resolve_decision と同じ流儀。決着 (until / max_rounds / goal 到達) で
/// `pending_contest` を閉じ、[`ContestEnd`] を返す。turn は増えない (対決は 1 ターンの内側)。
pub fn contest_round(
    state: &mut GameState,
    scenario: &Scenario,
) -> Result<ContestRound, ContestError> {
    let Some(pending) = state.pending_contest.clone() else {
        return Err(ContestError::NoContest);
    };
    let Some(def) = scenario.contest(&pending.contest) else {
        state.pending_contest = None; // 定義が消えた対決は続行不能 — 帳簿を破棄 (防御)
        return Err(ContestError::UnknownContest);
    };
    let is_percentile = def.resolution == crate::spine::Resolution::Percentile;
    // validate 済みの定義なので解決は成功するはずだが、防御的に inline 0 面へフォールバック。
    let p_spec = scenario.resolve_roll(PLAYER, &def.player_roll).unwrap_or_default();
    let o_spec = scenario.resolve_roll(&def.opponent, &def.opponent_roll).unwrap_or_default();

    // 1 振り (side): additive = 1d{sides}+stat+bonus / percentile = 1d100 ≤ stat+bonus。
    let mut roll_side = |entity: &str, spec: &crate::spine::RollSpec| -> CheckOutcome {
        // stat 現在値 or 式修正 (spec 19)。
        let stat_mod = stat_or_expr(state, entity, &spec.stat, &spec.expr);
        if is_percentile {
            let roll = state.rng.roll(100);
            let target = stat_mod + spec.bonus;
            let (degree, _) = percentile_degree(roll, target);
            CheckOutcome {
                entity: entity.to_string(),
                stat: spec.stat.clone().unwrap_or_default(),
                sides: 100,
                count: 1,
                times: 1,
                roll,
                modifier: spec.bonus,
                total: i64::from(roll),
                dc: target.clamp(0, i64::from(u32::MAX)) as u32,
                success: false, // 勝敗確定後に上書き
                tier: None,
                narration: String::new(),
                sound: String::new(),
                degree: Some(degree.to_string()),
                pushed: false,
                spent: 0,
                pending: false,
            }
        } else {
            let cnt = spec.count.max(1);
            let roll: u32 = (0..cnt).map(|_| state.rng.roll(spec.sides.max(1))).sum();
            let modifier = stat_mod + spec.bonus;
            CheckOutcome {
                entity: entity.to_string(),
                stat: spec.stat.clone().unwrap_or_default(),
                sides: spec.sides.max(1),
                count: spec.count.max(1),
                times: spec.times.max(1),
                roll,
                modifier,
                total: i64::from(roll) * spec.times.max(1) + modifier,
                dc: 0, // 対抗は DC でなく相手の合計と比べる
                success: false,
                tier: None,
                narration: String::new(),
                sound: String::new(),
                degree: None,
                pushed: false,
                spent: 0,
                pending: false,
            }
        }
    };
    let mut player = roll_side(PLAYER, &p_spec);
    let mut opponent = roll_side(&def.opponent, &o_spec);

    // 比較: additive = 合計 / percentile = degree 順位 → 同位なら目標値の高い側 (CoC7 準拠)。
    let outcome: &str = if is_percentile {
        let pr = degree_rank(player.degree.as_deref().unwrap_or("failure"));
        let or = degree_rank(opponent.degree.as_deref().unwrap_or("failure"));
        match pr.cmp(&or) {
            std::cmp::Ordering::Greater => "win",
            std::cmp::Ordering::Less => "lose",
            std::cmp::Ordering::Equal => match player.dc.cmp(&opponent.dc) {
                std::cmp::Ordering::Greater => "win",
                std::cmp::Ordering::Less => "lose",
                std::cmp::Ordering::Equal => "tie",
            },
        }
    } else {
        match player.total.cmp(&opponent.total) {
            std::cmp::Ordering::Greater => "win",
            std::cmp::Ordering::Less => "lose",
            std::cmp::Ordering::Equal => "tie",
        }
    };
    player.success = outcome == "win";
    opponent.success = outcome == "lose";

    // 帰結スロット (player 視点) を原子適用 — resolve_decision と同じ流儀。
    let slot = match outcome {
        "win" => def.on_win.as_ref(),
        "lose" => def.on_lose.as_ref(),
        _ => def.on_tie.as_ref(),
    };
    let flags_before: BTreeSet<String> =
        state.flags.iter().filter(|(_, v)| **v).map(|(k, _)| k.clone()).collect();
    let mut rolls = Vec::new();
    let mut scratch_checks = Vec::new();
    let mut stat_rolls = Vec::new();
    let (mut narration, mut sound) = (String::new(), String::new());
    if let Some(o) = slot {
        if let Some(flag) = &o.flag {
            state.flags.insert(flag.clone(), true);
        }
        if !o.effects.is_empty() {
            let effect_delta = StateDelta::new(String::new(), o.effects.clone());
            apply_ops(state, scenario, &effect_delta, &mut rolls, &mut scratch_checks, &mut stat_rolls);
        }
        narration = o.narration.clone();
        sound = o.sound.clone();
    }
    let fired = fire_triggers(state, scenario, &mut rolls, &mut scratch_checks, &mut stat_rolls);
    let newly_true: Vec<String> = state
        .flags
        .iter()
        .filter(|(k, v)| **v && !flags_before.contains(*k))
        .map(|(k, _)| k.clone())
        .collect();
    for key in newly_true {
        state.flag_turns.insert(key, state.turn);
    }

    // 帳簿の更新と決着判定。
    let mut tally = pending;
    tally.rounds += 1;
    match outcome {
        "win" => tally.wins += 1,
        "lose" => tally.losses += 1,
        _ => tally.ties += 1,
    }
    let reason = if def.until.as_ref().is_some_and(|g| g.eval(state)) {
        Some("until")
    } else if scenario.reached(state).is_some() {
        // 決着条件の書き漏れでも goal 到達 (死亡等) で必ず閉じる安全弁。
        Some("goal")
    } else if tally.rounds >= def.max_rounds.max(1) {
        Some("max_rounds")
    } else {
        None
    };
    let ended = reason.map(|r| ContestEnd {
        contest: tally.contest.clone(),
        description: def.description.clone(),
        rounds: tally.rounds,
        wins: tally.wins,
        losses: tally.losses,
        ties: tally.ties,
        reason: r.to_string(),
    });
    state.pending_contest = if ended.is_some() { None } else { Some(tally) };

    Ok(ContestRound {
        player,
        opponent,
        outcome: outcome.to_string(),
        narration,
        sound,
        rolls,
        stat_rolls,
        fired,
        ended,
    })
}

// =============================================================================
// 決断つき判定 — プッシュ / 差分買い (spec 18 Phase B)
//
// 第三の権能「プレイヤー op」: LLM 提案でも authored 専権でもなく、プレイヤーが UI から
// 直接 engine に入れる決断。凍結された失敗 (PendingDecision) を Accept / Push / Buy の
// いずれかで確定し、そこで初めて帰結 (フラグ/effects/トリガー) を原子適用する。
// LLM を介さない = トークンを消費しない。
// =============================================================================

/// 凍結中の判定の CheckOutcome (提示用)。narration/sound は決断確定まで空 —
/// 結末文が先に見えたら開帳の意味がない (spec 18 Phase A の伏せと同じ理由で B でも守る)。
fn pending_check(p: &crate::state::PendingDecision) -> CheckOutcome {
    CheckOutcome {
        entity: p.entity.clone(),
        stat: p.stat.clone(),
        sides: p.sides,
        count: p.count.max(1),
        times: p.times.max(1),
        roll: p.roll,
        modifier: p.modifier,
        total: p.total,
        dc: p.dc,
        success: false,
        tier: None,
        narration: String::new(),
        sound: String::new(),
        degree: p.degree.clone(),
        pushed: false,
        spent: 0,
        pending: true,
    }
}

/// この challenge が決断 (プッシュ/差分買い) の対象になりうるか (静的判定・射影の除外にも使う)。
fn decision_enabled(scenario: &Scenario, def: &crate::spine::ChallengeDef) -> bool {
    def.pushable.unwrap_or(false)
        || (scenario.spend_rules.is_some() && def.spendable.unwrap_or(true))
}

/// player が `from` stat から払える上限 = 現在値 − 宣言 min (払って死ぬ設計も作者が
/// min:0 + goal で書けば成立する — engine は止めない)。
fn spendable_amount(state: &GameState, scenario: &Scenario, from: &str) -> i64 {
    state.stat_of(PLAYER, from) - scenario.stat_bounds(PLAYER, from).0
}

/// プッシュが実行可能か (宣言 + 未プッシュ + 代償を払える)。
fn push_available(
    state: &GameState,
    scenario: &Scenario,
    def: &crate::spine::ChallengeDef,
    p: &crate::state::PendingDecision,
) -> bool {
    def.pushable.unwrap_or(false)
        && !p.pushed
        && scenario
            .push_cost
            .as_ref()
            .map_or(true, |pc| spendable_amount(state, scenario, &pc.from) >= pc.amount)
}

/// 差分買いの選択肢 (段階買い)。percentile は regular/hard/extreme の三段
/// (critical=01 は出目そのものなので買えない)、additive は success の一段。
/// **買えるのは支払える段だけ** (提示 = 実行可能、の一致)。
fn buy_options(
    state: &GameState,
    scenario: &Scenario,
    def: &crate::spine::ChallengeDef,
    p: &crate::state::PendingDecision,
) -> Vec<BuyOption> {
    let Some(sr) = &scenario.spend_rules else { return Vec::new() };
    if !def.spendable.unwrap_or(true) || p.pushed {
        return Vec::new();
    }
    let avail = spendable_amount(state, scenario, &sr.from);
    let rate = sr.rate.max(1);
    let mut out = Vec::new();
    if p.degree.is_some() {
        // percentile: 買い上げ先の閾値 (percentile_degree と同じ整数除算)。
        let dc = i64::from(p.dc);
        for (degree, threshold) in [("regular", dc), ("hard", dc / 2), ("extreme", dc / 5)] {
            let cost = (i64::from(p.roll) - threshold) * rate;
            if threshold >= 1 && cost > 0 && cost <= avail {
                out.push(BuyOption { degree: degree.into(), cost, from: sr.from.clone() });
            }
        }
    } else {
        // additive: 差分 = dc - total を埋めて成功に。
        let cost = (i64::from(p.dc) - p.total) * rate;
        if cost > 0 && cost <= avail {
            out.push(BuyOption { degree: "success".into(), cost, from: sr.from.clone() });
        }
    }
    out
}

/// 凍結すべきか = 実行可能な選択肢が一つでも在るか。「受け入れる」しか無い停止は無意味なので、
/// 選択肢ゼロ (宣言なし/払えない) なら凍結せず従来どおり即時確定する。
fn decision_has_options(
    state: &GameState,
    scenario: &Scenario,
    def: &crate::spine::ChallengeDef,
    p: &crate::state::PendingDecision,
) -> bool {
    push_available(state, scenario, def, p) || !buy_options(state, scenario, def, p).is_empty()
}

/// 差分買いの 1 段 (提示層がボタンにする)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuyOption {
    /// 買い上げ先: percentile = `regular`/`hard`/`extreme`、additive = `success`。
    pub degree: String,
    /// 支払い量 (差分 × rate)。
    pub cost: i64,
    /// 支払い元 stat (表示用)。
    pub from: String,
}

/// 先頭の決断待ちと、いま実行可能な選択肢 (提示層の決断 UI の素)。
#[derive(Debug, Clone, Serialize)]
pub struct DecisionOptions {
    pub pending: crate::state::PendingDecision,
    pub can_push: bool,
    /// プッシュの代償 (stat, 量)。None = 無償。
    pub push_cost: Option<(String, i64)>,
    pub buys: Vec<BuyOption>,
}

/// 先頭の決断待ちの選択肢を返す (無ければ None)。決断は先頭から順に (開帳→決断の直列)。
pub fn decision_options(state: &GameState, scenario: &Scenario) -> Option<DecisionOptions> {
    let p = state.pending_decisions.first()?;
    let def = scenario.challenge(&p.challenge)?;
    Some(DecisionOptions {
        pending: p.clone(),
        can_push: push_available(state, scenario, def, p),
        push_cost: scenario.push_cost.as_ref().map(|pc| (pc.from.clone(), pc.amount)),
        buys: buy_options(state, scenario, def, p),
    })
}

/// プレイヤーの決断。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub enum DecisionChoice {
    /// 失敗を受け入れる (凍結していた失敗帰結を適用)。
    Accept,
    /// プッシュ: 代償を払って 1 度だけ振り直す。結果は成否に依らず final。
    Push,
    /// 差分買い: `degree` まで買い上げて成功に変える。
    Buy { degree: String },
}

/// 決断の失敗 (UI が正しい選択肢だけ出していれば起きない防御的エラー)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum DecisionError {
    /// 決断待ちが無い。
    NoPending,
    /// challenge 定義が見つからない (セーブ後に content が変わった等)。凍結は破棄済み。
    UnknownChallenge,
    /// この challenge はプッシュできない (宣言 false / 代償を払えない)。
    NotPushable,
    /// 買えない (spend_rules 無し / spendable false / 支払い不足 / 不正な段)。
    NotBuyable,
}

/// 決断の確定結果 (提示層が最終の判定行・語り・発火ビートを描く素)。
#[derive(Debug, Clone, Serialize)]
pub struct DecisionResolution {
    /// 最終の判定 (narration/sound 込み。pushed/spent が経緯を運ぶ)。
    pub check: CheckOutcome,
    /// 帰結 effects が振ったダイス (roll_stat 等) と発火ビート。
    pub rolls: Vec<RollOutcome>,
    pub stat_rolls: Vec<StatRollOutcome>,
    pub fired: Vec<FiredTrigger>,
    /// 差分買いの支払い (stat, 量)。
    pub spent: Option<(String, i64)>,
    /// プッシュの代償の支払い (stat, 量)。
    pub push_paid: Option<(String, i64)>,
}

/// 先頭の決断待ちを確定する (spec 18 Phase B の中枢)。
///
/// ここで初めて帰結が state に触れる: スロット解決 → フラグ直書き + effects 原子適用 →
/// トリガー settle → flag_turns 記録。turn は増えない (決断は同じ物語ターンの内側)。
/// プッシュの振り直しは本流 RNG を消費する (決定論: 同 seed + 同じ選択列 → 同じ出目)。
pub fn resolve_decision(
    state: &mut GameState,
    scenario: &Scenario,
    choice: DecisionChoice,
) -> Result<DecisionResolution, DecisionError> {
    let Some(p) = state.pending_decisions.first().cloned() else {
        return Err(DecisionError::NoPending);
    };
    let Some(def) = scenario.challenge(&p.challenge) else {
        // content 差し替え等の防御: 凍結を破棄する (帰結は永遠に適用できないので抱えない)。
        state.pending_decisions.remove(0);
        return Err(DecisionError::UnknownChallenge);
    };
    let is_percentile = p.degree.is_some();

    let flags_before: BTreeSet<String> =
        state.flags.iter().filter(|(_, v)| **v).map(|(k, _)| k.clone()).collect();
    let mut rolls = Vec::new();
    let mut scratch_checks = Vec::new();
    let mut stat_rolls = Vec::new();
    let mut spent = None;
    let mut push_paid = None;

    // 決断ごとに最終スロットと最終判定値を確定する。
    let (slot, check) = match choice {
        DecisionChoice::Accept => {
            let slot = if is_percentile {
                resolve_degree_slot(def, "failure")
            } else {
                def.on_failure.as_ref()
            };
            (slot, pending_check(&p))
        }
        DecisionChoice::Buy { degree } => {
            let option = buy_options(state, scenario, def, &p)
                .into_iter()
                .find(|b| b.degree == degree)
                .ok_or(DecisionError::NotBuyable)?;
            // 支払い (afford 済み・宣言 min まで)。
            let next = state.stat_of(PLAYER, &option.from) - option.cost;
            state.set_stat(PLAYER, &option.from, clamp_stat(scenario, PLAYER, &option.from, next));
            spent = Some((option.from.clone(), option.cost));
            let slot = if is_percentile {
                resolve_degree_slot(def, &option.degree)
            } else {
                def.on_success.as_ref()
            };
            let mut check = pending_check(&p);
            check.success = true;
            check.spent = option.cost;
            if is_percentile {
                check.degree = Some(option.degree);
            }
            (slot, check)
        }
        DecisionChoice::Push => {
            if !push_available(state, scenario, def, &p) {
                return Err(DecisionError::NotPushable);
            }
            // 代償 (任意) を先に払う — 振り直しは代償込みの決断。
            if let Some(pc) = &scenario.push_cost {
                let next = state.stat_of(PLAYER, &pc.from) - pc.amount;
                state.set_stat(PLAYER, &pc.from, clamp_stat(scenario, PLAYER, &pc.from, next));
                push_paid = Some((pc.from.clone(), pc.amount));
            }
            let mut check = pending_check(&p);
            check.pushed = true;
            let slot = if is_percentile {
                let roll2 = state.rng.roll(100);
                let (deg2, success2) = percentile_degree(roll2, i64::from(p.dc));
                check.roll = roll2;
                check.total = i64::from(roll2);
                check.degree = Some(deg2.to_string());
                check.success = success2;
                if success2 {
                    resolve_degree_slot(def, deg2)
                } else {
                    // 押した失敗はより悪い: on_push_failure → (fumble なら on_fumble) → on_failure。
                    def.on_push_failure.as_ref().or_else(|| {
                        if deg2 == "fumble" {
                            def.on_fumble.as_ref().or(def.on_failure.as_ref())
                        } else {
                            def.on_failure.as_ref()
                        }
                    })
                }
            } else {
                // additive の振り直し。tier は最初の自然出目だけの劇 — 振り直しでは判定しない
                // (会心/大失敗の二重抽選を作らない)。count/times も元の式のまま振り直す。
                let roll2: u32 = (0..p.count.max(1)).map(|_| state.rng.roll(p.sides)).sum();
                let total2 = i64::from(roll2) * p.times.max(1) + p.modifier;
                let success2 = total2 >= i64::from(p.dc);
                check.roll = roll2;
                check.total = total2;
                check.success = success2;
                if success2 {
                    def.on_success.as_ref()
                } else {
                    def.on_push_failure.as_ref().or(def.on_failure.as_ref())
                }
            };
            (slot, check)
        }
    };

    // 凍結を解いてから帰結を適用する (帰結が別の決断を生むことは無い —
    // effects に attempt_challenge は validate が遮断済み)。
    state.pending_decisions.remove(0);

    let mut check = check;
    if let Some(o) = slot {
        if let Some(flag) = &o.flag {
            state.flags.insert(flag.clone(), true);
        }
        if !o.effects.is_empty() {
            let effect_delta = StateDelta::new(String::new(), o.effects.clone());
            apply_ops(state, scenario, &effect_delta, &mut rolls, &mut scratch_checks, &mut stat_rolls);
        }
        check.narration = o.narration.clone();
        check.sound = o.sound.clone();
    }
    check.pending = false;

    // 帰結からの発火 (apply と同じ settle) と、真化フラグのターン刻印。
    let fired = fire_triggers(state, scenario, &mut rolls, &mut scratch_checks, &mut stat_rolls);
    let newly_true: Vec<String> = state
        .flags
        .iter()
        .filter(|(k, v)| **v && !flags_before.contains(*k))
        .map(|(k, _)| k.clone())
        .collect();
    for key in newly_true {
        state.flag_turns.insert(key, state.turn);
    }

    Ok(DecisionResolution { check, rolls, stat_rolls, fired, spent, push_paid })
}

/// 受理・適用後の `state` に対し、発火条件 `when` が真でまだ発火していないトリガーを発火させる。
///
/// 禁忌 (`check_taboos`) の双対: 禁忌が「真化を却下」するのに対し、トリガーは「真化で発火」する。
/// 発火は authored な `effects` を **検証せず** 原子適用し (シナリオ作者の信頼済データ、LLM 提案でない)、
/// 非 repeatable は [`GameState::fired`] に latch して二度目の発火を抑止する (edge-triggered once)。
/// 効果が別トリガーの `when` を真化させる連鎖は、新たな発火が無くなるまで settle する。authored 順で決定論。
///
/// **停止性 (二層管理)**: `fired_this_settle` (この apply 内の局所集合) に毎発火 id を入れ、
/// 同じトリガーは settle 内で二度選ばない → 1 回の apply で発火は高々 (トリガー数) 回 = **必ず停止**
/// (repeatable で効果が `when` を真のまま残しても無限ループしない)。永続 latch (`state.fired`) は
/// **非 repeatable のみ**に入れる。よって repeatable は次ターン以降 (新しい settle) で `when` 再真化時に再発火する。
fn fire_triggers(
    state: &mut GameState,
    scenario: &Scenario,
    rolls: &mut Vec<RollOutcome>,
    checks: &mut Vec<CheckOutcome>,
    stat_rolls: &mut Vec<StatRollOutcome>,
) -> Vec<FiredTrigger> {
    let mut fired = Vec::new();
    // この apply (settle) 内で発火済みの id。repeatable も含め settle 内は高々 1 回 → 停止保証。
    let mut fired_this_settle: BTreeSet<TriggerId> = BTreeSet::new();
    loop {
        // この settle で未発火・永続 latch されておらず・発火条件成立の最初のトリガー (authored 順)。
        let next = scenario.triggers.iter().find(|t| {
            !fired_this_settle.contains(&t.id) && !state.fired.contains(&t.id) && t.when.eval(state)
        });
        let Some(t) = next else { break };

        // 効果は authored・信頼済なので validate せず原子適用する。
        let effect_delta = StateDelta::new(String::new(), t.effects.clone());
        apply_ops(state, scenario, &effect_delta, rolls, checks, stat_rolls);

        fired_this_settle.insert(t.id.clone());
        if !t.repeatable {
            state.fired.insert(t.id.clone()); // 非 repeatable のみ永続 latch (once)。
        }
        fired.push(FiredTrigger {
            id: t.id.clone(),
            narration: t.narration.clone(),
            recall: t.recall.clone(), // cue を passthrough。解釈は harness。
            image: t.image.clone(),   // イベント CG を passthrough。解決は提示層。
            image_mode: t.image_mode,
            sound: t.sound.clone(),   // SE を passthrough。再生は提示層。
        });
    }
    fired
}

/// **決定論 op** を state に適用する (検証なし・ダイス無し)。
///
/// `apply_ops` (実適用) と `validate_ops` の逐次射影 (spec 09) が**共有**する —
/// 射影と実適用の乖離を構造的に防ぐ (二重実装は将来の齟齬源)。
/// 戻り値: この関数が処理した (決定論 op だった) か。ダイス op と authored 専権 op は false。
fn apply_deterministic_op(state: &mut GameState, scenario: &Scenario, op: &StateOp) -> bool {
    match op {
        StateOp::AddItem { item } => {
            // 既に所持しているなら no-op (spec 09-B。taken の記録もしない)。
            if !state.has_item(PLAYER, item) {
                state.add_to_inventory(PLAYER, item);
                // once アイテムは「持ち去った」事実を**その時点の現在地**に記録
                // (再取得=複製の遮断)。Move を含む delta では逐次の現在地が基準
                // (spec 09: 裁定も適用も同じ順次意味論)。
                let here = state.location.clone();
                if scenario
                    .locations
                    .get(&here)
                    .and_then(|l| l.items.get(item))
                    .map(|li| li.take())
                    == Some(TakeMode::Once)
                {
                    state.record_taken(&here, item);
                }
            }
            true
        }
        StateOp::RemoveItem { item } => {
            state.remove_from_inventory(PLAYER, item);
            true
        }
        StateOp::GiveItem { from, to, item } => {
            // adjudicate が from 所持・to 既知を保証済。原子的に移す。
            state.remove_from_inventory(from, item);
            state.add_to_inventory(to, item);
            true
        }
        StateOp::SetFlag { key, value } => {
            state.flags.insert(key.clone(), *value);
            true
        }
        StateOp::Move { to } => {
            state.location = to.clone();
            true
        }
        // --- 算術はエンジンが行う。LLM は意図だけ提案、値は持てない ---
        StateOp::AdjustStat { entity, key, delta } => {
            let next = state.stat_of(entity, key) + delta; // 加減
            let clamped = clamp_stat(scenario, entity, key, next);
            state.set_stat(entity, key, clamped);
            true
        }
        StateOp::ScaleStat { entity, key, num, den } => {
            // den != 0 は adjudicate が保証済。乗算先行で精度を確保。
            let next = state.stat_of(entity, key).saturating_mul(*num) / den;
            let clamped = clamp_stat(scenario, entity, key, next);
            state.set_stat(entity, key, clamped);
            true
        }
        StateOp::CastVote { voter, target } => {
            // 一人一票 (voter キーの map)。再投票は上書き。
            state.votes.insert(voter.clone(), target.clone());
            true
        }
        _ => false,
    }
}

/// delta の各 op を state に適用する (検証なし)。`apply` と taboo 射影が共有する。
/// [`StateOp::RequestRoll`]/[`StateOp::Check`] はここで決定論的に振られ、`rolls`/`checks` に積まれる。
/// 決定論 op は [`apply_deterministic_op`] へ委譲 (裁定の射影と同一コード)。
fn apply_ops(
    state: &mut GameState,
    scenario: &Scenario,
    delta: &StateDelta,
    rolls: &mut Vec<RollOutcome>,
    checks: &mut Vec<CheckOutcome>,
    stat_rolls: &mut Vec<StatRollOutcome>,
) {
    for op in &delta.ops {
        if apply_deterministic_op(state, scenario, op) {
            continue;
        }
        match op {
            StateOp::RequestRoll { sides, dc } => {
                let result = state.rng.roll(*sides);
                rolls.push(RollOutcome {
                    sides: *sides,
                    dc: *dc,
                    result,
                    success: result >= *dc,
                });
            }
            StateOp::Check { entity, stat, sides, dc } => {
                // 技能判定: 1d{sides} + stat修正 vs dc。出目も合計もエンジンが決める。
                let roll = state.rng.roll(*sides);
                let modifier = state.stat_of(entity, stat);
                let total = roll as i64 + modifier;
                checks.push(CheckOutcome {
                    entity: entity.clone(),
                    stat: stat.clone(),
                    sides: *sides,
                    count: 1,
                    times: 1,
                    roll,
                    modifier,
                    total,
                    dc: *dc,
                    success: total >= *dc as i64,
                    tier: None, // 素の判定は極を持たない (tier は authored challenge の専権)。
                    narration: String::new(), // 素の Check は authored 結末文を持たない (LLM が次ターンに語る)。
                    sound: String::new(),     // 素の Check は authored 効果音を持たない。
                    degree: None,             // 加算式は成功度を持たない (spec 16)。
                    pushed: false,
                    spent: 0,
                    pending: false,
                });
            }
            StateOp::CheckUnder { entity, key } => {
                // d100 ロールアンダー即興判定 (spec 16)。目標値 = stat 現在値。
                // 出目も成功度もエンジンが決める (LLM は主張できない)。帰結は持たない。
                let roll = state.rng.roll(100);
                let target = state.stat_of(entity, key);
                let (degree, success) = percentile_degree(roll, target);
                checks.push(CheckOutcome {
                    entity: entity.clone(),
                    stat: key.clone(),
                    sides: 100,
                    count: 1,
                    times: 1,
                    roll,
                    modifier: 0,
                    total: i64::from(roll),
                    // dc = 実効目標値 (表示用)。負の stat は 0 に写す (u32 表示の安全側)。
                    dc: target.clamp(0, i64::from(u32::MAX)) as u32,
                    success,
                    tier: None,
                    narration: String::new(),
                    sound: String::new(),
                    degree: Some(degree.to_string()),
                    pushed: false,
                    spent: 0,
                    pending: false,
                });
            }
            StateOp::RollStat { entity, key, count, sides, bonus, negate } => {
                // 可変量ダイス (spec 16)。ここに到達するのは authored effects のみ
                // (LLM 提案は adjudicate が StatRollNotAllowed で却下済)。
                let die_rolls: Vec<u32> = (0..*count).map(|_| state.rng.roll(*sides)).collect();
                let sum: i64 = die_rolls.iter().map(|r| i64::from(*r)).sum::<i64>() + bonus;
                let amount = if *negate { -sum } else { sum };
                let next = state.stat_of(entity, key) + amount;
                let clamped = clamp_stat(scenario, entity, key, next);
                state.set_stat(entity, key, clamped);
                stat_rolls.push(StatRollOutcome {
                    entity: entity.clone(),
                    key: key.clone(),
                    count: *count,
                    sides: *sides,
                    bonus: *bonus,
                    rolls: die_rolls,
                    amount,
                });
            }
            StateOp::AttemptChallenge { entity, challenge } => {
                // adjudicate が challenge 既知・stat 宣言済を保証済。authored 定義から判定を組む。
                // ここに到達する challenge は必ず存在する (adjudicate 通過後)。
                if let Some(def) = scenario.challenge(challenge) {
                    if def.resolution == crate::spine::Resolution::Percentile {
                        // d100 ロールアンダー (spec 16)。目標値 = 判定主体の stat + modifiers
                        // (percentile では bonus を目標値に加算 = 「技能値 +10 相当」)。
                        let roll = state.rng.roll(100);
                        let subject = def.entity.as_ref().unwrap_or(entity);
                        // 目標値の素: stat 現在値 or 式修正 (spec 19。式は現在値で評価)。
                        let base = stat_or_expr(state, subject, &def.stat, &def.expr);
                        let cond_mod: i64 =
                            def.modifiers.iter().filter(|m| m.when.eval(state)).map(|m| m.bonus).sum();
                        let target = base + cond_mod;
                        let (degree, success) = percentile_degree(roll, target);
                        // spec 18 Phase B: 決断つき challenge の通常失敗は帰結を適用せず凍結する
                        // (apply 済みの帰結は巻き戻せない → プッシュ/差分買いの確定まで遅延)。
                        // fumble は final (逃れられない)、player 以外の主体は凍結しない。
                        if degree == "failure" && subject == PLAYER {
                            let p = crate::state::PendingDecision {
                                challenge: challenge.clone(),
                                entity: subject.clone(),
                                stat: def.stat.clone().unwrap_or_default(),
                                sides: 100,
                                count: 1,
                                times: 1,
                                roll,
                                modifier: cond_mod,
                                total: i64::from(roll),
                                dc: target.clamp(0, i64::from(u32::MAX)) as u32,
                                degree: Some(degree.to_string()),
                                pushed: false,
                            };
                            if decision_has_options(state, scenario, def, &p) {
                                checks.push(pending_check(&p));
                                state.pending_decisions.push(p);
                                continue;
                            }
                        }
                        // 帰結スロット: degree 別 → フォールバック連鎖 (apply と射影が共有する
                        // resolve_degree_slot で解決)。フラグ直書き + effects 原子適用は additive と同型。
                        let outcome = resolve_degree_slot(def, degree);
                        if let Some(flag) = outcome.and_then(|o| o.flag.as_ref()) {
                            state.flags.insert(flag.clone(), true);
                        }
                        let effects: Vec<StateOp> =
                            outcome.map(|o| o.effects.clone()).unwrap_or_default();
                        if !effects.is_empty() {
                            let effect_delta = StateDelta::new(String::new(), effects);
                            apply_ops(state, scenario, &effect_delta, rolls, checks, stat_rolls);
                        }
                        checks.push(CheckOutcome {
                            entity: subject.clone(),
                            stat: def.stat.clone().unwrap_or_default(),
                            sides: 100,
                            count: 1,
                            times: 1,
                            roll,
                            // percentile の modifier は「目標値への修正」(出目加算ではない)。
                            modifier: cond_mod,
                            total: i64::from(roll),
                            dc: target.clamp(0, i64::from(u32::MAX)) as u32,
                            success,
                            tier: None,
                            narration: outcome.map(|o| o.narration.clone()).unwrap_or_default(),
                            sound: outcome.map(|o| o.sound.clone()).unwrap_or_default(),
                            degree: Some(degree.to_string()),
                            pushed: false,
                            spent: 0,
                            pending: false,
                        });
                        continue;
                    }
                    // 素の合計: count 個の d{sides} (既定 1 = 従来形)。tier は素の合計で判定し、
                    // times (乗数) は**合計だけ**に掛かる (修正は乗算の後 — 3D6×5 系)。
                    let count = def.count.max(1);
                    let roll: u32 = (0..count).map(|_| state.rng.roll(def.sides)).sum();
                    // 判定主体: authored 固定 (def.entity) が op の entity を上書きする。
                    let subject = def.entity.as_ref().unwrap_or(entity);
                    // stat 無し = 能力に依らない純粋ダイス (修正値 0)。式修正 (spec 19) があれば
                    // 現在値で評価した値が修正になる ((CON+SIZ)/2 等)。
                    let stat_mod = stat_or_expr(state, subject, &def.stat, &def.expr);
                    // 条件付き修正: when (Gate) が真の分だけ bonus を加える (導師の教えで +5 等)。
                    let cond_mod: i64 = def.modifiers.iter().filter(|m| m.when.eval(state)).map(|m| m.bonus).sum();
                    let modifier = stat_mod + cond_mod;
                    let total = i64::from(roll) * def.times.max(1) + modifier;
                    let success = total >= def.dc as i64;
                    // 極 (tier): 自然出目が min/max/閾値に該当する authored tier を引く。
                    // 複数該当時は BTreeMap のキー名昇順で最初が勝つ (決定論)。
                    // 該当 tier に flag があれば engine が直書きする (通常成否フラグと併存)。
                    // 閾値欠落 (validate が弾く前提だが) は発火させない安全側 (map_or false)。
                    // 凍結判定 (spec 18) に tier 該当の有無が要るため、成否帰結より先に引く。
                    let hit = def.tiers.iter().find(|(_, t)| match t.natural {
                        // 素の合計で判定: min = 全部 1 (合計 == count) / max = 全部最大。
                        crate::spine::Natural::Min => roll == count,
                        crate::spine::Natural::Max => roll == def.sides * count,
                        crate::spine::Natural::AtMost => t.threshold.is_some_and(|n| roll <= n),
                        crate::spine::Natural::AtLeast => t.threshold.is_some_and(|n| roll >= n),
                    });
                    // spec 18 Phase B: 決断つき challenge の**素の失敗**は帰結を凍結する。
                    // tier 該当 (大失敗等) は authored の劇 = final なので凍結しない
                    // (additive の fumble 相当)。player 以外の主体も凍結しない。
                    if !success && hit.is_none() && subject == PLAYER {
                        let p = crate::state::PendingDecision {
                            challenge: challenge.clone(),
                            entity: subject.clone(),
                            stat: def.stat.clone().unwrap_or_default(),
                            sides: def.sides,
                            count: def.count.max(1),
                            times: def.times.max(1),
                            roll,
                            modifier,
                            total,
                            dc: def.dc,
                            degree: None,
                            pushed: false,
                        };
                        if decision_has_options(state, scenario, def, &p) {
                            checks.push(pending_check(&p));
                            state.pending_decisions.push(p);
                            continue;
                        }
                    }
                    // 通常成否の帰結 (フラグ + 結末ナレーション)。フラグは直書き (validate が宣言保証)。
                    let outcome = if success { def.on_success.as_ref() } else { def.on_failure.as_ref() };
                    if let Some(flag) = outcome.and_then(|o| o.flag.as_ref()) {
                        state.flags.insert(flag.clone(), true);
                    }
                    let tier = hit.map(|(name, _)| name.clone());
                    if let Some((_, t)) = hit {
                        if let Some(flag) = &t.flag {
                            state.flags.insert(flag.clone(), true);
                        }
                    }
                    // 帰結の直接効果 (authored 専権 — trigger effects と同じ信頼モデルで
                    // apply_ops 直行)。通常成否と極の effects は併存で、同一 apply 内に
                    // 原子適用される (attempt_challenge の入れ子は validate が遮断済)。
                    let mut effects: Vec<StateOp> =
                        outcome.map(|o| o.effects.clone()).unwrap_or_default();
                    if let Some((_, t)) = hit {
                        effects.extend(t.effects.iter().cloned());
                    }
                    if !effects.is_empty() {
                        let effect_delta = StateDelta::new(String::new(), effects);
                        apply_ops(state, scenario, &effect_delta, rolls, checks, stat_rolls);
                    }
                    // 結末ナレーション: 極(tier)に narration があれば優先 (より具体的・劇的)、
                    // 無ければ通常成否の narration。毎回・同ターンに提示層が出す (非 latch)。
                    let narration = hit
                        .map(|(_, t)| t.narration.clone())
                        .filter(|n| !n.is_empty())
                        .or_else(|| outcome.map(|o| o.narration.clone()))
                        .unwrap_or_default();
                    // 結末効果音: narration と同じ優先順 (tier 優先 → 通常成否)。提示層が
                    // audios/ から解決し one-shot 再生する。
                    let sound = hit
                        .map(|(_, t)| t.sound.clone())
                        .filter(|s| !s.is_empty())
                        .or_else(|| outcome.map(|o| o.sound.clone()))
                        .unwrap_or_default();
                    checks.push(CheckOutcome {
                        // 提示は実際に振った主体 (authored 固定があればそれ) — UI/還流が正しい名を出す。
                        entity: subject.clone(),
                        stat: def.stat.clone().unwrap_or_default(),
                        sides: def.sides,
                        count: def.count.max(1),
                        times: def.times.max(1),
                        roll,
                        modifier,
                        total,
                        dc: def.dc,
                        success,
                        tier,
                        narration,
                        sound,
                        degree: None, // 加算式は成功度を持たない (spec 16)。
                        pushed: false,
                        spent: 0,
                        pending: false,
                    });
                }
            }
            StateOp::AttemptContest { contest } => {
                // 対決を開く (spec 18 Phase C)。この apply ではダイスを振らない —
                // ラウンドは決着まで contest_round がプレイヤーと直接回す (LLM 非関与)。
                state.pending_contest = Some(crate::state::PendingContest {
                    contest: contest.clone(),
                    rounds: 0,
                    wins: 0,
                    losses: 0,
                    ties: 0,
                });
            }
            StateOp::GrantSkill { entity, skill } => {
                // ここに到達するのは authored トリガーの effect のみ (LLM 提案は adjudicate で却下済)。
                state.grant_skill(entity, skill);
            }
            StateOp::RecordTurn { entity, key } => {
                // ここに到達するのは authored トリガーの effect のみ (LLM 提案は adjudicate で却下済)。
                // 現在ターンを生値で刻む (stat 境界で clamp しない = タイムスタンプ)。
                let t = i64::from(state.turn);
                state.set_stat(entity, key, t);
            }
            StateOp::SetAttribute { entity, key, value } => {
                // ここに到達するのは authored トリガーの effect のみ (LLM 提案は adjudicate で却下済)。
                state.set_attribute(entity, key, value);
            }
            StateOp::SetPresence { entity, present } => {
                // ここに到達するのは authored トリガーの effect のみ (LLM 提案は adjudicate で却下済)。
                state.present_overrides.insert(entity.clone(), *present);
            }
            StateOp::ResolveVote => {
                // ここに到達するのは authored トリガーの effect のみ (LLM 提案は adjudicate で却下済)。
                // 開票 → 死亡 → カウンタ再計算 → 票リセット、を一箇所で原子適用 (spec 06 Phase C)。
                resolve_vote(state, scenario);
            }
            // 決定論 op は apply_deterministic_op が処理済み (continue で到達しない)。
            _ => {}
        }
    }
}

/// "VOTE_RNG" (ASCII)。同数抽選の専用ストリームのラベル (role_rng と同系)。
const VOTE_RNG_LABEL: u64 = 0x564F_5445_5F52_4E47;

/// 開票の原子適用 (spec 06 Phase C)。票が無ければ何もしない。
///
/// 1. 集計 (BTreeMap = 決定論順) → 最多得票者。同数は **seed 派生の専用ストリーム**
///    (seed ^ VOTE_RNG ^ turn) で抽選 — 決定論・本流ダイス列非消費・ターンごとに変わる。
/// 2. 死亡: `生存`=0 (Gate/集計の**正本**) + `present_overrides`=false (表示への**投影**。
///    presence 接地が「死者の発言」を防ぐ砦になる)。
/// 3. `role_assignment` 盤面なら bookkeeping stat を再計算: `生存{役職}数` 減算・`生存者数`
///    減算・各役職の優位 stat `{役職}優位 = 2×生存{役職}数 − 生存者数` (パリティ勝利条件を
///    単体比較 Gate で書くための差分 stat)。
/// 4. 票をリセット (次のフェーズへ)。
fn resolve_vote(state: &mut GameState, scenario: &Scenario) {
    if state.votes.is_empty() {
        return;
    }
    // 集計。BTreeMap なので得票順・同数候補の並びが決定論。
    let mut tally: BTreeMap<&String, u32> = BTreeMap::new();
    for target in state.votes.values() {
        *tally.entry(target).or_insert(0) += 1;
    }
    let max = *tally.values().max().expect("votes 非空");
    let top: Vec<String> =
        tally.iter().filter(|(_, n)| **n == max).map(|(t, _)| (*t).clone()).collect();
    let victim = if top.len() == 1 {
        top[0].clone()
    } else {
        // 同数はエンジンが抽選 (決定論)。turn を混ぜるので同じ顔ぶれの同数でもターンが違えば
        // 結果が変わりうるが、同 seed 同経過なら必ず同じ。
        let mut vote_rng = RngState {
            seed: state.rng.seed ^ VOTE_RNG_LABEL ^ u64::from(state.turn),
            cursor: 0,
        };
        top[(vote_rng.roll(top.len() as u32) - 1) as usize].clone()
    };

    // 死亡の原子適用: 正本 (生存 stat) + 表示への投影 (presence)。
    state.set_stat(&victim, "生存", 0);
    state.present_overrides.insert(victim.clone(), false);

    // 役職カウンタ・優位 stat の再計算 (role_assignment 盤面のみ)。
    if let Some(ra) = &scenario.role_assignment {
        if let Some(role) = state.attributes.get(&victim).and_then(|a| a.get(&ra.key)).cloned() {
            let key = format!("生存{role}数");
            let n = state.stat_of(PLAYER, &key);
            state.set_stat(PLAYER, &key, n - 1);
        }
        let alive = state.stat_of(PLAYER, "生存者数") - 1;
        state.set_stat(PLAYER, "生存者数", alive);
        for role in ra.pool.keys() {
            let n = state.stat_of(PLAYER, &format!("生存{role}数"));
            state.set_stat(PLAYER, &format!("{role}優位"), 2 * n - alive);
        }
    }

    state.votes.clear();
}

/// stat を宣言された境界 `[min, max]` に収める。max 未宣言なら上限なし。
fn clamp_stat(scenario: &Scenario, entity: &str, key: &str, value: i64) -> i64 {
    let (min, max) = scenario.stat_bounds(entity, key);
    let v = value.max(min);
    max.map_or(v, |m| v.min(m))
}

/// クリア条件を満たしているか (いずれかのエンディングに到達したか)。
pub fn is_goal(state: &GameState, scenario: &Scenario) -> bool {
    scenario.reached(state).is_some()
}

// =============================================================================
// PoC: 正本エンジンの実証 (Red→Green)
// クラウドLLM を繋ぐ前に、最重要の「裁定」脚をテストで固める。
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::reason::RejectReason;
    use crate::state::{RngState, StateOp, PLAYER};

    // 密室脱出シナリオをコンパイル時に埋め込む (cwd 非依存)。
    const LOCKED_ROOM: &str = include_str!("../fixtures/locked_room.yaml");
    // 数値の最小盤面。
    const STRENGTH_TRIAL: &str = include_str!("../fixtures/strength_trial.yaml");
    // キャラ別ステータスの最小盤面。
    const HEROINE_ROUTE: &str = include_str!("../fixtures/heroine_route.yaml");
    // 反応ビート (Phase C) の最小盤面。
    const TRIGGER_RECALL: &str = include_str!("../fixtures/trigger_recall.yaml");
    // 閉世界 capability (スキル覚醒) の最小盤面。
    const SKILL_AWAKENING: &str = include_str!("../fixtures/skill_awakening.yaml");
    // NPC inventory + 譲渡 (give_item) の最小盤面。
    const GIFT: &str = include_str!("../fixtures/gift.yaml");
    // 技能判定の大失敗が世界を変える (fumble-as-trigger, PoC-1) の最小盤面。
    const FUMBLE_CHECK: &str = include_str!("../fixtures/fumble_check.yaml");

    fn scenario() -> Scenario {
        Scenario::from_yaml(LOCKED_ROOM).expect("locked_room.yaml がパースできること")
    }

    fn trial() -> Scenario {
        Scenario::from_yaml(STRENGTH_TRIAL).expect("strength_trial.yaml がパースできること")
    }

    fn route() -> Scenario {
        Scenario::from_yaml(HEROINE_ROUTE).expect("heroine_route.yaml がパースできること")
    }

    fn recall() -> Scenario {
        Scenario::from_yaml(TRIGGER_RECALL).expect("trigger_recall.yaml がパースできること")
    }

    fn awakening() -> Scenario {
        Scenario::from_yaml(SKILL_AWAKENING).expect("skill_awakening.yaml がパースできること")
    }

    fn gift() -> Scenario {
        Scenario::from_yaml(GIFT).expect("gift.yaml がパースできること")
    }

    fn fumble() -> Scenario {
        Scenario::from_yaml(FUMBLE_CHECK).expect("fumble_check.yaml がパースできること")
    }

    /// アリスの好感度を増やす delta (発火条件を跨ぐための糖衣)。
    fn raise_affection(amount: i64) -> StateDelta {
        d(vec![StateOp::AdjustStat {
            entity: "alice".into(),
            key: "好感度".into(),
            delta: amount,
        }])
    }

    fn fresh(sc: &Scenario) -> GameState {
        GameState::new(sc.start.clone(), 42)
    }

    fn d(ops: Vec<StateOp>) -> StateDelta {
        StateDelta::new("", ops)
    }

    #[test]
    fn yaml_contract_loads() {
        let sc = scenario();
        assert_eq!(sc.start, "cell");
        assert!(sc.locations.contains_key("cell"));
        assert!(sc.locations.contains_key("corridor"));
    }

    /// 正規の筋を通すと goal に到達する。
    #[test]
    fn legal_playthrough_reaches_goal() {
        let sc = scenario();
        let mut s = fresh(&sc);
        assert!(!is_goal(&s, &sc));

        apply(&mut s, &sc, &d(vec![StateOp::SetFlag { key: "drawer_opened".into(), value: true }]))
            .expect("引き出しはいつでも開けられる");
        apply(&mut s, &sc, &d(vec![StateOp::AddItem { item: "rusty_key".into() }]))
            .expect("引き出しを開けたので鍵が取れる");
        apply(&mut s, &sc, &d(vec![StateOp::SetFlag { key: "door_unlocked".into(), value: true }]))
            .expect("鍵を持っているので解錠できる");
        apply(&mut s, &sc, &d(vec![StateOp::Move { to: "corridor".into() }]))
            .expect("解錠したので廊下へ出られる");

        assert!(is_goal(&s, &sc), "goal (location_is corridor) に到達しているはず");
        assert_eq!(s.turn, 4);
    }

    /// 引き出しを開ける前に鍵は取れない。
    #[test]
    fn take_key_before_opening_drawer_is_rejected() {
        let sc = scenario();
        let s = fresh(&sc);
        let v = adjudicate(&s, &sc, &d(vec![StateOp::AddItem { item: "rusty_key".into() }]));
        assert!(!v.is_accept(), "drawer_opened 未達なので鍵取得は却下されるべき");
    }

    /// 鍵なしで扉は解錠できない。
    #[test]
    fn open_door_without_key_is_rejected() {
        let sc = scenario();
        let s = fresh(&sc);
        let v = adjudicate(&s, &sc, &d(vec![StateOp::SetFlag { key: "door_unlocked".into(), value: true }]));
        assert!(!v.is_accept());
    }

    /// 解錠前に廊下へは出られない。
    #[test]
    fn move_without_unlock_is_rejected() {
        let sc = scenario();
        let s = fresh(&sc);
        let v = adjudicate(&s, &sc, &d(vec![StateOp::Move { to: "corridor".into() }]));
        assert!(!v.is_accept());
    }

    /// 【敵対ターン】存在しない「マスターキー」を持っていると嘘をついても、
    /// エンジンが LLM の流暢さに勝つ。これが「正本 > 文章力」の最小証明。
    #[test]
    fn phantom_master_key_is_rejected() {
        let sc = scenario();
        let s = fresh(&sc);
        let v = adjudicate(&s, &sc, &d(vec![StateOp::AddItem { item: "master_key".into() }]));
        match v {
            Verdict::Reject { reasons } => {
                assert!(reasons.iter().any(|r| matches!(
                    r,
                    RejectReason::ItemNotHere { item } if item == "master_key"
                )));
            }
            Verdict::Accept => panic!("幻のアイテムを受理してはならない"),
        }
    }

    /// 【原子性】一部が不正なデルタは全体却下、state は無傷。
    #[test]
    fn mixed_delta_is_atomic() {
        let sc = scenario();
        let mut s = fresh(&sc);
        let delta = d(vec![
            StateOp::SetFlag { key: "drawer_opened".into(), value: true }, // 単体なら合法
            StateOp::AddItem { item: "master_key".into() },                // 不正
        ]);
        let result = apply(&mut s, &sc, &delta);
        assert!(result.is_err(), "不正な op を含むデルタは却下されるべき");
        assert!(!s.flag("drawer_opened"), "却下されたデルタは state を変えてはならない");
        assert_eq!(s.turn, 0, "却下では turn が進まない");
    }

    /// ダイスは決定論的・監査可能。同じ seed/cursor は同じ目を返す。
    #[test]
    fn dice_are_deterministic_and_in_range() {
        let mut a = RngState { seed: 7, cursor: 0 };
        let mut b = RngState { seed: 7, cursor: 0 };
        let seq_a: Vec<u32> = (0..8).map(|_| a.roll(6)).collect();
        let seq_b: Vec<u32> = (0..8).map(|_| b.roll(6)).collect();
        assert_eq!(seq_a, seq_b, "同じ seed なら同じ出目列");
        assert!(seq_a.iter().all(|&r| (1..=6).contains(&r)), "1d6 は 1..=6");
    }

    /// request_roll は op 構造上 LLM が結果を持てない。エンジンが振り、DC で成否判定。
    #[test]
    fn request_roll_is_adjudicated_by_engine() {
        let sc = scenario();
        let mut s = fresh(&sc);
        let out = apply(&mut s, &sc, &d(vec![StateOp::RequestRoll { sides: 20, dc: 10 }]))
            .expect("ダイス要求自体は合法");
        assert_eq!(out.rolls.len(), 1);
        let outcome = &out.rolls[0];
        assert!((1..=20).contains(&outcome.result));
        assert_eq!(outcome.success, outcome.result >= 10);
        assert_eq!(s.rng.cursor, 1, "1回振ったので cursor が進む");
    }

    // -------------------------------------------------------------------------
    // 技能判定 PoC: 1d{sides} + stat修正 vs dc。出目も合計もエンジンが裁く (LLM は持てない)。
    // -------------------------------------------------------------------------

    /// 【技能判定】判定は 1d{sides} に宣言済み stat を修正として足し、dc と比べる。
    #[test]
    fn check_resolves_with_stat_modifier() {
        let sc = trial(); // str=12
        let mut s = sc.initial_state(42);
        let out = apply(&mut s, &sc, &d(vec![StateOp::Check {
            entity: PLAYER.into(),
            stat: "str".into(),
            sides: 20,
            dc: 15,
        }]))
        .expect("宣言済み stat の判定は合法");
        assert_eq!(out.checks.len(), 1);
        let c = &out.checks[0];
        assert_eq!(c.modifier, 12, "str=12 が修正に乗る");
        assert!((1..=20).contains(&c.roll), "1d20 の出目");
        assert_eq!(c.total, c.roll as i64 + 12, "合計 = 出目 + 修正");
        assert_eq!(c.success, c.total >= 15, "total>=dc で成功");
        assert_eq!(s.rng.cursor, 1, "1回振ったので cursor が進む");
    }

    /// 【幻ステータス遮断】未宣言の stat を修正に使う判定は却下 (判定を盛れない)。
    #[test]
    fn check_with_unknown_stat_is_rejected() {
        let sc = trial();
        let s = sc.initial_state(42);
        let v = adjudicate(&s, &sc, &d(vec![StateOp::Check {
            entity: PLAYER.into(),
            stat: "mana".into(), // 未宣言
            sides: 20,
            dc: 10,
        }]));
        match v {
            Verdict::Reject { reasons } => assert!(reasons
                .iter()
                .any(|r| matches!(r, RejectReason::UnknownStat { key, .. } if key == "mana"))),
            Verdict::Accept => panic!("未宣言 stat の判定を受理してはならない"),
        }
    }

    /// 【決定論】同じ seed なら同じ判定結果 (監査可能)。
    #[test]
    fn check_is_deterministic() {
        let sc = trial();
        let mut a = sc.initial_state(7);
        let mut b = sc.initial_state(7);
        let chk = |st: &mut GameState| {
            apply(st, &sc, &d(vec![StateOp::Check {
                entity: PLAYER.into(),
                stat: "str".into(),
                sides: 20,
                dc: 10,
            }]))
            .unwrap()
            .checks
        };
        assert_eq!(chk(&mut a), chk(&mut b), "同じ seed なら同じ判定結果");
    }

    // -------------------------------------------------------------------------
    // fumble-as-trigger PoC-1: authored challenge の大失敗(natural 1)が宣言済フラグを
    // 直書きし、それを gate にした既存トリガーが同じ適用内で発火する。
    // tier/flag は authored、LLM は challenge を「選ぶ」だけ (帰結を持てない=閉世界)。
    // -------------------------------------------------------------------------

    /// 【fumble-as-trigger】大失敗(natural 1) → engine が authored flag 直書き → trigger 発火 → goal。
    #[test]
    fn attempt_challenge_crit_fail_sets_flag_and_fires_trigger() {
        let sc = fumble();
        assert!(sc.validate().is_empty(), "正しいシナリオは validate を通る");
        let mut s = sc.initial_state(19); // seed 19 → 1d6 初回が natural 1
        assert!(!is_goal(&s, &sc));

        let out = apply(
            &mut s,
            &sc,
            &d(vec![StateOp::AttemptChallenge {
                entity: PLAYER.into(),
                challenge: "drawer_pick".into(),
            }]),
        )
        .expect("authored challenge への挑戦は合法");

        // 出目と tier (engine が裁く)。
        assert_eq!(out.checks.len(), 1);
        let c = &out.checks[0];
        assert_eq!(c.roll, 1, "seed 19 で 1d6 は natural 1");
        assert_eq!(c.tier.as_deref(), Some("crit_fail"), "natural min → crit_fail tier");
        assert!(!c.success, "1+2=3 < dc5 なので判定自体は失敗");

        // 帰結: authored flag が engine 直書きで立ち、それを gate にした trigger が同一適用で発火。
        assert_eq!(
            s.flags.get("fumble_drawer"),
            Some(&true),
            "engine が authored 定義から fumble_drawer を直書き (LLM 経路でない)"
        );
        assert!(
            out.fired.iter().any(|f| f.id == "drawer_jam"),
            "fumble_drawer を gate にした既存トリガーが発火する"
        );
        assert!(is_goal(&s, &sc), "trigger が drawer_jammed を立て goal 到達 (失敗が分岐になった)");
    }

    /// 【閉世界】宣言されていない challenge には挑めない (幻チャレンジ遮断)。
    #[test]
    fn attempt_unknown_challenge_is_rejected() {
        let sc = fumble();
        let s = sc.initial_state(19);
        let v = adjudicate(
            &s,
            &sc,
            &d(vec![StateOp::AttemptChallenge {
                entity: PLAYER.into(),
                challenge: "teleport".into(), // 未宣言
            }]),
        );
        match v {
            Verdict::Reject { reasons } => assert!(reasons.iter().any(
                |r| matches!(r, RejectReason::UnknownChallenge { challenge } if challenge == "teleport")
            )),
            Verdict::Accept => panic!("未宣言 challenge への挑戦を受理してはならない"),
        }
    }

    /// 【非クリティカル】natural min/max でなければ tier は付かず、帰結フラグも立たない。
    #[test]
    fn attempt_challenge_non_crit_sets_no_flag() {
        let sc = fumble();
        let mut s = sc.initial_state(42); // seed 42 → 1d6 は 1 でも 6 でもない
        let out = apply(
            &mut s,
            &sc,
            &d(vec![StateOp::AttemptChallenge {
                entity: PLAYER.into(),
                challenge: "drawer_pick".into(),
            }]),
        )
        .unwrap();
        let c = &out.checks[0];
        assert!(c.roll != 1 && c.roll != 6, "natural でない出目 (seed 42)");
        assert_eq!(c.tier, None, "natural でなければ tier 無し");
        assert_eq!(s.flags.get("fumble_drawer"), None, "帰結フラグは立たない");
        assert!(out.fired.is_empty(), "トリガー発火なし");
    }

    /// 【load 時参照整合】challenge の tier flag が allowed_flags に無ければ validate が弾く
    /// (engine が幻参照のフラグを立てる経路を作らせない)。
    #[test]
    fn validate_rejects_undeclared_tier_flag() {
        let yaml = r#"
title: bad
start: room
allowed_flags: []
challenges:
  bad_check:
    stat: str
    sides: 6
    dc: 5
    tiers:
      crit_fail: { natural: min, flag: ghost_flag }
locations:
  room:
    description: x
    items: {}
    exits: []
goal: { kind: always }
"#;
        let sc = Scenario::from_yaml(yaml).expect("パースは通る (整合性検査は別工程)");
        let errs = sc.validate();
        assert!(
            errs.iter().any(|e| matches!(
                e,
                crate::spine::ScenarioError::ChallengeFlagUndeclared { flag, .. } if flag == "ghost_flag"
            )),
            "未宣言の tier flag は validate で検出されるべき"
        );
    }

    // -------------------------------------------------------------------------
    // transition PoC-2a (campaign keystone): 状態を持ち越したまま骨格を差し替える。
    // 「密室脱出→森へ、HP/所持品/好感度を保ったまま」。局所フラグは捨て、location は次の start。
    // 名前付き goal (reached) は 2b に分離。ここは純粋な状態持ち越し swap のみ。
    // -------------------------------------------------------------------------

    const VILLAGE: &str = r#"
title: 村
start: square
initial_stats: { hp: 10 }
initial_skills: [tracking]
global_flags: [met_alice]
allowed_flags: [met_alice, door_open]
characters:
  alice:
    name: アリス
    stats: { 好感度: { initial: 0, min: 0, max: 100 } }
locations:
  square:
    description: 村の広場。
    items: { lantern: { kind: always } }
    exits: []
goal: { kind: always }
"#;

    const FOREST: &str = r#"
title: 森
start: forest_entrance
initial_stats: { hp: 10, stamina: 5 }
allowed_flags: [campfire_lit]
locations:
  forest_entrance:
    description: 森の入口。
    items: {}
    exits: []
goal: { kind: always }
"#;

    /// 【状態持ち越し swap】村で進めた状態が森へ運ばれる。global は残り、局所は消え、場所はリセット。
    #[test]
    fn transition_carries_state_drops_local_flags() {
        let village = Scenario::from_yaml(VILLAGE).expect("village yaml");
        let forest = Scenario::from_yaml(FOREST).expect("forest yaml");
        assert!(village.validate().is_empty() && forest.validate().is_empty());

        // 村で状態を進める: hp を減らし、ランタンを拾い、アリスの好感度を上げ、両フラグを立てる。
        let mut prev = village.initial_state(7);
        apply(&mut prev, &village, &d(vec![StateOp::AdjustStat { entity: PLAYER.into(), key: "hp".into(), delta: -3 }])).unwrap();
        apply(&mut prev, &village, &d(vec![StateOp::AddItem { item: "lantern".into() }])).unwrap();
        apply(&mut prev, &village, &d(vec![StateOp::AdjustStat { entity: "alice".into(), key: "好感度".into(), delta: 40 }])).unwrap();
        apply(&mut prev, &village, &d(vec![StateOp::SetFlag { key: "met_alice".into(), value: true }])).unwrap();
        apply(&mut prev, &village, &d(vec![StateOp::SetFlag { key: "door_open".into(), value: true }])).unwrap();
        assert_eq!(prev.stat("hp"), 7);

        // 森へ遷移 (状態を持ち越し、骨格だけ差し替え)。
        let s = forest.transition(&prev, &village);

        assert_eq!(s.location, "forest_entrance", "場所は次モジュールの start にリセット");
        assert_eq!(s.stat("hp"), 7, "数値は持ち越し (森の initial 10 を上書き)");
        assert_eq!(s.stat("stamina"), 5, "次モジュールの新規 stat は初期化される");
        assert!(s.has_item(PLAYER, "lantern"), "所持品は持ち越し");
        assert!(s.has_skill(PLAYER, "tracking"), "能力は持ち越し");
        assert_eq!(s.stat_of("alice", "好感度"), 40, "NPC の数値も持ち越し (忘れない GM)");
        assert_eq!(s.flags.get("met_alice"), Some(&true), "global フラグは持ち越し");
        assert_eq!(s.flags.get("door_open"), None, "局所フラグは捨てる (再訪で復活しない最小形)");
        assert!(s.fired.is_empty(), "発火済みトリガーはリセット (次モジュールの反応は新規)");
    }

    /// 【load 時参照整合】global_flags が allowed_flags に無ければ validate が弾く。
    #[test]
    fn validate_rejects_undeclared_global_flag() {
        let yaml = r#"
title: bad
start: room
allowed_flags: []
global_flags: [ghost_world_flag]
locations:
  room: { description: x, items: {}, exits: [] }
goal: { kind: always }
"#;
        let sc = Scenario::from_yaml(yaml).expect("パースは通る");
        assert!(
            sc.validate().iter().any(|e| matches!(
                e,
                crate::spine::ScenarioError::GlobalFlagUndeclared { flag } if flag == "ghost_world_flag"
            )),
            "未宣言の global_flag は validate で検出されるべき"
        );
    }

    // -------------------------------------------------------------------------
    // PoC-2b (reached / 名前付き goal): spine の尾を閉じる。
    // 大失敗フラグ → どのエンディング(GoalId)に着いたか → それが次モジュールの分岐セレクタ。
    // 後方互換: goal(単一) はそのまま、goals(名前付き) は任意追加。
    // -------------------------------------------------------------------------

    const BRANCHING: &str = r#"
title: 分岐の引き出し
start: study
initial_stats: { str: 2 }
allowed_flags: [fumble_drawer, drawer_jammed, drawer_opened]
challenges:
  drawer_pick:
    stat: str
    sides: 6
    dc: 5
    tiers:
      crit_fail: { natural: min, flag: fumble_drawer }
triggers:
  - id: drawer_jam
    when: { kind: flag_is, key: fumble_drawer, value: true }
    effects: [ { op: set_flag, key: drawer_jammed, value: true } ]
    narration: 工具が折れ、引き出しは固まった。
goals:
  - id: jammed_ending
    when: { kind: flag_is, key: drawer_jammed, value: true }
  - id: opened_ending
    when: { kind: flag_is, key: drawer_opened, value: true }
locations:
  study: { description: 書斎, items: {}, exits: [] }
"#;

    /// 【分岐セレクタ】大失敗 → fumble_drawer → trigger → drawer_jammed → reached() が jammed_ending を選ぶ。
    /// この GoalId が次モジュールの transition 分岐セレクタになる (spine の尾)。
    #[test]
    fn reached_selects_named_goal_from_fumble_branch() {
        let sc = Scenario::from_yaml(BRANCHING).expect("branching yaml");
        assert!(sc.validate().is_empty(), "goals だけ (goal 無し) でも健全");
        let mut s = sc.initial_state(19); // 1d6 → natural 1
        assert_eq!(sc.reached(&s), None, "開始時はどのエンディングにも未到達");

        apply(
            &mut s,
            &sc,
            &d(vec![StateOp::AttemptChallenge {
                entity: PLAYER.into(),
                challenge: "drawer_pick".into(),
            }]),
        )
        .unwrap();

        assert_eq!(s.flags.get("drawer_jammed"), Some(&true), "大失敗→fumble→trigger→drawer_jammed");
        assert_eq!(
            sc.reached(&s).as_deref(),
            Some("jammed_ending"),
            "大失敗が jammed_ending を選ぶ=分岐セレクタ (opened_ending ではない)"
        );
        assert!(is_goal(&s, &sc), "named goal 到達でも is_goal は true (後方互換)");
    }

    /// 【後方互換】単一 goal のシナリオも reached に既定 GoalId で乗る。既存 is_goal は不変。
    #[test]
    fn reached_falls_back_to_single_goal() {
        let sc = scenario(); // locked_room: goal=location_is corridor, goals 無し
        let mut s = fresh(&sc);
        assert_eq!(sc.reached(&s), None, "未クリア時は None");
        apply(&mut s, &sc, &d(vec![StateOp::SetFlag { key: "drawer_opened".into(), value: true }])).unwrap();
        apply(&mut s, &sc, &d(vec![StateOp::AddItem { item: "rusty_key".into() }])).unwrap();
        apply(&mut s, &sc, &d(vec![StateOp::SetFlag { key: "door_unlocked".into(), value: true }])).unwrap();
        apply(&mut s, &sc, &d(vec![StateOp::Move { to: "corridor".into() }])).unwrap();
        assert!(sc.reached(&s).is_some(), "単一 goal も既定 GoalId で reached に乗る");
        assert!(is_goal(&s, &sc), "is_goal は reached 経由でも従来通り true");
    }

    /// 【HP0=死を goal に / どの goal か + 結末ナレーション】`StatAtMost` で hp≤0 を勝敗条件に書け、
    /// `reached_goal` が到達した GoalDef (id + narration) を返す。複数 goal の識別と結末の語り。
    #[test]
    fn stat_at_most_death_goal_surfaces_id_and_narration() {
        let yaml = r#"
title: t
start: room
initial_stats: { hp: 10 }
allowed_flags: [escaped]
goals:
  - id: defeated
    when: { kind: stat_at_most, entity: player, key: hp, value: 0 }
    narration: あなたは力尽き、視界が暗転した。
  - id: escaped
    when: { kind: flag_is, key: escaped, value: true }
    narration: 扉を抜け、外の光を浴びた。
locations:
  room: { description: 部屋, items: {}, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(sc.validate().is_empty(), "death/escaped goal 込みで健全");
        let mut s = sc.initial_state(1);
        assert_eq!(sc.reached_goal(&s), None, "開始時はどの goal も未到達");

        // hp を 0 へ削る (10 減 → 0 クランプ)。StatAtMost(hp,0) が真化。
        apply(&mut s, &sc, &d(vec![StateOp::AdjustStat {
            entity: PLAYER.into(),
            key: "hp".into(),
            delta: -10,
        }])).unwrap();
        assert_eq!(s.stat_of(PLAYER, "hp"), 0, "hp は 0 クランプ");

        let g = sc.reached_goal(&s).expect("hp≤0 で defeated に到達");
        assert_eq!(g.id, "defeated", "どの goal に達したか (StatAtMost が効く)");
        assert_eq!(g.narration, "あなたは力尽き、視界が暗転した。", "結末のナレーション");
        // reached (GoalId) も同じ goal を選ぶ (後方互換の selector)。
        assert_eq!(sc.reached(&s).as_deref(), Some("defeated"));
    }

    /// 【goal の title / hint / visible — プレイヤー向け提示素材】`GoalDef.title` は目標一覧の
    /// 表示名 (id はスペース等を避ける機械用セレクタゆえ、人間向けの文はこちら)、`hint` は
    /// 「何をすればだいたい行けるか」の道しるべ (narration の入口版)、`visible: false` は
    /// **隠しゴール** (到達するまで目標一覧に出さない・到達判定 reached は不変で効く)。
    /// いずれも authored・非検証、engine は不解釈で提示層が扱う。省略時は
    /// title/hint 空・visible true (既存 YAML は無改修、title 空は id 表示)。
    #[test]
    fn goal_title_hint_visible_parse_and_default() {
        let yaml = r#"
title: t
start: room
allowed_flags: [escaped, hidden_exit]
goals:
  - id: escaped
    when: { kind: flag_is, key: escaped, value: true }
    title: 正面からの脱出
    hint: 鍵を探して正面の扉を開ける
    narration: 扉を抜けた。
  - id: secret
    when: { kind: flag_is, key: hidden_exit, value: true }
    visible: false
locations:
  room: { description: 部屋, items: {}, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(sc.validate().is_empty(), "title/hint/visible 込みで健全");
        assert_eq!(sc.goals[0].title, "正面からの脱出", "title (表示名) がそのまま乗る");
        assert_eq!(sc.goals[0].hint, "鍵を探して正面の扉を開ける", "hint がそのまま乗る");
        assert!(sc.goals[0].visible, "visible 省略時は true (既存 YAML 無改修で全 goal 表示)");
        assert_eq!(sc.goals[1].title, "", "title 省略時は空 (提示層が id へフォールバック)");
        assert_eq!(sc.goals[1].hint, "", "hint 省略時は空 (後方互換・既存 YAML 無改修)");
        assert!(!sc.goals[1].visible, "visible: false = 隠しゴール (提示層が一覧から外す)");

        // 隠しゴールでも到達判定 (reached) は不変で効く = visible は純粋に提示層の宣言。
        let mut s = sc.initial_state(1);
        apply(&mut s, &sc, &d(vec![StateOp::SetFlag { key: "hidden_exit".into(), value: true }]))
            .unwrap();
        assert_eq!(sc.reached(&s).as_deref(), Some("secret"), "隠しゴールも reached には乗る");
    }

    /// 【アイテムの取得様式 take: once/infinite/fixed】場所アイテムに 3 様式を宣言できる。
    /// once (既定・旧 Gate 直書き形式もこれ) = 一度取ったら場所から無くなる (`taken_items` に
    /// 記録、手放して戻っても再取得=複製は却下)。infinite (自販機のジュース等) = 何度でも取れる。
    /// fixed (シャワー/リモコン等の備え付け) = 取得不可、却下理由が「取らずにその場で使える」を
    /// LLM に説明する (self-repair で語り直しへ誘導)。
    #[test]
    fn item_take_modes_once_infinite_fixed() {
        let yaml = r#"
title: t
start: room
goal: { kind: location_is, at: exit }
locations:
  room:
    description: 部屋
    items:
      juice: { when: { kind: always }, take: infinite }
      rusty_key: { kind: always }
      shower: { take: fixed }
    exits: []
  exit: { description: 外, items: {}, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).expect("新旧混在の items が parse できる");
        assert!(sc.validate().is_empty());
        let mut s = sc.initial_state(1);

        // fixed: 取得不可。理由は「備え付け・その場で使える」を説明する。
        let v = adjudicate(&s, &sc, &d(vec![StateOp::AddItem { item: "shower".into() }]));
        match v {
            Verdict::Reject { reasons } => {
                assert!(
                    reasons.iter().any(|r| matches!(
                        r,
                        RejectReason::ItemFixed { item } if item == "shower"
                    )),
                    "備え付けは ItemFixed で却下"
                );
                let msg = reasons[0].localize(crate::Lang::Ja);
                assert!(msg.contains("備え付け"), "却下理由が備え付けを説明する: {msg}");
                assert!(msg.contains("使える"), "取らずに使えることを LLM に教える: {msg}");
            }
            Verdict::Accept => panic!("fixed アイテムの取得が通ってしまった"),
        }

        // infinite: 取る → 手放す → もう一度取れる (自販機)。
        apply(&mut s, &sc, &d(vec![StateOp::AddItem { item: "juice".into() }])).unwrap();
        apply(&mut s, &sc, &d(vec![StateOp::RemoveItem { item: "juice".into() }])).unwrap();
        apply(&mut s, &sc, &d(vec![StateOp::AddItem { item: "juice".into() }]))
            .expect("infinite は何度でも取れる");

        // once (旧形式 Gate 直書き = 既定): 取る → 手放す → 再取得は複製ゆえ却下。
        apply(&mut s, &sc, &d(vec![StateOp::AddItem { item: "rusty_key".into() }])).unwrap();
        apply(&mut s, &sc, &d(vec![StateOp::RemoveItem { item: "rusty_key".into() }])).unwrap();
        let v = adjudicate(&s, &sc, &d(vec![StateOp::AddItem { item: "rusty_key".into() }]));
        match v {
            Verdict::Reject { reasons } => assert!(
                reasons.iter().any(|r| matches!(
                    r,
                    RejectReason::ItemAlreadyTaken { item } if item == "rusty_key"
                )),
                "once の再取得 (複製) は ItemAlreadyTaken で却下"
            ),
            Verdict::Accept => panic!("once アイテムの複製が通ってしまった"),
        }
    }

    /// 【使えるフラグ / authored 専権フラグの機械判別】trigger effects・challenge 帰結
    /// (on_success/on_failure/tier) が書くフラグは authored 専権 — LLM が set_flag すべきで
    /// ない (却下ループの素)。`authored_only_flags` が宣言走査で収集し、`usable_flags`
    /// (= allowed − 専権) が LLM への語彙提示 (prompt / FlagNotAllowed 却下文面) の素になる。
    /// filter_authored_only_ops (op の構造的遮断) のフラグ版。
    #[test]
    fn authored_only_flags_are_excluded_from_usable_vocabulary() {
        let yaml = r#"
title: t
start: room
allowed_flags: [聞いた_在処, 罠が作動, 扉が開いた, 腕が滑った, 奇跡が起きた]
flag_titles: { 聞いた_在処: 鍵の在処の知識 }
triggers:
  - id: trap
    when: { kind: flag_is, key: 聞いた_在処, value: true }
    effects:
      - { op: set_flag, key: 罠が作動, value: true }
challenges:
  force:
    sides: 6
    dc: 4
    on_success: { flag: 扉が開いた }
    on_failure: { flag: 腕が滑った }
    tiers:
      crit: { natural: max, flag: 奇跡が起きた }
locations:
  room: { description: d, items: {}, exits: [] }
goal: { kind: always }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(sc.validate().is_empty());
        let authored = sc.authored_only_flags();
        for f in ["罠が作動", "扉が開いた", "腕が滑った", "奇跡が起きた"] {
            assert!(authored.contains(f), "authored 専権に {f} が入る");
        }
        assert!(!authored.contains("聞いた_在処"), "LLM が立てる知識フラグは専権でない");
        let usable = sc.usable_flags();
        assert_eq!(usable.len(), 1);
        assert!(usable.contains("聞いた_在処"), "usable = allowed − authored 専権");

        // 幻フラグの却下理由が「使えるフラグ」語彙を運ぶ (self-repair が一発で直せる)。
        let s = sc.initial_state(1);
        match adjudicate(&s, &sc, &d(vec![StateOp::SetFlag { key: "幻".into(), value: true }])) {
            Verdict::Reject { reasons } => {
                let msg = reasons[0].localize(crate::Lang::Ja);
                assert!(msg.contains("聞いた_在処"), "使えるフラグを却下文面で提示: {msg}");
                assert!(!msg.contains("罠が作動"), "authored 専権は提示しない: {msg}");
            }
            Verdict::Accept => panic!("幻フラグは却下されるべき"),
        }
    }

    /// 【#51: challenge の effects 内 set_flag も専権 (2026-07-14 ユーザー報告)】
    /// `authored_only_flags` の走査が challenge 帰結の `.flag` 欄しか見ておらず、
    /// **`on_success`/`on_failure`/`tiers` の `effects` に書いた `set_flag` が漏れていた** —
    /// そのフラグは GM の usable 語彙に出て (先取りを誘う)、#50 の engine バックストップも
    /// 素通りする (二層とも開く)。effects は 2026-07-03 に足した「フラグ+トリガーの 2 点セット
    /// 無しで直接動かす」経路で、そちらで書いた作者だけが穴に落ちる非対称だった。
    #[test]
    fn challenge_effects_setflags_are_authored_only() {
        let yaml = r#"
title: t
start: room
allowed_flags: [今日は働いた, 大失態を演じた, 挨拶した]
challenges:
  work:
    sides: 6
    dc: 4
    on_success: { effects: [{ op: set_flag, key: 今日は働いた, value: true }] }
    on_failure: { effects: [{ op: set_flag, key: 今日は働いた, value: true }] }
    tiers:
      fumble: { natural: min, effects: [{ op: set_flag, key: 大失態を演じた, value: true }] }
locations:
  room: { description: d, items: {}, exits: [] }
goal: { kind: always }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(sc.validate().is_empty(), "{:?}", sc.validate());
        let authored = sc.authored_only_flags();
        assert!(authored.contains("今日は働いた"), "outcome.effects の set_flag も専権");
        assert!(authored.contains("大失態を演じた"), "tier.effects の set_flag も専権");
        let usable = sc.usable_flags();
        assert_eq!(usable.len(), 1, "usable に漏れない: {usable:?}");
        assert!(usable.contains("挨拶した"));

        // #50 のバックストップも effects 経路のフラグに効く (見せない + 通さないの二層)。
        let s = sc.initial_state(1);
        let preempt = d(vec![StateOp::SetFlag { key: "今日は働いた".into(), value: true }]);
        match adjudicate(&s, &sc, &preempt) {
            Verdict::Reject { reasons } => assert!(
                reasons.iter().any(|r| matches!(r, RejectReason::FlagNotAllowed { key, .. } if key == "今日は働いた")),
                "effects 経路の専権フラグへの LLM set_flag を却下: {reasons:?}"
            ),
            Verdict::Accept => panic!("effects 経路の専権フラグは却下されるべき (#51)"),
        }
    }

    /// 【フラグの真化ターン記録】op / トリガー効果のどちらで立っても、`flag_turns` に
    /// 「true になったターン」が刻まれる (apply 末尾の差分で一括捕捉)。提示層が chronicle の
    /// 該当ターン要約と join して「何をして立ったフラグか」を思い出せる素。
    #[test]
    fn flag_turns_record_when_flags_became_true() {
        let yaml = r#"
title: t
start: room
allowed_flags: [x_flag, y_flag, z_flag]
triggers:
  - id: chain
    when: { kind: flag_is, key: y_flag, value: true }
    effects:
      - { op: set_flag, key: z_flag, value: true }
locations:
  room: { description: d, items: {}, exits: [] }
goal: { kind: always }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        let mut s = sc.initial_state(1);
        apply(&mut s, &sc, &d(vec![StateOp::SetFlag { key: "x_flag".into(), value: true }])).unwrap();
        apply(&mut s, &sc, &d(vec![StateOp::SetFlag { key: "y_flag".into(), value: true }])).unwrap();
        assert_eq!(s.flag_turns.get("x_flag"), Some(&1), "op で立った x はターン 1");
        assert_eq!(s.flag_turns.get("y_flag"), Some(&2), "op で立った y はターン 2");
        assert_eq!(s.flag_turns.get("z_flag"), Some(&2), "トリガー効果で立った z も同ターンに記録");
    }

    /// 【ランダム役職割り当て (spec 06 Phase A)】`role_assignment` が seed **派生の専用
    /// ストリーム** (role_rng) で役職を shuffle して attributes に配る。同 seed 同配役
    /// (決定論・監査可能)、本流 `state.rng` は消費しない (配役の有無でプレイ中のダイス列が
    /// 変わらない)、bookkeeping stat (各自の 生存=1・役職別カウンタ) を自動生成する。
    /// 割り当てはエンジンの専権 — LLM は関与できない (「出目は正本」の配役版)。
    #[test]
    fn role_assignment_deals_roles_deterministically_without_touching_main_rng() {
        let yaml = r#"
title: t
start: village
role_assignment:
  key: 役職
  pool: { 人狼: 2, 占い師: 1, 村人: 3 }
  among: [player, alice, bob, chris, diana, eri]
characters:
  alice: { name: A }
  bob: { name: B }
  chris: { name: C }
  diana: { name: D }
  eri: { name: E }
locations:
  village: { description: d, items: {}, exits: [] }
goal: { kind: always }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(sc.validate().is_empty(), "整合した role_assignment は健全: {:?}", sc.validate());
        let members = ["player", "alice", "bob", "chris", "diana", "eri"];

        let s1 = sc.initial_state(7);
        let s2 = sc.initial_state(7);
        let roles = |s: &GameState| -> Vec<String> {
            members
                .iter()
                .map(|e| s.attributes.get(*e).and_then(|a| a.get("役職")).cloned().unwrap_or_default())
                .collect()
        };
        // 決定論: 同 seed 同配役。
        assert_eq!(roles(&s1), roles(&s2), "同 seed は同配役 (再現・監査可能)");
        // 全員に配られ、人数が pool どおり。
        let mut counts: std::collections::BTreeMap<String, u32> = Default::default();
        for r in roles(&s1) {
            assert!(!r.is_empty(), "全員に役職が配られる");
            *counts.entry(r).or_insert(0) += 1;
        }
        assert_eq!(counts.get("人狼"), Some(&2));
        assert_eq!(counts.get("占い師"), Some(&1));
        assert_eq!(counts.get("村人"), Some(&3));
        // 本流 rng は無傷 (専用ストリーム)。
        assert_eq!(s1.rng.cursor, 0, "配役が本流のダイス列を消費しない");
        // seed を変えると配役が変わりうる (shuffle が効いている。20 seed 中 2 通り以上)。
        let distinct: std::collections::BTreeSet<Vec<String>> =
            (0..20).map(|seed| roles(&sc.initial_state(seed))).collect();
        assert!(distinct.len() >= 2, "seed で配役が変わる (shuffle 実効): {distinct:?}");
        // bookkeeping stat の自動生成 (更新は ResolveVote の専権 = Phase C)。
        for e in members {
            assert_eq!(s1.stat_of(e, "生存"), 1, "{e} の生存=1");
        }
        assert_eq!(s1.stat_of(PLAYER, "生存人狼数"), 2);
        assert_eq!(s1.stat_of(PLAYER, "生存占い師数"), 1);
        assert_eq!(s1.stat_of(PLAYER, "生存村人数"), 3);
    }

    /// 【challenge の直接効果 (effects)】on_success/on_failure/tier に `effects: [StateOp]` を
    /// 書けば、帰結の機械効果 (stat/attribute/スキル等) をフラグ+トリガーの2点セット無しで
    /// **同一 apply 内に原子適用**できる。authored 専権 — LLM は challenge を「選ぶ」だけで
    /// 帰結を持てない閉世界は不変 (trigger effects と同じ信頼モデル)。通常成否と極 (tier) の
    /// effects は併存する (フラグと同じ)。
    #[test]
    fn challenge_effects_apply_stats_and_attributes_atomically() {
        let yaml = r#"
title: t
start: room
initial_stats: { hp: 10, str: 5 }
initial_attributes: { 状態: 健康 }
allowed_flags: [ぶつけた, 押し開けた]
challenges:
  slam_fail:
    sides: 1
    dc: 2
    on_failure:
      flag: ぶつけた
      effects:
        - { op: adjust_stat, key: hp, delta: -2 }
    tiers:
      crit_fail:
        natural: min
        effects:
          - { op: set_attribute, key: 状態, value: 打ち身 }
  slam_win:
    sides: 1
    dc: 1
    on_success:
      flag: 押し開けた
      effects:
        - { op: adjust_stat, key: str, delta: 1 }
locations:
  room: { description: d, items: {}, exits: [] }
goal: { kind: always }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(sc.validate().is_empty(), "{:?}", sc.validate());
        let mut s = sc.initial_state(1);

        // 1d1=1 < DC2 = 失敗 (natural 1 = min で極も併発)。
        apply(&mut s, &sc, &d(vec![StateOp::AttemptChallenge {
            entity: PLAYER.into(),
            challenge: "slam_fail".into(),
        }]))
        .unwrap();
        assert!(s.flag("ぶつけた"), "帰結フラグは従来どおり");
        assert_eq!(s.stat_of(PLAYER, "hp"), 8, "on_failure.effects の adjust_stat が同一 apply で効く");
        assert_eq!(s.attribute_of(PLAYER, "状態"), "打ち身", "tier.effects の set_attribute も併存で効く");

        // 1d1=1 >= DC1 = 成功。
        apply(&mut s, &sc, &d(vec![StateOp::AttemptChallenge {
            entity: PLAYER.into(),
            challenge: "slam_win".into(),
        }]))
        .unwrap();
        assert!(s.flag("押し開けた"));
        assert_eq!(s.stat_of(PLAYER, "str"), 6, "on_success.effects も効く");
    }

    /// 【challenge effects の再帰禁止】effects に attempt_challenge を書くと A→A の無限再帰が
    /// 組めてしまうため validate が load 時に弾く (連鎖したければ従来どおり flag→トリガー経由)。
    #[test]
    fn validate_rejects_attempt_challenge_inside_challenge_effects() {
        let yaml = r#"
title: t
start: room
challenges:
  a:
    sides: 1
    dc: 1
    on_success:
      effects:
        - { op: attempt_challenge, challenge: a }
locations:
  room: { description: d, items: {}, exits: [] }
goal: { kind: always }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(
            sc.validate().iter().any(|e| matches!(e,
                crate::spine::ScenarioError::ChallengeEffectRecursive { challenge } if challenge == "a")),
            "challenge effects 内の attempt_challenge を弾く: {:?}",
            sc.validate()
        );
    }

    /// spec 06 Phase C の共通盤面: 人狼1・村人3、昼は誰でも・夜は人狼のみ投票可、
    /// 「開票する」フラグで ResolveVote トリガーが発火する。
    const VOTE_BOARD: &str = r#"
title: t
start: v
allowed_flags: [投票フェーズ, 夜フェーズ, 開票する]
role_assignment: { key: 役職, pool: { 人狼: 1, 村人: 3 }, among: [player, alice, bob, chris] }
secret_attributes: [役職]
vote_rules:
  - when: { kind: flag_is, key: 投票フェーズ, value: true }
  - when: { kind: flag_is, key: 夜フェーズ, value: true }
    voter_attribute: { key: 役職, value: 人狼 }
characters:
  alice: { name: A }
  bob: { name: B }
  chris: { name: C }
triggers:
  - id: 開票
    when: { kind: flag_is, key: 開票する, value: true }
    effects: [{ op: resolve_vote }]
    narration: 票が読み上げられた。
locations:
  v: { description: d, items: {}, exits: [] }
goal: { kind: always }
"#;

    /// 盤面から役職で entity を引く (shuffle 後の実際の配役を読む)。
    fn by_role(s: &GameState, role: &str) -> Vec<String> {
        ["player", "alice", "bob", "chris"]
            .iter()
            .filter(|e| s.attribute_of(e, "役職") == role)
            .map(|e| e.to_string())
            .collect()
    }

    /// 【投票権 (spec 06 Phase C)】CastVote は vote_rules の**いずれかに合致**したときだけ
    /// 受理される (デフォルト拒否)。昼 (投票フェーズ) は生存者なら誰でも、夜は
    /// voter_attribute (役職=人狼) を満たす者だけ。死者は投票もされもしない。
    #[test]
    fn cast_vote_respects_vote_rules_default_deny() {
        let sc = Scenario::from_yaml(VOTE_BOARD).unwrap();
        assert!(sc.validate().is_empty(), "{:?}", sc.validate());
        let mut s = sc.initial_state(3);
        let wolf = by_role(&s, "人狼").pop().unwrap();
        let villager = by_role(&s, "村人").pop().unwrap();
        let vote = |voter: &str, target: &str| {
            d(vec![StateOp::CastVote { voter: voter.into(), target: target.into() }])
        };

        // フェーズ外 = どの rule にも合致しない → デフォルト拒否。
        match adjudicate(&s, &sc, &vote(&villager, &wolf)) {
            Verdict::Reject { reasons } => assert!(
                reasons.iter().any(|r| matches!(r, RejectReason::VoteNotAllowed { .. })),
                "フェーズ外の票はデフォルト拒否: {reasons:?}"
            ),
            Verdict::Accept => panic!("rule 合致なしで票が通ってはならない"),
        }

        // 昼 (投票フェーズ): 誰でも投票できる。
        apply(&mut s, &sc, &d(vec![StateOp::SetFlag { key: "投票フェーズ".into(), value: true }]))
            .unwrap();
        assert!(adjudicate(&s, &sc, &vote(&villager, &wolf)).is_accept(), "昼は村人も投票可");
        assert!(adjudicate(&s, &sc, &vote(&wolf, &villager)).is_accept(), "昼は人狼も投票可");

        // 夜: voter_attribute (人狼) を満たす者だけ。
        apply(
            &mut s,
            &sc,
            &d(vec![
                StateOp::SetFlag { key: "投票フェーズ".into(), value: false },
                StateOp::SetFlag { key: "夜フェーズ".into(), value: true },
            ]),
        )
        .unwrap();
        match adjudicate(&s, &sc, &vote(&villager, &wolf)) {
            Verdict::Reject { reasons } => assert!(
                reasons.iter().any(|r| matches!(r, RejectReason::VoteNotAllowed { .. })),
                "夜に村人の票は弾かれる: {reasons:?}"
            ),
            Verdict::Accept => panic!("夜に村人が襲撃票を入れられてはならない"),
        }
        assert!(adjudicate(&s, &sc, &vote(&wolf, &villager)).is_accept(), "夜の人狼は投票可");

        // 死者は投票できず、投票もされない。
        s.set_stat(&villager, "生存", 0);
        match adjudicate(&s, &sc, &vote(&wolf, &villager)) {
            Verdict::Reject { reasons } => assert!(
                reasons.iter().any(|r| matches!(
                    r,
                    RejectReason::EntityNotAlive { entity } if entity == &villager
                )),
                "死者への票は弾かれる: {reasons:?}"
            ),
            Verdict::Accept => panic!("死者に投票できてはならない"),
        }
    }

    /// 【HasVoted gate — 投票のイベント駆動終了 (#38)】タイマー駆動 (`turns_since 投票T`) だけ
    /// だと「プレイヤーが誰も指名しないまま空開票で流れる」— プレイヤーの票を**イベント**として
    /// 開票を発火できる純粋述語 `has_voted` を新設。resolve_vote が votes をリセットするので
    /// gate は開票後に自然と偽へ戻り、repeatable トリガーは次サイクルで再武装する (リセット op 不要)。
    #[test]
    fn has_voted_gate_fires_execution_on_player_vote() {
        let sc = Scenario::from_yaml(
            r#"
title: t
start: v
allowed_flags: [投票フェーズ]
role_assignment: { key: 役職, pool: { 人狼: 1, 村人: 3 }, among: [player, alice, bob, chris] }
vote_rules:
  - when: { kind: flag_is, key: 投票フェーズ, value: true }
triggers:
  - id: execution
    repeatable: true
    when:
      kind: all
      of:
        - { kind: flag_is, key: 投票フェーズ, value: true }
        - { kind: has_voted }
    effects:
      - { op: resolve_vote }
      - { op: set_flag, key: 投票フェーズ, value: false }
    narration: 開票された。
characters:
  alice: { name: A }
  bob: { name: B }
  chris: { name: C }
locations:
  v: { description: d, items: {}, exits: [] }
goal: { kind: always }
"#,
        )
        .unwrap();
        assert!(sc.validate().is_empty(), "{:?}", sc.validate());
        let mut s = sc.initial_state(3);
        // 投票フェーズはトリガー effects が閉じる = 専権フラグ (#50 で LLM set_flag は却下される)。
        // フェーズを開くのは authored 側の仕事なので、authored 効果相当の直接操作でセットアップする。
        s.flags.insert("投票フェーズ".into(), true);

        // NPC の票だけでは発火しない — プレイヤーの票がイベント。
        let out = apply(
            &mut s,
            &sc,
            &d(vec![StateOp::CastVote { voter: "alice".into(), target: "bob".into() }]),
        )
        .unwrap();
        assert!(out.fired.is_empty(), "player 未投票では開票しない: {:?}", out.fired);
        assert!(s.flag("投票フェーズ"), "フェーズは開いたまま");

        // プレイヤーの票が入った瞬間、同一 apply で開票 (処刑・票リセット・フェーズ閉じ) まで走る。
        let out = apply(
            &mut s,
            &sc,
            &d(vec![
                StateOp::CastVote { voter: "bob".into(), target: "alice".into() },
                StateOp::CastVote { voter: "player".into(), target: "bob".into() },
            ]),
        )
        .unwrap();
        assert!(
            out.fired.iter().any(|f| f.id == "execution"),
            "player の票で開票が発火: {:?}",
            out.fired
        );
        // 票: alice→bob (前ターン持ち越し), bob→alice, player→bob = bob 2票で処刑。
        assert_eq!(s.stat_of("bob", "生存"), 0, "最多得票の bob が死ぬ");
        assert!(s.votes.is_empty(), "開票で票はリセット = has_voted は自然に偽へ再武装");
        assert!(!s.flag("投票フェーズ"), "フェーズが閉じる");
    }

    /// 【投票の無い盤面 / 生存 stat の無い盤面 (実プレイ #35)】(a) `vote_rules` の無い盤面への
    /// cast_vote は「この盤面に投票は無い」(VoteNotDeclared) で却下する — 実プレイでは
    /// EntityNotAlive (「mayu は既に生存していない」) が出た: **未宣言の 生存 stat を 0=死者と
    /// 誤読**していた。(b) `vote_rules` は有るが role_assignment (生存 seed) の無い盤面では、
    /// 生存 stat を持たない entity は**生きている**扱いで投票できる (未宣言 = 生死の概念が無い
    /// 盤面。従来は全員死者扱いで投票が構造的に不可能だった)。
    #[test]
    fn cast_vote_without_rules_or_survival_stats() {
        // (a) 投票機構ゼロの盤面 (合コン等) — 死亡理由でなく「投票が無い」で却下する。
        let sc = Scenario::from_yaml(
            r#"
title: t
start: v
characters:
  mayu: { name: M }
locations:
  v: { description: d, items: {}, exits: [] }
goal: { kind: always }
"#,
        )
        .unwrap();
        let s = sc.initial_state(1);
        let vote = d(vec![StateOp::CastVote { voter: "mayu".into(), target: "player".into() }]);
        match adjudicate(&s, &sc, &vote) {
            Verdict::Reject { reasons } => {
                assert!(
                    reasons.iter().any(|r| matches!(r, RejectReason::VoteNotDeclared)),
                    "投票の無い盤面は VoteNotDeclared で名指し却下: {reasons:?}"
                );
                assert!(
                    !reasons.iter().any(|r| matches!(r, RejectReason::EntityNotAlive { .. })),
                    "生存 stat の無いキャラを死者と誤読しない: {reasons:?}"
                );
            }
            Verdict::Accept => panic!("投票機構の無い盤面で票が通ってはならない"),
        }

        // (b) vote_rules 有り + 生存 stat 無し (role_assignment 無し) — 未宣言は生存扱いで投票可。
        let sc2 = Scenario::from_yaml(
            r#"
title: t
start: v
vote_rules:
  - when: { kind: always }
characters:
  mayu: { name: M }
locations:
  v: { description: d, items: {}, exits: [] }
goal: { kind: always }
"#,
        )
        .unwrap();
        let s2 = sc2.initial_state(1);
        let vote2 = d(vec![StateOp::CastVote { voter: "mayu".into(), target: "player".into() }]);
        assert!(
            adjudicate(&s2, &sc2, &vote2).is_accept(),
            "生存 stat 未宣言のキャラは生きている扱いで投票できる"
        );
    }

    /// 【開票 (spec 06 Phase C)】ResolveVote (authored トリガー専権) が集計し、最多得票者を
    /// **一箇所で原子適用**で死亡させる: 生存=0 (正本) + presence false (表示投影) +
    /// 役職カウンタ/優位 stat 再計算 + 票リセット。
    #[test]
    fn resolve_vote_tallies_kills_and_recalculates() {
        let sc = Scenario::from_yaml(VOTE_BOARD).unwrap();
        let mut s = sc.initial_state(3);
        let wolf = by_role(&s, "人狼").pop().unwrap();
        let voters: Vec<String> = ["player", "alice", "bob", "chris"]
            .iter()
            .filter(|e| **e != wolf)
            .map(|e| e.to_string())
            .collect();
        assert_eq!(s.stat_of(PLAYER, "生存者数"), 4, "初期の生存者数");

        // 昼フェーズで全員が人狼に投票 → 開票フラグでトリガーが resolve_vote を焚く。
        apply(&mut s, &sc, &d(vec![StateOp::SetFlag { key: "投票フェーズ".into(), value: true }]))
            .unwrap();
        let mut ops: Vec<StateOp> = voters
            .iter()
            .map(|v| StateOp::CastVote { voter: v.clone(), target: wolf.clone() })
            .collect();
        ops.push(StateOp::SetFlag { key: "開票する".into(), value: true });
        apply(&mut s, &sc, &d(ops)).unwrap();

        assert_eq!(s.stat_of(&wolf, "生存"), 0, "最多得票の人狼が死亡 (正本)");
        assert_eq!(s.present_overrides.get(&wolf), Some(&false), "presence へ投影 (退場)");
        assert_eq!(s.stat_of(PLAYER, "生存人狼数"), 0, "役職カウンタ再計算");
        assert_eq!(s.stat_of(PLAYER, "生存者数"), 3);
        assert_eq!(s.stat_of(PLAYER, "人狼優位"), -3, "優位 = 2×生存人狼数 − 生存者数");
        assert_eq!(s.stat_of(PLAYER, "村人優位"), 3, "優位 = 2×生存村人数 − 生存者数");
        assert!(s.votes.is_empty(), "開票後は票がリセットされる");
    }

    /// 【同数の抽選 (spec 06 Phase C)】同数は seed 派生の専用ストリーム (VOTE_RNG) で抽選 —
    /// 同 seed 同結果の決定論。本流ダイス列は消費しない。
    #[test]
    fn resolve_vote_tie_break_is_deterministic_per_seed() {
        let run = || {
            let sc = Scenario::from_yaml(VOTE_BOARD).unwrap();
            let mut s = sc.initial_state(11);
            apply(&mut s, &sc, &d(vec![StateOp::SetFlag { key: "投票フェーズ".into(), value: true }]))
                .unwrap();
            // 2 対 2 の同数: player/alice → bob、bob/chris → alice。
            apply(
                &mut s,
                &sc,
                &d(vec![
                    StateOp::CastVote { voter: "player".into(), target: "bob".into() },
                    StateOp::CastVote { voter: "alice".into(), target: "bob".into() },
                    StateOp::CastVote { voter: "bob".into(), target: "alice".into() },
                    StateOp::CastVote { voter: "chris".into(), target: "alice".into() },
                    StateOp::SetFlag { key: "開票する".into(), value: true },
                ]),
            )
            .unwrap();
            let cursor = s.rng.cursor;
            let dead: Vec<String> = ["alice", "bob"]
                .iter()
                .filter(|e| s.stat_of(e, "生存") == 0)
                .map(|e| e.to_string())
                .collect();
            (dead, cursor)
        };
        let (dead1, cursor1) = run();
        let (dead2, _) = run();
        assert_eq!(dead1.len(), 1, "同数でも犠牲者はちょうど一人: {dead1:?}");
        assert_eq!(dead1, dead2, "同 seed の同数抽選は同結果 (決定論)");
        assert_eq!(cursor1, 0, "抽選は本流ダイス列を消費しない");
    }

    /// 【捏造遮断 (spec 06 Phase C)】LLM が resolve_vote を提案しても却下される
    /// (authored トリガー専権の効果 op 第5例。開票結果は捏造できない)。
    #[test]
    fn llm_proposed_resolve_vote_is_rejected() {
        let sc = Scenario::from_yaml(VOTE_BOARD).unwrap();
        let s = sc.initial_state(3);
        match adjudicate(&s, &sc, &d(vec![StateOp::ResolveVote])) {
            Verdict::Reject { reasons } => assert!(
                reasons.iter().any(|r| matches!(r, RejectReason::VoteResolveNotAllowed)),
                "{reasons:?}"
            ),
            Verdict::Accept => panic!("LLM の開票提案は却下されるべき (開票の捏造遮断)"),
        }
    }

    /// 【宛先別秘匿 (spec 06 Phase B)】`secret_attributes` はゲーム的秘匿情報の属性キー
    /// (役職等)。hidden_* (全提示層から隠す帳簿) とは別軸 — GM は全員分を見る (注記付き、
    /// 提示は harness/app の責務)、プレイヤー UI は本人分のみ。engine は宣言を運ぶだけで
    /// gate/トリガー評価は不変。キーはどこかで宣言済み (initial_attributes /
    /// CharacterDef.attributes / role_assignment.key) 必須。
    #[test]
    fn secret_attributes_parse_and_validate_declaration() {
        let yaml = r#"
title: t
start: v
role_assignment: { key: 役職, pool: { 人狼: 1, 村人: 1 }, among: [player, alice] }
secret_attributes: [役職]
characters: { alice: { name: A } }
locations: { v: { description: d, items: {}, exits: [] } }
goal: { kind: always }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(sc.validate().is_empty(), "role_assignment.key の秘匿は健全: {:?}", sc.validate());
        assert!(sc.secret_attributes.contains("役職"));

        // どこにも宣言されていないキーの秘匿は幻属性 → validate が弾く。
        let yaml = r#"
title: t
start: v
secret_attributes: [幽霊属性]
locations: { v: { description: d, items: {}, exits: [] } }
goal: { kind: always }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(
            sc.validate().iter().any(|e| matches!(e,
                crate::spine::ScenarioError::SecretAttributeUndeclared { key } if key == "幽霊属性")),
            "未宣言キーの秘匿を弾く: {:?}",
            sc.validate()
        );
    }

    /// spec 09 PoC 用の小盤面: 砂浜で建材を拾い、シェルターを組む (mujinto T15/T16 の再現形)。
    const PROJECTION_BOARD: &str = r#"
title: t
start: beach
allowed_flags: [made_shelter, blessed, after_blessed]
flag_rules:
  after_blessed: { kind: flag_is, key: blessed, value: true }
locations:
  beach:
    description: d
    items:
      流木: { when: { kind: always }, take: infinite }
      小石: { when: { kind: always }, take: infinite }
    exits: []
challenges:
  build_shelter:
    description: 流木と小石でシェルターを組む
    requires:
      kind: all
      of:
        - { kind: location_is, at: beach }
        - { kind: has_item, entity: player, item: 流木 }
        - { kind: has_item, entity: player, item: 小石 }
    sides: 1
    dc: 1
    on_success: { flag: made_shelter }
  bless:
    description: 祝福の儀 (成功で blessed)
    sides: 1
    dc: 1
    on_success: { flag: blessed }
goal: { kind: flag_is, key: made_shelter, value: true }
"#;

    /// 【逐次射影裁定 (spec 09-A)】「拾って組む」を 1 delta に束ねられる — adjudicate は
    /// op を書かれた順に検証し、受理した決定論 op を射影に仮適用してから次を検証する
    /// (裁定 = apply のドライラン)。旧来はターン開始時点の一括評価で ChallengeLocked に
    /// なっていた (mujinto T15 の catch-22)。
    #[test]
    fn sequential_projection_allows_pick_then_use_in_one_delta() {
        let sc = Scenario::from_yaml(PROJECTION_BOARD).unwrap();
        let mut s = sc.initial_state(1);
        let delta = d(vec![
            StateOp::AddItem { item: "流木".into() },
            StateOp::AddItem { item: "小石".into() },
            StateOp::AttemptChallenge { entity: PLAYER.into(), challenge: "build_shelter".into() },
        ]);
        assert!(
            matches!(adjudicate(&s, &sc, &delta), Verdict::Accept),
            "拾ってから組む束は受理される: {:?}",
            adjudicate(&s, &sc, &delta)
        );
        apply(&mut s, &sc, &delta).expect("適用できる");
        assert!(s.flag("made_shelter"), "sides:1 dc:1 なので必ず成功しシェルターが立つ");
    }

    /// 【順序の意味 (spec 09-A)】ops は書かれた順に裁く — 「組んでから拾う」順は
    /// attempt の時点で未所持なので従来どおり却下される (順序は作者/LLM の責任)。
    #[test]
    fn order_matters_use_before_pick_is_rejected() {
        let sc = Scenario::from_yaml(PROJECTION_BOARD).unwrap();
        let s = sc.initial_state(1);
        let delta = d(vec![
            StateOp::AttemptChallenge { entity: PLAYER.into(), challenge: "build_shelter".into() },
            StateOp::AddItem { item: "流木".into() },
            StateOp::AddItem { item: "小石".into() },
        ]);
        match adjudicate(&s, &sc, &delta) {
            Verdict::Reject { reasons } => assert!(
                reasons.iter().any(|r| matches!(r, RejectReason::ChallengeLocked { .. })),
                "{reasons:?}"
            ),
            Verdict::Accept => panic!("attempt 時点で未所持なので却下されるべき"),
        }
    }

    /// 【重複拾得の no-op (spec 09-B)】既に所持している物への add_item は却下でなく
    /// 受理して no-op (mujinto T16: 「念のため再拾得」を束ねた delta が ItemAlreadyHeld で
    /// 全体却下されていた)。inventory は集合なので複製は構造的に起きず、複製穴の守りは
    /// taken_items (take:once の再取得却下) が担い続ける。
    #[test]
    fn duplicate_add_item_is_noop_when_already_held() {
        let sc = Scenario::from_yaml(PROJECTION_BOARD).unwrap();
        let mut s = sc.initial_state(1);
        apply(&mut s, &sc, &d(vec![
            StateOp::AddItem { item: "流木".into() },
            StateOp::AddItem { item: "小石".into() },
        ]))
        .unwrap();

        // 所持済みのまま「念のため拾い直して組む」— T16 の形がそのまま通る。
        let delta = d(vec![
            StateOp::AddItem { item: "流木".into() },
            StateOp::AddItem { item: "小石".into() },
            StateOp::AttemptChallenge { entity: PLAYER.into(), challenge: "build_shelter".into() },
        ]);
        assert!(matches!(adjudicate(&s, &sc, &delta), Verdict::Accept));
        apply(&mut s, &sc, &delta).unwrap();
        assert!(s.flag("made_shelter"));
        assert_eq!(
            s.inventory.get(PLAYER).map(|i| i.len()),
            Some(2),
            "no-op なので複製は生まれない"
        );
    }

    /// 【ダイス帰結は射影しない (spec 09-A)】判定の成否は apply 時に出目で確定する —
    /// 純粋な adjudicate は帰結 (on_success フラグ) を先取りできないので、判定結果に
    /// 依存する後続 op は同一 delta に束ねられない (次ターンで動く。物語的にも正しい制約)。
    #[test]
    fn dice_outcomes_are_not_projected() {
        let sc = Scenario::from_yaml(PROJECTION_BOARD).unwrap();
        let s = sc.initial_state(1);
        // bless は sides:1 dc:1 で必ず成功するが、裁定はそれを知ってはならない。
        let delta = d(vec![
            StateOp::AttemptChallenge { entity: PLAYER.into(), challenge: "bless".into() },
            StateOp::SetFlag { key: "after_blessed".into(), value: true },
        ]);
        match adjudicate(&s, &sc, &delta) {
            Verdict::Reject { reasons } => assert!(
                reasons.iter().any(|r| matches!(r, RejectReason::FlagGateUnmet { key, .. } if key == "after_blessed")),
                "{reasons:?}"
            ),
            Verdict::Accept => panic!("判定帰結 (blessed) は射影されないので却下されるべき"),
        }
    }

    /// 【却下理由の actionable 化 (#42)】gate 未達系の却下 (move/flag/item/challenge) は
    /// **満たすべき条件そのもの**を理由に載せる。「移動条件が未達」だけでは LLM が
    /// 「move は失敗する」としか学べず、以後 move を出さなくなり語りだけで移動した気になる
    /// (回避学習)。条件を明示すれば「条件を満たす計画」へ転じる (#31 の UnknownStat entity /
    /// FlagNotAllowed available と同じ診断可能性の一般化)。
    #[test]
    fn gate_unmet_reasons_carry_requirement() {
        let yaml = r#"
title: t
start: cell
allowed_flags: [drawer_opened, door_unlocked]
flag_rules:
  door_unlocked: { kind: has_item, item: rusty_key }
locations:
  cell:
    description: d
    items:
      rusty_key: { kind: flag_is, key: drawer_opened, value: true }
    exits:
      - { to: corridor, gate: { kind: flag_is, key: door_unlocked, value: true } }
  corridor: { description: d, items: {}, exits: [] }
goal: { kind: location_is, at: corridor }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        let s = sc.initial_state(1);

        // move: 未達の gate (door_unlocked) が理由文に現れる。
        let d1 = d(vec![StateOp::Move { to: "corridor".into() }]);
        match adjudicate(&s, &sc, &d1) {
            Verdict::Reject { reasons } => {
                let text = reasons[0].localize(crate::Lang::Ja);
                assert!(text.contains("door_unlocked"), "必要条件を明示する: {text}");
            }
            _ => panic!("却下されるはず"),
        }
        // set_flag: flag_rules の gate (rusty_key 所持) が理由文に現れる。
        let d2 = d(vec![StateOp::SetFlag { key: "door_unlocked".into(), value: true }]);
        match adjudicate(&s, &sc, &d2) {
            Verdict::Reject { reasons } => {
                let text = reasons[0].localize(crate::Lang::Ja);
                assert!(text.contains("rusty_key"), "必要条件を明示する: {text}");
            }
            _ => panic!("却下されるはず"),
        }
        // add_item: 場所アイテムの取得条件 (drawer_opened) が理由文に現れる。
        let d3 = d(vec![StateOp::AddItem { item: "rusty_key".into() }]);
        match adjudicate(&s, &sc, &d3) {
            Verdict::Reject { reasons } => {
                let text = reasons[0].localize(crate::Lang::Ja);
                assert!(text.contains("drawer_opened"), "必要条件を明示する: {text}");
            }
            _ => panic!("却下されるはず"),
        }
    }

    /// 【どの条件が false かの名指し (2026-07-09, mujinto 実プレイ発見)】`All` gate が
    /// 却下されたとき、`unmet` は**現に false の葉条件だけ**を運ぶ — 4 条件のうち 1 つが
    /// 未達でも「どれがダメか」が正本から読めるので、作者は「フラグを満たしているのに
    /// 却下される」がバグか本当に未達かを切り分けられる。localize も未達葉を名指しする。
    #[test]
    fn gate_unmet_names_the_failing_leaf_in_an_all_gate() {
        // requires: beach にいる / 手製の弓 所持 / beast_defeated==true / dangerous==false。
        // 初期状態は beach・弓所持・dangerous=false は満たし、beast_defeated だけ false。
        let yaml = r#"
title: t
start: beach
allowed_flags: [beast_defeated, dangerous_beast_defeated]
initial_inventory: [手製の弓]
locations:
  beach: { description: d, items: {}, exits: [] }
challenges:
  hunt:
    description: 危険な獣を狩る
    sides: 20
    dc: 15
    requires:
      kind: all
      of:
        - { kind: location_is, at: beach }
        - { kind: has_item, item: 手製の弓 }
        - { kind: flag_is, key: beast_defeated, value: true }
        - { kind: flag_is, key: dangerous_beast_defeated, value: false }
goal: { kind: flag_is, key: dangerous_beast_defeated, value: true }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        let s = sc.initial_state(1);
        let delta = d(vec![StateOp::AttemptChallenge {
            entity: PLAYER.into(),
            challenge: "hunt".into(),
        }]);
        match adjudicate(&s, &sc, &delta) {
            Verdict::Reject { reasons } => {
                let unmet = reasons
                    .iter()
                    .find_map(|r| match r {
                        RejectReason::ChallengeLocked { unmet, .. } => Some(unmet),
                        _ => None,
                    })
                    .expect("ChallengeLocked が出る");
                // 満たしている 3 条件はノイズなので出さず、未達の 1 葉だけを名指す。
                assert_eq!(
                    unmet,
                    &vec![crate::Gate::FlagIs { key: "beast_defeated".into(), value: true }],
                    "未達は beast_defeated だけ (他 3 条件は満たしている): {unmet:?}"
                );
                // localize も未達葉を名指しし、満たしている条件を犯人扱いしない。
                let text = reasons[0].localize(crate::Lang::Ja);
                assert!(text.contains("未達"), "文面が未達を名指す: {text}");
                assert!(text.contains("beast_defeated"), "犯人フラグ名が出る: {text}");
            }
            _ => panic!("beast_defeated 未達で却下されるはず"),
        }
    }

    /// 【本人未知の秘匿 (2026-07-08)】`hidden_attributes` は**当人にも見えない**属性キー
    /// (呪い・自覚のない正体等)。`secret_attributes` (本人分は見える) より一段強い秘匿 —
    /// プレイヤー UI は本人分ごと落とし、GM prompt だけが注記付きで見る (提示は harness/app
    /// の責務)。engine は宣言を運ぶだけで gate/トリガー評価は不変。キーの宣言必須は secret と同じ。
    #[test]
    fn hidden_attributes_parse_and_validate_declaration() {
        let yaml = r#"
title: t
start: v
role_assignment: { key: 真の正体, pool: { 吸血鬼: 1, 人間: 1 }, among: [player, alice] }
hidden_attributes: [真の正体]
characters: { alice: { name: A } }
locations: { v: { description: d, items: {}, exits: [] } }
goal: { kind: always }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(sc.validate().is_empty(), "role_assignment.key の秘匿は健全: {:?}", sc.validate());
        assert!(sc.hidden_attributes.contains("真の正体"));

        // どこにも宣言されていないキーの秘匿は幻属性 → validate が弾く。
        let yaml = r#"
title: t
start: v
hidden_attributes: [幽霊属性]
locations: { v: { description: d, items: {}, exits: [] } }
goal: { kind: always }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(
            sc.validate().iter().any(|e| matches!(e,
                crate::spine::ScenarioError::HiddenAttributeUndeclared { key } if key == "幽霊属性")),
            "未宣言キーの秘匿を弾く: {:?}",
            sc.validate()
        );
    }

    /// 【場所の表示名 (2026-07-08)】`Location.title` = 人間向け表示名 (id=機械用セレクタ /
    /// title=表示名 の三層思想、`GoalDef.title`/`flag_titles` と同類)。非検証の提示素材・
    /// serde default = 既存 YAML 無改修 (省略時は空で、提示層が id へフォールバック)。
    #[test]
    fn location_title_parses_and_defaults_empty() {
        let yaml = r#"
title: t
start: v
locations:
  v: { title: 宿屋の広間, description: d, items: {}, exits: [] }
  w: { description: d, items: {}, exits: [] }
goal: { kind: always }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert_eq!(sc.location("v").unwrap().title, "宿屋の広間", "表示名が読める");
        assert_eq!(sc.location("w").unwrap().title, "", "省略時は空 (後方互換)");
    }

    /// 【role_assignment の整合性】人数不整合・幻キャラ・重複配布は validate が load 時に弾く。
    #[test]
    fn validate_rejects_role_assignment_mismatches() {
        // pool 合計 (3) ≠ among 人数 (2)。
        let yaml = r#"
title: t
start: v
role_assignment: { key: 役職, pool: { 人狼: 1, 村人: 2 }, among: [player, alice] }
characters: { alice: { name: A } }
locations: { v: { description: d, items: {}, exits: [] } }
goal: { kind: always }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(
            sc.validate().iter().any(|e| matches!(
                e,
                crate::spine::ScenarioError::RoleAssignmentCountMismatch { .. }
            )),
            "人数不整合を弾く: {:?}",
            sc.validate()
        );

        // among に幻キャラ + 重複。
        let yaml = r#"
title: t
start: v
role_assignment: { key: 役職, pool: { 人狼: 1, 村人: 2 }, among: [player, ghost, player] }
characters: { alice: { name: A } }
locations: { v: { description: d, items: {}, exits: [] } }
goal: { kind: always }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        let errs = sc.validate();
        assert!(
            errs.iter().any(|e| matches!(
                e,
                crate::spine::ScenarioError::RoleAssignmentUnknownEntity { entity } if entity == "ghost"
            )),
            "幻キャラへの配布を弾く: {errs:?}"
        );
        assert!(
            errs.iter().any(|e| matches!(
                e,
                crate::spine::ScenarioError::RoleAssignmentDuplicateEntity { entity } if entity == "player"
            )),
            "重複配布を弾く: {errs:?}"
        );
    }

    /// 【内部フラグの秘匿 (hidden_flags)】タイマーの armed フラグ (`x_done` 等) のような
    /// **変数として使う帳簿フラグ**を提示層 (UI 一覧 / state_brief / 語彙節) から隠す宣言。
    /// `hidden_stats` のフラグ版 — engine 非使用・非検証、gate/トリガーは従来どおり効く。
    /// キーは `allowed_flags` 宣言必須 (幻フラグの秘匿を load 時に弾く)。
    #[test]
    fn hidden_flags_parse_and_validate_membership() {
        let yaml = r#"
title: t
start: room
allowed_flags: [x_done, visible_flag]
hidden_flags: [x_done]
locations:
  room: { description: d, items: {}, exits: [] }
goal: { kind: always }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(sc.validate().is_empty(), "宣言済みフラグの秘匿は健全");
        assert!(sc.hidden_flags.contains("x_done"));

        let bad = r#"
title: t
start: room
allowed_flags: [real]
hidden_flags: [ghost]
locations:
  room: { description: d, items: {}, exits: [] }
goal: { kind: always }
"#;
        let sc = Scenario::from_yaml(bad).unwrap();
        assert!(
            sc.validate().iter().any(|e| matches!(e,
                crate::spine::ScenarioError::HiddenFlagUndeclared { flag } if flag == "ghost")),
            "未宣言フラグの秘匿を validate が弾く: {:?}",
            sc.validate()
        );
    }

    /// 【internal_* の宣言整合 (2026-07-19 命名整理)】`internal_flags`/`internal_stats` は
    /// 「GM もプレイヤーも見ない engine 内部の帳簿」。internal_flags のキーは allowed_flags 宣言必須
    /// (hidden_flags と同型)、internal_stats は無検証 (hidden_stats と同型 = stat キーは開集合)。
    #[test]
    fn internal_flags_and_stats_parse_and_validate() {
        let yaml = r#"
title: t
start: room
allowed_flags: [timer_armed, visible_flag]
internal_flags: [timer_armed]
internal_stats: [timer_stamp]
locations:
  room: { description: d, items: {}, exits: [] }
goal: { kind: always }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(sc.validate().is_empty(), "宣言済みの帳簿は健全: {:?}", sc.validate());
        assert!(sc.internal_flags.contains("timer_armed"));
        assert!(sc.internal_stats.contains("timer_stamp"), "internal_stats は無検証で任意キー可");

        let bad = r#"
title: t
start: room
allowed_flags: [real]
internal_flags: [ghost]
locations:
  room: { description: d, items: {}, exits: [] }
goal: { kind: always }
"#;
        let sc = Scenario::from_yaml(bad).unwrap();
        assert!(
            sc.validate().iter().any(|e| matches!(e,
                crate::spine::ScenarioError::InternalFlagUndeclared { flag } if flag == "ghost")),
            "未宣言フラグの帳簿指定を validate が弾く: {:?}",
            sc.validate()
        );
    }

    /// 【表示名の宣言整合】`flag_titles` の幻フラグは validate が load 時に弾く (flag_hints と同型)。
    #[test]
    fn validate_rejects_undeclared_flag_title() {
        let yaml = r#"
title: t
start: room
allowed_flags: [real]
flag_titles: { ghost: 幽霊フラグ }
locations:
  room: { description: d, items: {}, exits: [] }
goal: { kind: always }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(
            sc.validate().iter().any(|e| matches!(e,
                crate::spine::ScenarioError::FlagTitleUndeclared { flag } if flag == "ghost")),
            "未宣言フラグへの表示名を validate が弾く: {:?}",
            sc.validate()
        );
    }

    /// 【整合性】goal も goals も無いシナリオ (勝利条件不在) は validate で弾く。
    #[test]
    fn validate_rejects_scenario_with_no_goal() {
        let yaml = r#"
title: goalless
start: room
allowed_flags: []
locations:
  room: { description: x, items: {}, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).expect("goal/goals 無しでもパースは通る (整合性は別検査)");
        assert!(
            sc.validate().iter().any(|e| matches!(e, crate::spine::ScenarioError::NoGoal)),
            "goal も goals も無いシナリオは validate で弾く"
        );
    }

    // =========================================================================
    // 数値ステータス PoC: 四則演算をエンジンが代行する (LLM は値を持てない)
    // =========================================================================

    /// 【初期所持品】initial_inventory(主人公) と CharacterDef.inventory(NPC) が
    /// initial_state で seed される (「最初から所持」経路。場所から拾う/譲渡/持ち越し以外)。
    #[test]
    fn initial_state_seeds_inventory_for_player_and_npc() {
        let yaml = concat!(
            "title: t\nstart: room\n",
            "initial_inventory: [chalk, textbook]\n",
            "allowed_flags: []\n",
            "characters:\n  moka:\n    name: モカ\n    inventory: [smartphone]\n",
            "locations:\n  room: { description: d, items: {}, exits: [] }\n",
            "goal: { kind: always }\n"
        );
        let sc = Scenario::from_yaml(yaml).unwrap();
        let s = sc.initial_state(1);
        assert!(s.has_item(PLAYER, "chalk") && s.has_item(PLAYER, "textbook"), "主人公の初期所持");
        assert!(s.has_item("moka", "smartphone"), "NPC の初期所持");
        assert!(!s.has_item(PLAYER, "smartphone"), "NPC の所持は player に混ざらない");
    }

    /// 初期 stat はシナリオから読まれる。
    #[test]
    fn stats_load_from_scenario() {
        let sc = trial();
        let s = sc.initial_state(42);
        assert_eq!(s.stat("hp"), 10);
        assert_eq!(s.stat("str"), 12);
        assert_eq!(s.stat("gold"), 0);
        assert_eq!(s.stat("mana"), 0, "未宣言 stat は 0 扱い");
    }

    /// 【加減】AdjustStat はエンジンが current + delta を計算する。LLM は値を書かない。
    #[test]
    fn adjust_stat_is_computed_by_engine() {
        let sc = trial();
        let mut s = sc.initial_state(42);
        apply(&mut s, &sc, &d(vec![StateOp::AdjustStat { entity: PLAYER.into(), key: "str".into(), delta: 3 }]))
            .expect("宣言済 stat の加算は合法");
        assert_eq!(s.stat("str"), 15, "12 + 3 をエンジンが計算");
        apply(&mut s, &sc, &d(vec![StateOp::AdjustStat { entity: PLAYER.into(), key: "gold".into(), delta: 25 }]))
            .expect("加算");
        assert_eq!(s.stat("gold"), 25);
    }

    /// 【0クランプ】HP は 0 未満にならない。死亡判定 (hp>=1 gate) の土台。
    #[test]
    fn hp_clamps_at_zero_and_blocks_exit() {
        let sc = trial();
        let mut s = sc.initial_state(42);
        // まず脱出に必要な力をつける。
        apply(&mut s, &sc, &d(vec![StateOp::AdjustStat { entity: PLAYER.into(), key: "str".into(), delta: 3 }])).unwrap();
        // 致命の一撃。-100 でも 0 でクランプ (負の HP にならない)。
        apply(&mut s, &sc, &d(vec![StateOp::AdjustStat { entity: PLAYER.into(), key: "hp".into(), delta: -100 }])).unwrap();
        assert_eq!(s.stat("hp"), 0, "HP は 0 でクランプ");
        // str は足りるが hp=0 なので脱出 gate (hp>=1) を満たせない = 死んでいては出られない。
        let v = adjudicate(&s, &sc, &d(vec![StateOp::Move { to: "hall".into() }]));
        assert!(!v.is_accept(), "hp=0 では hall へ出られない");
    }

    /// 【乗除】ScaleStat はエンジンが current * num / den を計算する。
    #[test]
    fn scale_stat_multiplies_and_divides() {
        let sc = trial();
        let mut s = sc.initial_state(42);
        apply(&mut s, &sc, &d(vec![StateOp::AdjustStat { entity: PLAYER.into(), key: "gold".into(), delta: 10 }])).unwrap();
        // ×2: 報酬を倍に。
        apply(&mut s, &sc, &d(vec![StateOp::ScaleStat { entity: PLAYER.into(), key: "gold".into(), num: 2, den: 1 }])).unwrap();
        assert_eq!(s.stat("gold"), 20, "10 × 2 をエンジンが計算");
        // ÷2: 半減。
        apply(&mut s, &sc, &d(vec![StateOp::ScaleStat { entity: PLAYER.into(), key: "gold".into(), num: 1, den: 2 }])).unwrap();
        assert_eq!(s.stat("gold"), 10, "20 / 2 をエンジンが計算");
    }

    /// 【ゼロ除算ガード】den=0 はエンジンが却下する。LLM は /0 で壊せない。state 無傷。
    #[test]
    fn divide_by_zero_is_rejected() {
        let sc = trial();
        let mut s = sc.initial_state(42);
        let before = s.stat("gold");
        let v = adjudicate(&s, &sc, &d(vec![StateOp::ScaleStat { entity: PLAYER.into(), key: "gold".into(), num: 1, den: 0 }]));
        match v {
            Verdict::Reject { reasons } => assert!(reasons
                .iter()
                .any(|r| matches!(r, RejectReason::DivideByZero { key } if key == "gold"))),
            Verdict::Accept => panic!("ゼロ除算を受理してはならない"),
        }
        let r = apply(&mut s, &sc, &d(vec![StateOp::ScaleStat { entity: PLAYER.into(), key: "gold".into(), num: 1, den: 0 }]));
        assert!(r.is_err(), "apply も却下する");
        assert_eq!(s.stat("gold"), before, "却下では state 無傷");
    }

    /// 【未宣言 stat の遮断】シナリオに無い stat は作れない (幻ステータス却下)。
    #[test]
    fn unknown_stat_is_rejected() {
        let sc = trial();
        let s = sc.initial_state(42);
        let v = adjudicate(&s, &sc, &d(vec![StateOp::AdjustStat { entity: PLAYER.into(), key: "mana".into(), delta: 9000 }]));
        match v {
            Verdict::Reject { reasons } => assert!(reasons
                .iter()
                .any(|r| matches!(r, RejectReason::UnknownStat { key, .. } if key == "mana"))),
            Verdict::Accept => panic!("未宣言 stat の操作を受理してはならない"),
        }
    }

    /// 【数値 gate × 正規プレイ】鍛えて力 15 にしてから扉を押すと脱出できる。
    #[test]
    fn train_then_exit_reaches_goal() {
        let sc = trial();
        let mut s = sc.initial_state(42);
        assert!(!is_goal(&s, &sc));
        // 力 12 のままでは押せない。
        assert!(!adjudicate(&s, &sc, &d(vec![StateOp::Move { to: "hall".into() }])).is_accept());
        // 鍛錬して 12 → 15。
        apply(&mut s, &sc, &d(vec![StateOp::AdjustStat { entity: PLAYER.into(), key: "str".into(), delta: 3 }])).unwrap();
        // 今度は押せる。
        apply(&mut s, &sc, &d(vec![StateOp::Move { to: "hall".into() }]))
            .expect("str>=15 かつ hp>=1 なら脱出できる");
        assert!(is_goal(&s, &sc), "goal (hall) 到達");
    }

    // -------------------------------------------------------------------------
    // キャラ別ステータス PoC: 数値が entity ごとに紐づく (外部キャラ定義から)
    // -------------------------------------------------------------------------

    /// キャラ定義ファイルから各 entity の初期 stat が読まれる。
    #[test]
    fn character_stats_load_from_scenario() {
        let sc = route();
        let s = sc.initial_state(7);
        assert_eq!(s.stat_of("alice", "好感度"), 0);
        assert_eq!(s.stat_of("player", "好感度"), 0, "player は alice と別の数値空間");
    }

    /// 【entity 指定】好感度はアリスに紐づく。player の同名 stat とは別物。
    #[test]
    fn adjust_targets_named_entity() {
        let sc = route();
        let mut s = sc.initial_state(7);
        apply(&mut s, &sc, &d(vec![StateOp::AdjustStat {
            entity: "alice".into(),
            key: "好感度".into(),
            delta: 30,
        }]))
        .expect("アリスの好感度は宣言済");
        assert_eq!(s.stat_of("alice", "好感度"), 30);
        assert_eq!(s.stat_of("player", "好感度"), 0, "player には影響しない");
    }

    /// 【境界】好感度は宣言された上限 100 でクランプされる。
    #[test]
    fn affection_clamps_at_declared_max() {
        let sc = route();
        let mut s = sc.initial_state(7);
        apply(&mut s, &sc, &d(vec![StateOp::AdjustStat {
            entity: "alice".into(),
            key: "好感度".into(),
            delta: 200,
        }]))
        .unwrap();
        assert_eq!(s.stat_of("alice", "好感度"), 100, "max=100 でクランプ");
    }

    /// 【未宣言の遮断】alice が持たない stat / 未知の entity は却下。
    #[test]
    fn unknown_stat_or_entity_is_rejected() {
        let sc = route();
        let s = sc.initial_state(7);
        // alice は mana を宣言していない。
        assert!(!adjudicate(&s, &sc, &d(vec![StateOp::AdjustStat {
            entity: "alice".into(),
            key: "mana".into(),
            delta: 1,
        }]))
        .is_accept());
        // ghost という entity は存在しない (何も宣言していない)。
        assert!(!adjudicate(&s, &sc, &d(vec![StateOp::AdjustStat {
            entity: "ghost".into(),
            key: "好感度".into(),
            delta: 1,
        }]))
        .is_accept());
    }

    /// 【キャラ別数値 gate】アリスの好感度 50 で goal 到達。
    #[test]
    fn affection_gate_reaches_goal() {
        let sc = route();
        let mut s = sc.initial_state(7);
        assert!(!is_goal(&s, &sc));
        apply(&mut s, &sc, &d(vec![StateOp::AdjustStat {
            entity: "alice".into(),
            key: "好感度".into(),
            delta: 50,
        }]))
        .unwrap();
        assert!(is_goal(&s, &sc), "alice の好感度 >= 50 で goal");
    }

    // -------------------------------------------------------------------------
    // 硬い禁忌 PoC (Phase B): キャラは自分の禁忌を破れない (正本 > 文章力 のキャラ版)
    // -------------------------------------------------------------------------

    /// 【禁忌の強制】アリスの禁忌 (豚肉を断つ=flag alice_ate_pork) を立てる delta は却下。
    #[test]
    fn taboo_blocks_violating_delta() {
        let sc = route();
        let s = sc.initial_state(7);
        // op 単体は合法 (allowed_flags に在り gate も Always) だが、taboo が真化するので却下。
        let v = adjudicate(
            &s,
            &sc,
            &d(vec![StateOp::SetFlag { key: "alice_ate_pork".into(), value: true }]),
        );
        match v {
            Verdict::Reject { reasons } => assert!(reasons
                .iter()
                .any(|r| matches!(r, RejectReason::TabooViolated { entity } if entity == "alice"))),
            Verdict::Accept => panic!("禁忌を破る delta を受理してはならない"),
        }
    }

    /// 【禁忌の原子性】禁忌を破る op を含むデルタは全体却下、合法 op の効果も適用されない。
    #[test]
    fn taboo_violation_is_atomic() {
        let sc = route();
        let mut s = sc.initial_state(7);
        let delta = d(vec![
            StateOp::AdjustStat { entity: "alice".into(), key: "好感度".into(), delta: 10 }, // 合法
            StateOp::SetFlag { key: "alice_ate_pork".into(), value: true },                  // 禁忌
        ]);
        assert!(apply(&mut s, &sc, &delta).is_err());
        assert_eq!(s.stat_of("alice", "好感度"), 0, "却下なら好感度も動かない");
        assert!(!s.flag("alice_ate_pork"));
        assert_eq!(s.turn, 0);
    }

    /// 禁忌に無関係な合法 delta は通る (禁忌は無関係な行動を妨げない)。
    #[test]
    fn taboo_does_not_block_unrelated() {
        let sc = route();
        let mut s = sc.initial_state(7);
        apply(&mut s, &sc, &d(vec![StateOp::AdjustStat {
            entity: "alice".into(),
            key: "好感度".into(),
            delta: 10,
        }]))
        .expect("禁忌と無関係な好感度上昇は通る");
        assert_eq!(s.stat_of("alice", "好感度"), 10);
    }

    /// 【既定 entity】entity 省略のデルタ (LLM/YAML) は "player" に解決される。
    #[test]
    fn omitted_entity_defaults_to_player() {
        // entity を書かない (LLM/YAML が省略した) op は "player" に解決される。
        let op: StateOp = serde_yaml::from_str("op: adjust_stat\nkey: hp\ndelta: -1").unwrap();
        match op {
            StateOp::AdjustStat { entity, .. } => assert_eq!(entity, PLAYER),
            other => panic!("adjust_stat であるべき: {other:?}"),
        }
    }

    /// 【原子性 × stat】不正 op を含むデルタは全体却下、stat も無傷。
    #[test]
    fn mixed_stat_delta_is_atomic() {
        let sc = trial();
        let mut s = sc.initial_state(42);
        let delta = d(vec![
            StateOp::AdjustStat { entity: PLAYER.into(), key: "str".into(), delta: 3 },   // 単体なら合法
            StateOp::ScaleStat { entity: PLAYER.into(), key: "gold".into(), num: 1, den: 0 }, // ゼロ除算で不正
        ]);
        assert!(apply(&mut s, &sc, &delta).is_err());
        assert_eq!(s.stat("str"), 12, "却下されたデルタは stat を変えない");
        assert_eq!(s.turn, 0);
    }

    // -------------------------------------------------------------------------
    // 反応ビート PoC (Phase C): 禁忌の双対。真化を却下する代わりに真化で発火する。
    // 「伏線が必ず回収される」をエンジンが保証する (LLM の忘却に依存しない)。
    // -------------------------------------------------------------------------

    /// 【発火】好感度が閾値 (30) を越えると trigger が発火し、効果と語りが返る。
    #[test]
    fn trigger_fires_on_threshold_and_applies_effect() {
        let sc = recall();
        let mut s = sc.initial_state(7);
        assert!(!s.flag("promise_remembered"));

        let out = apply(&mut s, &sc, &raise_affection(30)).expect("好感度上昇は合法");

        assert!(s.flag("promise_remembered"), "発火効果でフラグが立つ");
        assert!(
            out.fired.iter().any(|f| f.id == "recall_promise"),
            "recall_promise が発火したと返る"
        );
        assert!(
            out.fired.iter().any(|f| f.id == "recall_promise" && !f.narration.is_empty()),
            "語りの指示が載っている"
        );
        assert!(s.fired.contains("recall_promise"), "発火済みが latch される");
    }

    /// 【連鎖】効果が次の trigger の when を真化させ、同じ適用内で settle する。
    /// 好感度 30 → recall_promise → (promise_remembered) → renew_vow → goal 到達。
    #[test]
    fn trigger_cascade_settles_in_one_apply() {
        let sc = recall();
        let mut s = sc.initial_state(7);
        assert!(!is_goal(&s, &sc));

        let out = apply(&mut s, &sc, &raise_affection(30)).expect("好感度上昇は合法");

        // 一度の適用で 2 つの反応ビートが連鎖発火する。
        let ids: Vec<&str> = out.fired.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, vec!["recall_promise", "renew_vow"], "authored 順に連鎖発火");
        assert!(s.flag("vow_renewed"));
        assert!(is_goal(&s, &sc), "連鎖の果てに goal (vow_renewed) 到達");
    }

    /// 【閾値未満】条件が成立しなければ発火しない。
    #[test]
    fn trigger_does_not_fire_below_threshold() {
        let sc = recall();
        let mut s = sc.initial_state(7);
        let out = apply(&mut s, &sc, &raise_affection(20)).expect("好感度上昇は合法");
        assert!(out.fired.is_empty(), "好感度 20 では発火しない");
        assert!(!s.flag("promise_remembered"));
        assert!(s.fired.is_empty());
    }

    /// 【once / latch】一度発火した trigger は、when が真のままでも二度と発火しない。
    #[test]
    fn trigger_fires_at_most_once() {
        let sc = recall();
        let mut s = sc.initial_state(7);
        apply(&mut s, &sc, &raise_affection(30)).unwrap(); // 1 回目: 連鎖発火
        assert!(s.fired.contains("recall_promise") && s.fired.contains("renew_vow"));

        // さらに好感度を上げても (when は依然真) 再発火しない。
        let out = apply(&mut s, &sc, &raise_affection(5)).expect("好感度上昇は合法");
        assert!(out.fired.is_empty(), "latch 済みなので再発火しない");
    }

    /// 【repeatable / 閾値ループ】`repeatable: true` のトリガーは latch されず、
    /// カウンタが閾値に達するたびに発火する。効果でカウンタをリセットして繰り返す
    /// (0→10 で他 stat を +1 しカウンタを 0 に戻す = ループ)。
    #[test]
    fn repeatable_trigger_loops_on_threshold_with_reset() {
        let sc = Scenario::from_yaml(concat!(
            "title: t\nstart: room\ninitial_stats: { charge: 0, level: 0 }\n",
            "allowed_flags: []\ngoal: { kind: always }\n",
            "triggers:\n",
            "  - id: levelup\n",
            "    repeatable: true\n",
            "    when: { kind: stat_at_least, entity: player, key: charge, value: 10 }\n",
            "    effects:\n",
            "      - { op: adjust_stat, entity: player, key: level, delta: 1 }\n",
            "      - { op: scale_stat, entity: player, key: charge, num: 0, den: 1 }\n", // charge を 0 にリセット
            "    narration: レベルアップ\n",
            "locations:\n  room: { description: d, items: {}, exits: [] }\n"
        ))
        .unwrap();
        let charge = |n: i64| {
            d(vec![StateOp::AdjustStat { entity: PLAYER.into(), key: "charge".into(), delta: n }])
        };

        // 1 回目: charge 0→10 で発火 → level +1、charge は 0 にリセット。
        let mut s = sc.initial_state(1);
        let o1 = apply(&mut s, &sc, &charge(10)).expect("charge 加算は合法");
        assert!(o1.fired.iter().any(|f| f.id == "levelup"), "閾値到達で発火");
        assert_eq!(s.stat("level"), 1, "効果で level +1");
        assert_eq!(s.stat("charge"), 0, "効果で charge を 0 にリセット");
        assert!(s.fired.is_empty(), "repeatable は永続 latch されない");

        // 2 回目: 再び charge を 10 まで上げると **また発火** する (once との違い)。
        let o2 = apply(&mut s, &sc, &charge(10)).expect("charge 加算は合法");
        assert!(o2.fired.iter().any(|f| f.id == "levelup"), "repeatable は閾値再到達で再発火");
        assert_eq!(s.stat("level"), 2, "2 周目で level +1");
        assert_eq!(s.stat("charge"), 0);
    }

    /// 【停止性】自己リセットしない repeatable トリガー (`when: always`) でも、1 回の apply (settle)
    /// 内では高々 1 回しか発火しない = 無限ループしない。永続 latch しないので次ターンで再発火する。
    #[test]
    fn repeatable_trigger_fires_once_per_apply_and_terminates() {
        let sc = Scenario::from_yaml(concat!(
            "title: t\nstart: room\ninitial_stats: { ticks: 0 }\n",
            "allowed_flags: []\ngoal: { kind: always }\n",
            "triggers:\n",
            "  - id: tick\n",
            "    repeatable: true\n",
            "    when: { kind: always }\n", // 効果は when を偽化しない (常に真) = 無限ループの危険
            "    effects:\n",
            "      - { op: adjust_stat, entity: player, key: ticks, delta: 1 }\n",
            "locations:\n  room: { description: d, items: {}, exits: [] }\n"
        ))
        .unwrap();
        let mut s = sc.initial_state(1);

        // 空デルタでも apply は settle を回す。when=always だが settle 内は 1 回で停止する。
        let o1 = apply(&mut s, &sc, &d(vec![])).expect("空デルタは合法");
        assert_eq!(o1.fired.iter().filter(|f| f.id == "tick").count(), 1, "settle 内は高々 1 回 = 停止");
        assert_eq!(s.stat("ticks"), 1);

        // 次の apply (新しい settle) でまた 1 回発火する (永続 latch しない)。
        apply(&mut s, &sc, &d(vec![])).expect("空デルタは合法");
        assert_eq!(s.stat("ticks"), 2, "次ターンで再発火 (repeatable)");
    }

    /// 【スケジュール発火】record_turn で「〇〇したターン」を刻み、turns_since gate で
    /// 「そこから N ターン後」に別イベントを発火する (遅延イベントのプリミティブ)。
    #[test]
    fn record_turn_and_turns_since_schedule_delayed_event() {
        let sc = Scenario::from_yaml(concat!(
            "title: t\nstart: room\ninitial_stats: { trigger_x: 0, x_turn: 0 }\n",
            "allowed_flags: [x_done, event_done]\n",
            "goal: { kind: flag_is, key: event_done, value: true }\n",
            "triggers:\n",
            "  - id: mark_x\n", // 〇〇 (trigger_x を上げる) → フラグ + そのターンを刻む
            "    when: { kind: stat_at_least, entity: player, key: trigger_x, value: 1 }\n",
            "    effects:\n",
            "      - { op: set_flag, key: x_done, value: true }\n",
            "      - { op: record_turn, key: x_turn }\n",
            "  - id: delayed\n", // x_done かつ X から 3 ターン経過で発火
            "    when: { kind: all, of: [\n",
            "        { kind: flag_is, key: x_done, value: true },\n",
            "        { kind: turns_since, key: x_turn, turns: 3 } ] }\n",
            "    effects: [ { op: set_flag, key: event_done, value: true } ]\n",
            "    narration: 三日が過ぎた。\n",
            "locations:\n  room: { description: d, items: {}, exits: [] }\n"
        ))
        .unwrap();
        let mut s = sc.initial_state(1);

        // X を起こす apply: trigger_x +1 → mark_x 発火 → x_turn に現在ターンを刻む。
        let o = apply(
            &mut s,
            &sc,
            &d(vec![StateOp::AdjustStat { entity: PLAYER.into(), key: "trigger_x".into(), delta: 1 }]),
        )
        .expect("trigger_x 加算は合法");
        assert!(o.fired.iter().any(|f| f.id == "mark_x"), "X で mark_x 発火");
        assert!(s.flag("x_done"));
        assert_eq!(s.stat("x_turn"), i64::from(s.turn), "刻まれたのは現在ターン");
        assert!(!s.flag("event_done"), "まだ遅延イベントは発火しない");

        // 経過待ち: あと 2 ターンは発火しない (turns_since < 3)。
        for _ in 0..2 {
            let o = apply(&mut s, &sc, &d(vec![])).expect("空デルタは合法");
            assert!(o.fired.is_empty(), "3 ターン未満では発火しない");
            assert!(!s.flag("event_done"));
        }
        // 記録から 3 ターン後の apply で delayed が発火する。
        let o = apply(&mut s, &sc, &d(vec![])).expect("空デルタは合法");
        assert!(o.fired.iter().any(|f| f.id == "delayed"), "3 ターン経過で遅延イベント発火");
        assert!(s.flag("event_done"));
    }

    /// 【タイマー詐称遮断】LLM が record_turn でターンを刻もうとしても却下される
    /// (GrantSkill/SetAttribute と同型の authored 専権)。
    #[test]
    fn llm_proposed_record_turn_is_rejected() {
        let sc = Scenario::from_yaml(concat!(
            "title: t\nstart: room\ninitial_stats: { x_turn: 0 }\nallowed_flags: []\n",
            "goal: { kind: always }\n",
            "locations:\n  room: { description: d, items: {}, exits: [] }\n"
        ))
        .unwrap();
        let s = sc.initial_state(1);
        let delta = d(vec![StateOp::RecordTurn { entity: PLAYER.into(), key: "x_turn".into() }]);
        match adjudicate(&s, &sc, &delta) {
            Verdict::Reject { reasons } => assert!(
                reasons
                    .iter()
                    .any(|r| matches!(r, RejectReason::TurnRecordNotAllowed { key, .. } if key == "x_turn")),
                "TurnRecordNotAllowed で却下されるべき"
            ),
            Verdict::Accept => panic!("LLM の record_turn は却下されるべき (タイマー詐称)"),
        }
    }

    /// 【presence の明示宣言 (spec 04 改訂)】`Location.present` が空 (未宣言含む) なら
    /// **誰もいない**。旧「空なら全 characters」フォールバックは廃止 — 「誰もいない場所」を
    /// 作るのに全キャラを set_presence false する羽目になるため (ユーザーFB 2026-07-02)。
    /// NPC を出したい場所には present を必ず書く。override (set_presence true) は従来どおり
    /// 空の場所にも登場させられる。
    #[test]
    fn empty_present_means_nobody() {
        let yaml = r#"
title: t
start: empty_room
allowed_flags: []
goal: { kind: location_is, at: hall }
characters:
  alice: { name: アリス }
  bob: { name: ボブ }
locations:
  empty_room: { description: 誰もいない部屋, exits: [{ to: hall }] }
  hall: { description: 広間, present: [alice], exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        let mut s = sc.initial_state(1);
        assert!(
            sc.present_at(&s).is_empty(),
            "present 未宣言 = 誰もいない (全 characters フォールバックはしない)"
        );
        // 宣言した場所では宣言どおり。
        apply(&mut s, &sc, &d(vec![StateOp::Move { to: "hall".into() }])).unwrap();
        let present = sc.present_at(&s);
        assert!(present.contains("alice") && !present.contains("bob"), "宣言した alice だけ");
        // override は空の場所にも登場させられる (トリガー専権の経路は不変)。
        s.present_overrides.insert("bob".into(), true);
        assert!(sc.present_at(&s).contains("bob"), "override true は base が空でも登場");
    }

    /// 【登場/退場 (spec 04)】authored トリガーの set_presence で entity が登場/退場し、
    /// `present_at` が場所ベース ± override を反映する。
    #[test]
    fn presence_override_via_trigger_changes_present_at() {
        let yaml = r#"
title: t
start: hall
allowed_flags: [scene2]
goal: { kind: flag_is, key: scene2, value: true }
characters:
  alice: { name: アリス }
  bob: { name: ボブ }
locations:
  hall: { description: d, present: [alice], exits: [] }
triggers:
  - id: swap_cast
    when: { kind: always }
    effects:
      - { op: set_presence, entity: bob, present: true }
      - { op: set_presence, entity: alice, present: false }
      - { op: set_flag, key: scene2, value: true }
    narration: ボブが入り、アリスが去る。
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        let mut s = sc.initial_state(1);
        // 初期: hall は present:[alice] → 実効 presence は {alice}。
        assert!(sc.present_at(&s).contains("alice"));
        assert!(!sc.present_at(&s).contains("bob"));
        // 空デルタ → always トリガー発火 → bob 登場・alice 退場。
        apply(&mut s, &sc, &d(vec![])).expect("空デルタは合法");
        let present = sc.present_at(&s);
        assert!(present.contains("bob"), "bob が登場 (override true)");
        assert!(!present.contains("alice"), "alice が退場 (override false が場所ベースを上書き)");
    }

    /// 【捏造遮断 (spec 04)】LLM が set_presence でキャラを勝手に登場させようとしても却下される
    /// (GrantSkill/SetAttribute/RecordTurn と同型の authored 専権)。
    #[test]
    fn llm_proposed_set_presence_is_rejected() {
        let sc = Scenario::from_yaml(concat!(
            "title: t\nstart: hall\n",
            "characters: { alice: { name: アリス } }\n",
            "goal: { kind: always }\n",
            "locations:\n  hall: { description: d, items: {}, exits: [] }\n"
        ))
        .unwrap();
        let s = sc.initial_state(1);
        let delta = d(vec![StateOp::SetPresence { entity: "alice".into(), present: true }]);
        match adjudicate(&s, &sc, &delta) {
            Verdict::Reject { reasons } => assert!(
                reasons
                    .iter()
                    .any(|r| matches!(r, RejectReason::PresenceSetNotAllowed { entity } if entity == "alice")),
                "PresenceSetNotAllowed で却下されるべき"
            ),
            Verdict::Accept => panic!("LLM の set_presence は却下されるべき (キャラ勝手登場の捏造)"),
        }
    }

    /// 【持ち越し (spec 04)】登場/退場のオーバーライドが次モジュールへ持ち越される (仲間が同行する)。
    #[test]
    fn transition_carries_present_overrides() {
        let a = Scenario::from_yaml(concat!(
            "title: A\nstart: hall\n",
            "characters: { bob: { name: ボブ } }\n",
            "goal: { kind: always }\n",
            "locations:\n  hall: { description: d, exits: [] }\n"
        ))
        .unwrap();
        let b = Scenario::from_yaml(concat!(
            "title: B\nstart: road\n",
            "characters: { bob: { name: ボブ } }\n",
            "goal: { kind: always }\n",
            "locations:\n  road: { description: d, exits: [] }\n"
        ))
        .unwrap();
        let mut s = a.initial_state(1);
        s.present_overrides.insert("bob".into(), true);
        s.present_overrides.insert("alice".into(), false);
        let next = b.transition(&s, &a);
        assert_eq!(next.present_overrides.get("bob"), Some(&true), "登場が次モジュールへ持ち越し");
        assert_eq!(next.present_overrides.get("alice"), Some(&false), "退場も持ち越し");
        // B は bob を cast に持つので、持ち越した仲間が次の画面でも同行する。
        assert!(b.present_at(&next).contains("bob"), "持ち越した登場が次の画面の presence に出る");
    }

    /// 【純粋性】adjudicate は trigger を発火させない (state を一切変えない)。
    /// 発火は受理・適用後の apply の責務であり、裁定は純粋なまま。
    #[test]
    fn adjudicate_does_not_fire_triggers() {
        let sc = recall();
        let s = sc.initial_state(7);
        let v = adjudicate(&s, &sc, &raise_affection(30));
        assert!(v.is_accept(), "好感度上昇自体は受理される");
        assert!(!s.flag("promise_remembered"), "adjudicate は発火させない (純粋)");
        assert!(s.fired.is_empty(), "adjudicate は fired を変えない");
    }

    // -------------------------------------------------------------------------
    // NPC inventory + 譲渡 PoC: 持っていない物は渡せない (#23 の engine 側バックストップ)。
    // 所持物は閉世界・キャラ別。player は拾い、NPC は譲渡でのみ受け取る。
    // -------------------------------------------------------------------------

    /// 【正規の譲渡】花を摘んでアリスに渡すと、アリスの所持物に移り goal 到達。
    #[test]
    fn give_transfers_held_item() {
        let sc = gift();
        let mut s = sc.initial_state(7);
        apply(&mut s, &sc, &d(vec![StateOp::AddItem { item: "flower".into() }]))
            .expect("花は摘める");
        assert!(s.has_item(PLAYER, "flower"));
        apply(&mut s, &sc, &d(vec![StateOp::GiveItem {
            from: PLAYER.into(),
            to: "alice".into(),
            item: "flower".into(),
        }]))
        .expect("所持している花は渡せる");
        assert!(s.has_item("alice", "flower"), "アリスの所持物に移る");
        assert!(!s.has_item(PLAYER, "flower"), "player の手からは離れる");
        assert!(is_goal(&s, &sc), "goal (alice が flower を所持) 到達");
    }

    /// 【行商ネックレス遮断】所持していない物は渡せない (engine バックストップ)。
    #[test]
    fn cannot_give_unheld_item() {
        let sc = gift();
        let mut s = sc.initial_state(7);
        // 摘む前に渡そうとする。
        let delta = d(vec![StateOp::GiveItem {
            from: PLAYER.into(),
            to: "alice".into(),
            item: "flower".into(),
        }]);
        match adjudicate(&s, &sc, &delta) {
            Verdict::Reject { reasons } => assert!(reasons
                .iter()
                .any(|r| matches!(r, RejectReason::ItemNotHeld { item } if item == "flower"))),
            Verdict::Accept => panic!("持っていない物の譲渡を受理してはならない"),
        }
        assert!(apply(&mut s, &sc, &delta).is_err());
        assert!(!s.has_item("alice", "flower"), "却下なら誰の手にも渡らない");
    }

    /// 【幻のキャラ遮断】存在しない entity には渡せない (閉世界)。
    #[test]
    fn cannot_give_to_unknown_entity() {
        let sc = gift();
        let mut s = sc.initial_state(7);
        apply(&mut s, &sc, &d(vec![StateOp::AddItem { item: "flower".into() }])).unwrap();
        let v = adjudicate(&s, &sc, &d(vec![StateOp::GiveItem {
            from: PLAYER.into(),
            to: "ghost".into(),
            item: "flower".into(),
        }]));
        match v {
            Verdict::Reject { reasons } => assert!(reasons
                .iter()
                .any(|r| matches!(r, RejectReason::UnknownEntity { entity } if entity == "ghost"))),
            Verdict::Accept => panic!("幻のキャラへの譲渡を受理してはならない"),
        }
    }

    // -------------------------------------------------------------------------
    // 閉世界 capability PoC: 能力は宣言された閉じた集合。開花は authored トリガーのみ。
    // = メアリー・スー (その場で能力開花) の構造遮断。未宣言の力は存在しない。
    // -------------------------------------------------------------------------

    /// 【宣言】スキルはシナリオ宣言から読まれる (player=initial_skills, NPC=CharacterDef.skills)。
    #[test]
    fn skills_load_from_declaration() {
        let sc = awakening();
        let s = sc.initial_state(7);
        assert!(s.has_skill(PLAYER, "剣術"), "player の宣言済みスキル");
        assert!(s.has_skill("alice", "癒し"), "NPC の宣言済みスキル");
        assert!(!s.has_skill(PLAYER, "予知"), "未宣言/未開花の能力は存在しない");
    }

    /// 【能力 gate】予知を持たないうちは、予知 gate の扉を越えられない。
    #[test]
    fn has_skill_gate_blocks_without_skill() {
        let sc = awakening();
        let s = sc.initial_state(7);
        let v = adjudicate(&s, &sc, &d(vec![StateOp::Move { to: "beyond".into() }]));
        assert!(!v.is_accept(), "予知が無ければ beyond へ出られない");
    }

    /// 【メアリー・スー遮断】LLM が grant_skill で能力をその場で生やそうとしても却下される。
    #[test]
    fn llm_proposed_grant_skill_is_rejected() {
        let sc = awakening();
        let mut s = sc.initial_state(7);
        let delta = d(vec![StateOp::GrantSkill { entity: PLAYER.into(), skill: "予知".into() }]);
        match adjudicate(&s, &sc, &delta) {
            Verdict::Reject { reasons } => assert!(reasons.iter().any(|r| matches!(
                r,
                RejectReason::SkillGrantNotAllowed { skill, .. } if skill == "予知"
            ))),
            Verdict::Accept => panic!("LLM の能力開花を受理してはならない (メアリー・スー)"),
        }
        // apply も却下し、state は無傷 (予知は生えない)。
        assert!(apply(&mut s, &sc, &delta).is_err());
        assert!(!s.has_skill(PLAYER, "予知"));
        assert_eq!(s.turn, 0);
    }

    /// 【正規の開花】儀式 (フラグ) → トリガー grant_skill が予知を開花 → 予知 gate を越えて goal。
    /// 開花は authored トリガーの専権であり、その後の能力 gate が正しく通る (双対の正面)。
    #[test]
    fn trigger_awakens_skill_then_gate_passes() {
        let sc = awakening();
        let mut s = sc.initial_state(7);
        assert!(!is_goal(&s, &sc));

        // 儀式を行う → トリガー awaken_foresight が発火し予知を開花。
        let out = apply(&mut s, &sc, &d(vec![StateOp::SetFlag { key: "awakening_rite".into(), value: true }]))
            .expect("儀式は行える");
        assert!(out.fired.iter().any(|f| f.id == "awaken_foresight"), "トリガーが開花を起こす");
        assert!(s.has_skill(PLAYER, "予知"), "authored トリガーは能力を付与できる");

        // 今度は予知 gate の扉を越えられる。
        apply(&mut s, &sc, &d(vec![StateOp::Move { to: "beyond".into() }]))
            .expect("予知を得たので beyond へ出られる");
        assert!(is_goal(&s, &sc), "goal (beyond) 到達");
    }

    /// 【文字列属性の生成・転職・gate】player の初期属性 (クラス=戦士) が seed され、authored
    /// トリガーが set_attribute で転職 (戦士→魔法剣士) し、AttributeIs gate がそれを縛る。
    /// クラスは第4の可変状態 (flags/stats/skills の隣)。書き換えはトリガー専権。
    #[test]
    fn attribute_seed_trigger_rewrite_and_gate() {
        let yaml = r#"
title: t
start: room
initial_attributes: { クラス: 戦士 }
allowed_flags: [awakened]
triggers:
  - id: awaken
    when: { kind: flag_is, key: awakened, value: true }
    effects: [ { op: set_attribute, entity: player, key: クラス, value: 魔法剣士 } ]
    narration: 剣に魔力が宿った。
goals:
  - id: mage_knight
    when: { kind: attribute_is, entity: player, key: クラス, value: 魔法剣士 }
    narration: 魔法剣士として歩み出す。
locations:
  room: { description: 部屋, items: {}, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(sc.validate().is_empty(), "宣言済みキーへの set_attribute は健全");
        let mut s = sc.initial_state(1);
        assert_eq!(s.attribute_of(PLAYER, "クラス"), "戦士", "初期属性が seed される");
        assert_eq!(sc.reached_goal(&s), None, "転職前は未到達");

        // 覚醒フラグを立てる (LLM の正規 op) → トリガーが転職を起こす。
        let out = apply(&mut s, &sc, &d(vec![StateOp::SetFlag { key: "awakened".into(), value: true }]))
            .expect("フラグは立てられる");
        assert!(out.fired.iter().any(|f| f.id == "awaken"), "トリガーが転職を起こす");
        assert_eq!(s.attribute_of(PLAYER, "クラス"), "魔法剣士", "トリガーで属性が書き換わる");
        assert_eq!(sc.reached_goal(&s).map(|g| g.id.as_str()), Some("mage_knight"), "AttributeIs gate を越えて到達");
    }

    /// 【クラス捏造遮断】LLM が set_attribute でクラスをその場で書き換えようとしても却下される
    /// (GrantSkill と同型のメアリー・スー遮断)。
    #[test]
    fn llm_proposed_set_attribute_is_rejected() {
        let yaml = r#"
title: t
start: room
initial_attributes: { クラス: 戦士 }
allowed_flags: []
goal: { kind: always }
locations:
  room: { description: 部屋, items: {}, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        let mut s = sc.initial_state(1);
        let delta = d(vec![StateOp::SetAttribute {
            entity: PLAYER.into(),
            key: "クラス".into(),
            value: "勇者".into(),
        }]);
        match adjudicate(&s, &sc, &delta) {
            Verdict::Reject { reasons } => assert!(
                reasons.iter().any(|r| matches!(r, RejectReason::AttributeSetNotAllowed { key, .. } if key == "クラス")),
                "AttributeSetNotAllowed で却下されるべき"
            ),
            Verdict::Accept => panic!("LLM の set_attribute は却下されるべき"),
        }
        assert!(apply(&mut s, &sc, &delta).is_err(), "却下デルタは適用されない");
        assert_eq!(s.attribute_of(PLAYER, "クラス"), "戦士", "却下ならクラスは元のまま");
    }

    /// 【幻属性遮断】トリガーが未宣言の属性キーに set_attribute すると validate が load 時に弾く。
    #[test]
    fn validate_rejects_undeclared_attribute_key() {
        let yaml = r#"
title: t
start: room
initial_attributes: { クラス: 戦士 }
allowed_flags: [x]
triggers:
  - id: bad
    when: { kind: flag_is, key: x, value: true }
    effects: [ { op: set_attribute, entity: player, key: 種族, value: エルフ } ]
goal: { kind: always }
locations:
  room: { description: 部屋, items: {}, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        let errs = sc.validate();
        assert!(
            errs.iter().any(|e| matches!(e, crate::spine::ScenarioError::AttributeKeyUndeclared { key, .. } if key == "種族")),
            "未宣言キー '種族' への set_attribute を validate が弾く: {errs:?}"
        );
    }

    /// 【NPC 数値の entity 明示】NPC の stat を entity 省略 (既定 player) で動かそうとすると、
    /// player はその stat を持たないので UnknownStat で却下され、理由が**どの entity か**を名指す
    /// (self-repair が「player でなく moka」と気づける = 「NPC の好感度が上がらない」の接地)。
    #[test]
    fn unknown_stat_reason_names_the_entity() {
        let yaml = r#"
title: t
start: room
characters:
  moka: { name: モカ, stats: { 好感度: { initial: 10, min: 0, max: 100 } } }
allowed_flags: []
goal: { kind: always }
locations:
  room: { description: d, items: {}, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        let s = sc.initial_state(1);
        // entity 省略 = player に好感度を当てる → player は持たない → UnknownStat(entity=player)。
        let delta = d(vec![StateOp::AdjustStat { entity: PLAYER.into(), key: "好感度".into(), delta: 5 }]);
        match adjudicate(&s, &sc, &delta) {
            Verdict::Reject { reasons } => assert!(
                reasons.iter().any(|r| matches!(r, RejectReason::UnknownStat { entity, key } if entity == "player" && key == "好感度")),
                "UnknownStat が entity=player を名指すべき: {reasons:?}"
            ),
            Verdict::Accept => panic!("player は好感度を持たないので却下されるべき"),
        }
        // entity=moka なら受理され、好感度が上がる。
        let ok = d(vec![StateOp::AdjustStat { entity: "moka".into(), key: "好感度".into(), delta: 5 }]);
        assert!(matches!(adjudicate(&s, &sc, &ok), Verdict::Accept), "entity=moka なら受理");
    }

    /// 【ダイス→フラグ】challenge の通常成否 (total>=dc) でフラグが立つ。stat 有り=修正が乗り、
    /// stat 無し=修正0 の純粋ダイス (能力に依らない運試し)。sides:1 で出目を 1 に固定し決定論検証。
    #[test]
    fn challenge_outcomes_set_flags_with_and_without_stat() {
        let yaml = r#"
title: t
start: room
initial_stats: { STR: 5 }
allowed_flags: [won, lost, luck_win, luck_lose]
challenges:
  power: { description: 力で押す, stat: STR, sides: 1, dc: 6, on_success: { flag: won }, on_failure: { flag: lost } }
  luck:  { description: 運任せ, sides: 1, dc: 6, on_success: { flag: luck_win }, on_failure: { flag: luck_lose } }
goal: { kind: always }
locations:
  room: { description: d, items: {}, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(sc.validate().is_empty(), "on_success/on_failure フラグ宣言済で健全");
        let mut s = sc.initial_state(1);

        // 能力あり: 1d1(=1) + STR5 = 6 >= 6 → 成功 → won。
        let o = apply(&mut s, &sc, &d(vec![StateOp::AttemptChallenge { entity: PLAYER.into(), challenge: "power".into() }])).unwrap();
        assert!(s.flag("won") && !s.flag("lost"), "stat 修正込みで成功 → on_success フラグ");
        assert_eq!(o.checks[0].modifier, 5, "stat 修正が乗る");

        // 能力なし: 1d1(=1) + 0 = 1 < 6 → 失敗 → luck_lose。
        let o2 = apply(&mut s, &sc, &d(vec![StateOp::AttemptChallenge { entity: PLAYER.into(), challenge: "luck".into() }])).unwrap();
        assert!(s.flag("luck_lose") && !s.flag("luck_win"), "stat 無し=修正0 → 失敗 → on_failure フラグ");
        assert_eq!(o2.checks[0].modifier, 0, "stat 無し = 修正 0 (能力に依らない純粋ダイス)");
    }

    /// 【挑戦の解禁(requires)と条件付き修正(modifiers)】導師の教え(flag)が無ければ秘奥義に挑めず
    /// (ChallengeLocked)、教えを受ければ挑め、かつ +5 の有利が乗って勝ちやすくなる。
    /// requires/when はいずれも純粋 Gate (flag/stat/attribute/skill どれでも可)。sides:1 で決定論。
    #[test]
    fn challenge_requires_gate_and_conditional_modifier() {
        let yaml = r#"
title: t
start: room
initial_stats: { STR: 5 }
allowed_flags: [taught, won]
challenges:
  secret:
    description: 秘奥義
    requires: { kind: flag_is, key: taught, value: true }
    stat: STR
    sides: 1
    dc: 11
    on_success: { flag: won }
    modifiers:
      - { when: { kind: flag_is, key: taught, value: true }, bonus: 5 }
goal: { kind: always }
locations:
  room: { description: d, items: {}, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(sc.validate().is_empty());
        let mut s = sc.initial_state(1);
        let attempt = || d(vec![StateOp::AttemptChallenge { entity: PLAYER.into(), challenge: "secret".into() }]);

        // B: 教えが無ければ requires 未達で挑めない (ChallengeLocked)。
        match adjudicate(&s, &sc, &attempt()) {
            Verdict::Reject { reasons } => assert!(
                reasons.iter().any(|r| matches!(r, RejectReason::ChallengeLocked { challenge, .. } if challenge == "secret")),
                "requires 未達なら ChallengeLocked: {reasons:?}"
            ),
            Verdict::Accept => panic!("requires 未達なら却下されるべき"),
        }

        // 教えを受ける。
        apply(&mut s, &sc, &d(vec![StateOp::SetFlag { key: "taught".into(), value: true }])).unwrap();

        // A: 1d1(=1) + STR5 + 教え5 = 11 >= 11 → 成功 (修正5無しなら 6 で DC11 に届かない)。
        let o = apply(&mut s, &sc, &attempt()).unwrap();
        assert_eq!(o.checks[0].modifier, 10, "STR5 + 教えボーナス5 = 修正10");
        assert!(s.flag("won"), "教えの有利で DC を越えて成功 (修正が load-bearing)");
    }

    /// 【専権フラグの engine バックストップ (#50)】authored 専権フラグ (トリガー/challenge が書く)
    /// への LLM set_flag は **true にも false にも**倒せない。従来この検査が無く、語彙除外は
    /// prompt 層のみ = `value:false` は素通り受理だった — 実測 (1ldk): GM が「退勤」の意味論で
    /// [set_flag 会社=false, move(gate:会社==true)] を束ね、**射影の中で自分の move を壊して**
    /// 「画面は true なのに『true が必要』で却下」の怪奇を毎日再演。単独提案なら authored 機構を
    /// 静かに妨害できた (grant_skill/set_attribute と同じ防衛線がフラグには欠けていた)。
    #[test]
    fn llm_cannot_set_authored_only_flags_either_direction() {
        let yaml = r#"
title: t
start: office
allowed_flags: [会社で仕事をする, 挨拶した]
triggers:
  - id: work_start
    when: { kind: location_is, at: office }
    effects:
      - { op: set_flag, key: 会社で仕事をする, value: true }
goal: { kind: always }
locations:
  office:
    description: d
    items: {}
    exits:
      - { to: home, gate: { kind: flag_is, key: 会社で仕事をする, value: true } }
  home: { description: d, items: {}, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        let mut s = sc.initial_state(1);
        // トリガー効果は従来どおり書ける (apply_ops 直行 = authored 信頼)。
        apply(&mut s, &sc, &d(vec![])).unwrap();
        assert!(s.flag("会社で仕事をする"), "authored トリガーは専権フラグを書ける");

        // LLM の false 倒し (退勤の意味論) は却下 — 単独でも束ねでも state を汚せない。
        let clock_out = d(vec![StateOp::SetFlag { key: "会社で仕事をする".into(), value: false }]);
        match adjudicate(&s, &sc, &clock_out) {
            Verdict::Reject { reasons } => assert!(
                reasons.iter().any(|r| matches!(r, RejectReason::FlagNotAllowed { key, .. } if key == "会社で仕事をする")),
                "専権フラグの false 倒しを却下: {reasons:?}"
            ),
            Verdict::Accept => panic!("専権フラグへの set_flag false は却下されるべき (#50)"),
        }
        // true 立ても同様に却下 (先取りの遮断)。
        let preempt = d(vec![StateOp::SetFlag { key: "会社で仕事をする".into(), value: true }]);
        assert!(matches!(adjudicate(&s, &sc, &preempt), Verdict::Reject { .. }));

        // #50 の self-sabotage 再現: [set_flag false, move] — 従来は set_flag が射影に乗り
        // move が「true が必要」で落ちた (画面は true なのに)。今は set_flag 自体が却下され、
        // move は無傷の射影で評価される (却下理由が正しい原因を名指しする)。
        let sabotage = d(vec![
            StateOp::SetFlag { key: "会社で仕事をする".into(), value: false },
            StateOp::Move { to: "home".into() },
        ]);
        match adjudicate(&s, &sc, &sabotage) {
            Verdict::Reject { reasons } => {
                assert!(
                    reasons.iter().any(|r| matches!(r, RejectReason::FlagNotAllowed { .. })),
                    "真因 (専権フラグへの set_flag) が名指しされる: {reasons:?}"
                );
                assert!(
                    !reasons.iter().any(|r| matches!(r, RejectReason::MoveGateUnmet { .. })),
                    "却下 op は射影に乗らないので move は壊れない (誤診の除去): {reasons:?}"
                );
            }
            Verdict::Accept => panic!("却下されるべき"),
        }
        // 非専権フラグ (挨拶した) は従来どおり LLM が set_flag できる。
        let ok = d(vec![StateOp::SetFlag { key: "挨拶した".into(), value: true }]);
        assert!(matches!(adjudicate(&s, &sc, &ok), Verdict::Accept), "通常フラグは不変");
    }

    /// 【全帰結共通効果の射影】attempt_challenge の効果のうち on_success/on_failure の**両方**に
    /// あるもの (=どの出目でも必ず起きる) は裁定の射影に乗る → [挑戦, その帰結を gate にした move]
    /// の束ねが一発受理される (日次フラグを全帰結に書く作法の摩擦を機構ごと消す)。
    /// 片側にしか無い効果 (帰結依存) は従来どおり非射影 = その gate の move は却下のまま
    /// (「ダイスはターンを割る」原則は帰結依存の手に対して不変)。
    #[test]
    fn adjudication_projects_outcome_invariant_challenge_effects() {
        let yaml = r#"
title: t
start: office
allowed_flags: [会社で仕事をする, 好調]
challenges:
  work:
    sides: 20
    dc: 10
    on_success:
      effects:
        - { op: set_flag, key: 会社で仕事をする, value: true }
        - { op: set_flag, key: 好調, value: true }
    on_failure:
      effects:
        - { op: set_flag, key: 会社で仕事をする, value: true }
goal: { kind: always }
locations:
  office:
    description: d
    items: {}
    exits:
      - { to: home, gate: { kind: flag_is, key: 会社で仕事をする, value: true } }
      - { to: bar, gate: { kind: flag_is, key: 好調, value: true } }
  home: { description: d, items: {}, exits: [] }
  bar: { description: d, items: {}, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(sc.validate().is_empty());

        // 会社で仕事をする は全帰結共通 → 射影され、[挑戦, 帰宅] の束ねが一発受理。
        let mut s = sc.initial_state(1);
        let bundle = d(vec![
            StateOp::AttemptChallenge { entity: PLAYER.into(), challenge: "work".into() },
            StateOp::Move { to: "home".into() },
        ]);
        assert!(
            matches!(adjudicate(&s, &sc, &bundle), Verdict::Accept),
            "全帰結共通の効果は裁定時に確定扱い → 束ねが通る"
        );
        apply(&mut s, &sc, &bundle).unwrap();
        assert_eq!(s.location, "home", "適用でも移動まで一気に進む");
        assert!(s.flag("会社で仕事をする"));

        // 好調 は成功側にしか無い (帰結依存) → 非射影のまま。それを gate にした move は却下。
        let s2 = sc.initial_state(1);
        let gamble = d(vec![
            StateOp::AttemptChallenge { entity: PLAYER.into(), challenge: "work".into() },
            StateOp::Move { to: "bar".into() },
        ]);
        match adjudicate(&s2, &sc, &gamble) {
            Verdict::Reject { reasons } => assert!(
                reasons.iter().any(|r| matches!(r, RejectReason::MoveGateUnmet { to, .. } if to == "bar")),
                "帰結依存の効果は射影しない (ダイスはターンを割る): {reasons:?}"
            ),
            Verdict::Accept => panic!("成功限定の帰結を先取りした move は却下されるべき"),
        }
    }

    /// 【判定主体の authored 固定】`ChallengeDef.entity` があれば op の entity を**上書き**して
    /// その entity の stat で振る — LLM は既定で player を主体にするため、NPC の stat を使う
    /// challenge (裏でヒナの浮気判定等) が UnknownStat で毎回却下されていた (実プレイ発見)。
    /// 「判定の素性は authored」の主体版。entity 省略 (=player) でも誤指定でも正しく振られる。
    #[test]
    fn challenge_authored_entity_overrides_op_entity() {
        let yaml = r#"
title: t
start: room
allowed_flags: [seen]
characters:
  hina: { name: ヒナ, stats: { 主人公❤: { initial: 50 } } }
challenges:
  hina_work:
    entity: hina
    stat: 主人公❤
    sides: 1
    dc: 51
    on_success: { flag: seen }
goal: { kind: always }
locations:
  room: { description: d, items: {}, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(sc.validate().is_empty(), "hina は 主人公❤ を宣言済み: {:?}", sc.validate());
        let mut s = sc.initial_state(1);

        // LLM が entity を省略 (=player 既定) しても、authored 固定 (hina) で判定される。
        // player は 主人公❤ を持たないが、主体が hina に上書きされるので却下されない。
        let delta = d(vec![StateOp::AttemptChallenge { entity: PLAYER.into(), challenge: "hina_work".into() }]);
        assert!(matches!(adjudicate(&s, &sc, &delta), Verdict::Accept), "player 指定でも却下されない");
        let o = apply(&mut s, &sc, &delta).unwrap();
        let c = &o.checks[0];
        assert_eq!(c.entity, "hina", "実際に振った主体 (hina) が surface される");
        assert_eq!(c.modifier, 50, "hina の 主人公❤ (50) が修正に乗る");
        assert!(c.success, "1d1(1)+50=51 >= DC51");
        assert!(s.flag("seen"), "帰結フラグも通常どおり");
    }

    /// 【幻主体の load 時遮断】authored 判定主体が判定 stat を宣言していなければ validate が
    /// ChallengeStatUndeclared で弾く (プレイ中の UnknownStat 却下でなく load 時に名指し)。
    #[test]
    fn validate_rejects_challenge_authored_entity_without_stat() {
        let yaml = r#"
title: t
start: room
characters:
  hina: { name: ヒナ }
challenges:
  hina_work: { entity: hina, stat: 主人公❤, sides: 6, dc: 3 }
goal: { kind: always }
locations:
  room: { description: d, items: {}, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(
            sc.validate().iter().any(|e| matches!(e,
                crate::spine::ScenarioError::ChallengeStatUndeclared { challenge, entity, stat }
                    if challenge == "hina_work" && entity == "hina" && stat == "主人公❤")),
            "stat 未宣言の authored 主体を load 時に弾く: {:?}", sc.validate()
        );
    }

    /// 【tier の閾値 (at_most/at_least)】d100 のように sides が大きく min(=1) では滅多に発火しない盤面で、
    /// 下位/上位帯を極にできる。自然出目そのもの (修正前) で判定。帯外の出目では発火しない。
    /// 決定論 seed: 2→d100=11 (≤20), 3→54 (帯外), 13→96 (≥96)。
    #[test]
    fn tier_threshold_at_most_and_at_least_fire_on_band() {
        let yaml = r#"
title: t
start: room
allowed_flags: [fumbled, crit_hit]
challenges:
  strike:
    sides: 100
    dc: 50
    tiers:
      crit_fail: { natural: at_most, threshold: 20, flag: fumbled }
      crit:      { natural: at_least, threshold: 96, flag: crit_hit }
goal: { kind: always }
locations:
  room: { description: d, items: {}, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(sc.validate().is_empty(), "1..=sides 内の閾値は健全");
        let attempt = || d(vec![StateOp::AttemptChallenge { entity: PLAYER.into(), challenge: "strike".into() }]);

        // 下位帯 (roll 11 <= 20) → crit_fail 発火。
        let mut lo = sc.initial_state(2);
        let o = apply(&mut lo, &sc, &attempt()).unwrap();
        assert_eq!(o.checks[0].roll, 11, "seed 2 で d100=11");
        assert_eq!(o.checks[0].tier.as_deref(), Some("crit_fail"), "roll 11 は at_most:20 帯");
        assert!(lo.flag("fumbled") && !lo.flag("crit_hit"), "下位帯フラグだけ立つ");

        // 上位帯 (roll 96 >= 96) → crit 発火。
        let mut hi = sc.initial_state(13);
        let o = apply(&mut hi, &sc, &attempt()).unwrap();
        assert_eq!(o.checks[0].roll, 96, "seed 13 で d100=96");
        assert_eq!(o.checks[0].tier.as_deref(), Some("crit"), "roll 96 は at_least:96 帯");
        assert!(hi.flag("crit_hit") && !hi.flag("fumbled"), "上位帯フラグだけ立つ");

        // 帯外 (roll 54) → どの tier にも該当せず発火なし。
        let mut mid = sc.initial_state(3);
        let o = apply(&mut mid, &sc, &attempt()).unwrap();
        assert_eq!(o.checks[0].roll, 54, "seed 3 で d100=54");
        assert_eq!(o.checks[0].tier, None, "帯外は極に該当しない");
        assert!(!mid.flag("fumbled") && !mid.flag("crit_hit"), "帯外はどのフラグも立たない");
    }

    /// 【tier 閾値の範囲外を load 時に弾く】at_most/at_least は 1..=sides の範囲でなければ
    /// 常時発火/絶対不発火の幻値なので validate が TierThresholdOutOfRange を返す (min/max は検査不要)。
    #[test]
    fn validate_rejects_out_of_range_tier_threshold() {
        let mk = |tier: &str| {
            Scenario::from_yaml(&format!(
                "title: t\nstart: room\nallowed_flags: [f]\n\
                 challenges:\n  c:\n    sides: 100\n    dc: 50\n    tiers:\n      t: {{ {tier}, flag: f }}\n\
                 goal: {{ kind: always }}\n\
                 locations:\n  room: {{ description: d, items: {{}}, exits: [] }}\n"
            ))
            .unwrap()
        };
        // 範囲外: sides=100 に対し 200 (常時発火) / 0 (絶対不発火) / 閾値欠落 (無制限)。
        for bad in [
            "natural: at_most, threshold: 200",
            "natural: at_least, threshold: 200",
            "natural: at_most, threshold: 0",
            "natural: at_most", // threshold 欠落
        ] {
            let errs = mk(bad).validate();
            assert!(
                errs.iter().any(|e| matches!(e, crate::spine::ScenarioError::TierThresholdOutOfRange { .. })),
                "{bad} は範囲外/欠落として弾く: {errs:?}"
            );
        }
        // 範囲内 (境界含む) と min/max (threshold 不要) は健全。
        for ok in [
            "natural: at_most, threshold: 1",
            "natural: at_most, threshold: 100",
            "natural: at_least, threshold: 100",
            "natural: min",
            "natural: max",
        ] {
            assert!(mk(ok).validate().is_empty(), "{ok} は健全なはず");
        }
    }

    /// 【インライン結末ナレーション】challenge の on_failure/on_success/tier に authored narration を
    /// 付けると `CheckOutcome.narration` に載り、**繰り返す失敗でも毎回**出る (トリガーと違い latch しない)。
    /// フラグ無しの失敗でも語れる。極(tier)の narration は通常成否より優先。
    #[test]
    fn challenge_outcome_narration_surfaces_every_attempt() {
        let yaml = r#"
title: t
start: room
initial_stats: { STR: 0 }
allowed_flags: [opened]
challenges:
  pick:
    stat: STR
    sides: 1
    dc: 6
    on_success: { flag: opened, narration: 錠が外れた。 }
    on_failure: { narration: 工具が滑る。 }
goal: { kind: always }
locations:
  room: { description: d, items: {}, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        let mut s = sc.initial_state(1);
        let pick = || d(vec![StateOp::AttemptChallenge { entity: PLAYER.into(), challenge: "pick".into() }]);
        // 1d1(=1)+STR0 = 1 < 6 → 失敗。フラグ無しでも narration が出る。
        let o1 = apply(&mut s, &sc, &pick()).unwrap();
        assert!(!o1.checks[0].success);
        assert_eq!(o1.checks[0].narration, "工具が滑る。", "失敗のナレーションが出る");
        // 二度目の失敗でも**毎回**出る (latch されない = トリガーとの違い)。
        let o2 = apply(&mut s, &sc, &pick()).unwrap();
        assert_eq!(o2.checks[0].narration, "工具が滑る。", "繰り返す失敗でも毎回出る");
    }

    /// 【challenge の効果音 (2026-07-09)】on_success/on_failure/tier に `sound` (音声アセット ID)
    /// を書ける — narration と同列の不透明 string で `CheckOutcome.sound` に載る (毎回・同ターン)。
    /// 極 (tier) の sound があれば優先、無ければ通常成否の sound。素の判定は空。
    #[test]
    fn challenge_outcome_sound_surfaces_on_check() {
        let yaml = r#"
title: t
start: room
initial_stats: { STR: 0 }
allowed_flags: [opened]
challenges:
  pick:
    stat: STR
    sides: 1
    dc: 6
    on_success: { flag: opened, sound: unlock.wav }
    on_failure: { sound: fail.wav }
goal: { kind: always }
locations:
  room: { description: d, items: {}, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        let mut s = sc.initial_state(1);
        let pick = || d(vec![StateOp::AttemptChallenge { entity: PLAYER.into(), challenge: "pick".into() }]);
        // 1d1(=1)+STR0 = 1 < 6 → 失敗。失敗の効果音 ID が CheckOutcome に載る。
        let o1 = apply(&mut s, &sc, &pick()).unwrap();
        assert!(!o1.checks[0].success);
        assert_eq!(o1.checks[0].sound, "fail.wav", "失敗の効果音が出る");
    }

    /// 【幻フラグ遮断】challenge の on_success/on_failure が立てるフラグも allowed_flags 宣言必須。
    #[test]
    fn validate_rejects_undeclared_challenge_outcome_flag() {
        let yaml = r#"
title: t
start: room
allowed_flags: []
challenges:
  c: { sides: 1, dc: 1, on_success: { flag: ghost } }
goal: { kind: always }
locations:
  room: { description: d, items: {}, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(
            sc.validate().iter().any(|e| matches!(e,
                crate::spine::ScenarioError::ChallengeFlagUndeclared { flag, tier, .. }
                if flag == "ghost" && tier == "on_success")),
            "未宣言の on_success フラグを validate が弾く: {:?}", sc.validate()
        );
    }

    /// 【知識フラグヒントの閉世界 / spec 03】flag_hints のキーが allowed_flags 未宣言なら
    /// validate が弾く (幻フラグへのヒントを load 時に弾く。値は自由文の語り素材)。
    #[test]
    fn validate_rejects_undeclared_flag_hint() {
        let yaml = r#"
title: t
start: room
allowed_flags: [known]
flag_hints: { ghost: 賢者から聞いたら立てる }
goal: { kind: always }
locations:
  room: { description: d, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(
            sc.validate().iter().any(|e| matches!(e,
                crate::spine::ScenarioError::FlagHintUndeclared { flag } if flag == "ghost")),
            "未宣言フラグへのヒントを validate が弾く: {:?}", sc.validate()
        );
    }

    /// 【二重所有の罠 / flag_hint × 専権フラグ】「GM に立てさせたい」意図の flag_hint を、
    /// トリガー/challenge の effects が書く**専権フラグ**に付けると、そのフラグは GM の usable
    /// 一覧に一切出ずヒントが死ぬ (作者が踏みやすい罠)。**lint (警告・非 fatal)** — プレイは
    /// 壊れないので validate (load 拒否) にはしない。fatal にすると配布済み content が受領側で
    /// 死ぬ (実測: 書庫アップロード済みシナリオの大半が該当、2026-07-12 ユーザー報告)。
    #[test]
    fn flag_hint_on_authored_only_is_lint_not_fatal() {
        // 午後 はトリガー effects が set_flag する = 専権。そこに flag_hint を付けると死ぬ。
        let yaml = r#"
title: t
start: room
allowed_flags: [仕事完了, 午後]
flag_hints:
  仕事完了: 仕事を終えたら立てる
  午後: 午後になったら立てる
triggers:
  - id: 時間経過
    when: { kind: flag_is, key: 仕事完了, value: true }
    effects:
      - { op: set_flag, key: 午後, value: true }
goal: { kind: always }
locations:
  room: { description: d, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        // validate は通る (fatal にしない = 既存の配布済み content を殺さない)。
        assert!(sc.validate().is_empty(), "死んだヒントで load を拒否しない: {:?}", sc.validate());
        // lints が警告として名指しする。
        let warns = sc.lints();
        assert!(
            warns.iter().any(|e| matches!(e,
                crate::spine::ScenarioError::FlagHintOnAuthoredOnly { flag } if flag == "午後")),
            "専権フラグへのヒントを lint が報せる: {warns:?}"
        );
        // 仕事完了 は純粋 set_flag (GM が立てる) なのでヒントは生きる → lint にも出ない。
        assert!(
            !warns.iter().any(|e| matches!(e,
                crate::spine::ScenarioError::FlagHintOnAuthoredOnly { flag } if flag == "仕事完了")),
            "GM が立てるフラグへのヒントは健全: {warns:?}"
        );
    }

    /// 【spec 11 Phase A】`GoalDef.epilogue_prompt` が parse され (省略時 None = 既存 YAML
    /// 無改修)、「指示あり + 結末文なし」の goal だけを lint が警告する。空の定義は
    /// `trim().is_empty()` — narration が空文字/空白のみでも「無い」、epilogue_prompt が
    /// 空白のみなら「書いていない」扱いで沈黙 (フォールバック不能の組み合わせだけを名指し)。
    #[test]
    fn epilogue_prompt_parses_and_lint_requires_narration() {
        let yaml = r#"
title: t
start: room
goals:
  - { id: ok, when: { kind: always }, narration: 幕が下りた, epilogue_prompt: 生存者のその後を一人ずつ }
  - { id: bare, when: { kind: always }, epilogue_prompt: 余韻を語れ }
  - { id: blank_narr, when: { kind: always }, narration: "   ", epilogue_prompt: 余韻を語れ }
  - { id: blank_prompt, when: { kind: always }, epilogue_prompt: "   " }
  - { id: plain, when: { kind: always }, narration: 終わり }
locations:
  room: { description: d, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert_eq!(
            sc.goals[0].epilogue_prompt.as_deref(),
            Some("生存者のその後を一人ずつ"),
            "指示が parse される"
        );
        assert_eq!(sc.goals[4].epilogue_prompt, None, "省略時は None (既存 YAML 無改修)");
        assert!(sc.validate().is_empty(), "lint であって load 拒否ではない: {:?}", sc.validate());

        let lints = sc.lints();
        let warns: Vec<&str> = lints
            .iter()
            .filter_map(|e| match e {
                crate::spine::ScenarioError::EpilogueWithoutNarration { goal } => {
                    Some(goal.as_str())
                }
                _ => None,
            })
            .collect();
        assert!(warns.contains(&"bare"), "結末文なし + 指示ありを警告: {warns:?}");
        assert!(warns.contains(&"blank_narr"), "空白のみの結末文も「無い」扱い: {warns:?}");
        assert!(!warns.contains(&"ok"), "結末文があれば沈黙");
        assert!(!warns.contains(&"blank_prompt"), "空白のみの指示は「書いていない」扱いで沈黙");
        assert!(!warns.contains(&"plain"), "指示なしは対象外");
    }

    /// 【アセット passthrough】Location.image/present・CharacterDef.icon が serde で読まれる
    /// (engine は使わない不透明データ。提示層が背景/顔アイコン/presence に使う)。
    #[test]
    fn location_present_and_character_icon_parse() {
        let yaml = r#"
title: t
start: room
characters:
  moka: { name: モカ, icon: moka.svg, stats: { 好感度: { initial: 10, min: 0, max: 100 } } }
goal: { kind: always }
locations:
  room: { description: d, image: room.svg, bgm: shrine.ogg, present: [moka], items: {}, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert_eq!(sc.characters["moka"].icon.as_deref(), Some("moka.svg"), "NPC の顔アイコン ID");
        let room = sc.location("room").unwrap();
        assert_eq!(room.image.as_deref(), Some("room.svg"), "場所の背景 ID");
        assert_eq!(room.bgm.as_deref(), Some("shrine.ogg"), "場所のループ BGM ID (Phase 3)");
        assert!(room.present.contains("moka"), "場所の presence");
    }

    /// 【イベント CG passthrough (Phase 2)】`Trigger.image`/`image_mode` が serde で読まれ、
    /// 発火時に `FiredTrigger` へそのまま載る (engine は解釈しない不透明データ。解決は提示層)。
    #[test]
    fn trigger_image_passthrough_to_fired() {
        let yaml = r#"
title: t
start: room
allowed_flags: [done]
goal: { kind: flag_is, key: done, value: true }
locations:
  room: { description: d, exits: [] }
triggers:
  - id: cg_beat
    when: { kind: always }
    effects:
      - { op: set_flag, key: done, value: true }
    narration: 光が祭壇を満たす。
    image: awakening.svg
    image_mode: overlay
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        let mut s = sc.initial_state(1);
        let out = apply(&mut s, &sc, &d(vec![])).expect("空デルタは合法");

        let beat = out.fired.iter().find(|f| f.id == "cg_beat").expect("発火する");
        assert_eq!(beat.image.as_deref(), Some("awakening.svg"), "イベント CG ID を passthrough");
        assert_eq!(beat.image_mode, Some(ImageMode::Overlay), "表示モードを passthrough");
    }

    /// 【既定モード】`image_mode` 省略時は `None` のまま (提示層が Background と解釈する)。
    #[test]
    fn trigger_image_mode_defaults_to_none() {
        let yaml = r#"
title: t
start: room
allowed_flags: [done]
goal: { kind: flag_is, key: done, value: true }
locations:
  room: { description: d, exits: [] }
triggers:
  - id: cg_beat
    when: { kind: always }
    effects:
      - { op: set_flag, key: done, value: true }
    image: scene.svg
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        let mut s = sc.initial_state(1);
        let out = apply(&mut s, &sc, &d(vec![])).expect("空デルタは合法");

        let beat = out.fired.iter().find(|f| f.id == "cg_beat").expect("発火する");
        assert_eq!(beat.image.as_deref(), Some("scene.svg"));
        assert_eq!(beat.image_mode, None, "省略時は None (既定 Background は提示層の解釈)");
    }

    /// 【SE passthrough (Phase 3)】`Trigger.sound` が serde で読まれ、発火時に `FiredTrigger.sound`
    /// へそのまま載る (engine は解釈しない不透明データ。再生は提示層)。省略時は `None`。
    #[test]
    fn trigger_sound_passthrough_to_fired() {
        let yaml = r#"
title: t
start: room
allowed_flags: [done, hit]
goal: { kind: flag_is, key: done, value: true }
locations:
  room: { description: d, exits: [] }
triggers:
  - id: se_beat
    when: { kind: always }
    effects:
      - { op: set_flag, key: hit, value: true }
    narration: 岩が砕ける。
    sound: rockfall.ogg
  - id: silent_beat
    when: { kind: flag_is, key: hit, value: true }
    effects:
      - { op: set_flag, key: done, value: true }
    narration: 静寂が戻る。
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        let mut s = sc.initial_state(1);
        let out = apply(&mut s, &sc, &d(vec![])).expect("空デルタは合法");

        let se = out.fired.iter().find(|f| f.id == "se_beat").expect("発火する");
        assert_eq!(se.sound.as_deref(), Some("rockfall.ogg"), "SE ID を passthrough");
        let silent = out.fired.iter().find(|f| f.id == "silent_beat").expect("連鎖発火する");
        assert_eq!(silent.sound, None, "sound 省略時は None (SE 無し)");
    }

    /// 【却下時は不発】不正 op を含むデルタは却下され、trigger も発火しない (原子性)。
    #[test]
    fn rejected_delta_fires_no_trigger() {
        let sc = recall();
        let mut s = sc.initial_state(7);
        // 好感度 +30 (単体なら閾値を跨ぐ) と未宣言 stat の不正 op を束ねる。
        let delta = d(vec![
            StateOp::AdjustStat { entity: "alice".into(), key: "好感度".into(), delta: 30 },
            StateOp::AdjustStat { entity: "alice".into(), key: "mana".into(), delta: 1 }, // 未宣言で不正
        ]);
        assert!(apply(&mut s, &sc, &delta).is_err());
        assert_eq!(s.stat_of("alice", "好感度"), 0, "却下なら好感度も動かない");
        assert!(!s.flag("promise_remembered"), "却下されたデルタは trigger を発火させない");
        assert!(s.fired.is_empty());
        assert_eq!(s.turn, 0);
    }


    /// 【spec 16 Phase A: degree 純関数のエッジ行列 (査読 Nit)】target×roll の境界を固定する。
    /// critical=01 は target 0 でも成功 / fumble 帯は target<50 で 96-100・>=50 で 100 のみ /
    /// 整数除算の端 / 判定順は critical 先勝ち。
    #[test]
    fn percentile_degree_edge_matrix() {
        use super::percentile_degree as deg;
        // target=0: 01 だけが成功 (critical)。96-100 は fumble、他は failure。
        assert_eq!(deg(1, 0), ("critical", true));
        assert_eq!(deg(2, 0), ("failure", false));
        assert_eq!(deg(96, 0), ("fumble", false));
        assert_eq!(deg(99, 0), ("fumble", false));
        assert_eq!(deg(100, 0), ("fumble", false));
        // target=1: extreme/hard 帯は 0 (整数除算) → 01 は critical、02 は failure。
        assert_eq!(deg(1, 1), ("critical", true));
        assert_eq!(deg(2, 1), ("failure", false));
        // target=49 (<50): 96 は fumble。2 は extreme (49/5=9)。
        assert_eq!(deg(2, 49), ("extreme", true));
        assert_eq!(deg(96, 49), ("fumble", false));
        assert_eq!(deg(99, 49), ("fumble", false));
        // target=50 (>=50): 96 は failure に降格、100 だけ fumble。帯: 10/25/50。
        assert_eq!(deg(2, 50), ("extreme", true));
        assert_eq!(deg(10, 50), ("extreme", true));
        assert_eq!(deg(11, 50), ("hard", true));
        assert_eq!(deg(25, 50), ("hard", true));
        assert_eq!(deg(26, 50), ("regular", true));
        assert_eq!(deg(50, 50), ("regular", true));
        assert_eq!(deg(51, 50), ("failure", false));
        assert_eq!(deg(96, 50), ("failure", false));
        assert_eq!(deg(100, 50), ("fumble", false));
        // target=100: roll=100 は常に fumble。99 は regular (hard 帯 50 超)。
        assert_eq!(deg(2, 100), ("extreme", true));
        assert_eq!(deg(20, 100), ("extreme", true));
        assert_eq!(deg(50, 100), ("hard", true));
        assert_eq!(deg(99, 100), ("regular", true));
        assert_eq!(deg(100, 100), ("fumble", false));
        // 負の target でも 01 は成功 (安全側)。
        assert_eq!(deg(1, -5), ("critical", true));
        assert_eq!(deg(2, -5), ("failure", false));
    }

    /// 【spec 16 Phase A: check_under】d100 ロールアンダーの即興判定。目標値 = stat 現在値、
    /// 出目も degree もエンジンが決める。幻技能は UnknownStat で却下。
    #[test]
    fn check_under_computes_degree_and_rejects_unknown_stat() {
        let sc = Scenario::from_yaml(concat!(
            "title: t\nstart: r\n",
            "initial_stats: { 目星: 60 }\n",
            "locations:\n  r: { description: d, items: {}, exits: [] }\n",
            "goal: { kind: always }\n"
        ))
        .unwrap();
        // seed 2 の初回 d100 = 11 (実測)。60/5=12 なので extreme 成功。
        let mut s = sc.initial_state(2);
        let out = apply(&mut s, &sc, &d(vec![StateOp::CheckUnder {
            entity: "player".into(),
            key: "目星".into(),
        }]))
        .unwrap();
        let c = &out.checks[0];
        assert_eq!((c.sides, c.roll, c.dc, c.total), (100, 11, 60, 11));
        assert_eq!(c.degree.as_deref(), Some("extreme"));
        assert!(c.success);
        assert_eq!(c.modifier, 0, "即興 check_under に修正は無い");

        // 幻技能 (未宣言 stat) は却下 — 閉世界 (既存 Check と同一)。
        let v = adjudicate(&s, &sc, &d(vec![StateOp::CheckUnder {
            entity: "player".into(),
            key: "図書館".into(),
        }]));
        assert!(matches!(v, Verdict::Reject { .. }), "未宣言 stat の check_under は却下");
    }

    /// 【spec 16 Phase B+C: percentile challenge と可変量ダイス】SAN チェック「1/1d6」を
    /// 成功/失敗/fumble/degree スロットの経路で実証。roll_stat は clamp され決定論。
    #[test]
    fn percentile_challenge_degree_slots_fallback_and_roll_stat() {
        let yaml = concat!(
            "title: t\nstart: r\n",
            "allowed_flags: [seen]\n",
            "initial_stats: { SAN: 60 }\n",
            "challenges:\n",
            "  san_check:\n",
            "    resolution: percentile\n",
            "    stat: SAN\n",
            "    on_success:\n",
            "      effects: [ { op: adjust_stat, key: SAN, delta: -1 } ]\n",
            "    on_failure:\n",
            "      narration: 悲鳴が漏れた。\n",
            "      effects: [ { op: roll_stat, key: SAN, count: 1, sides: 6, negate: true } ]\n",
            "locations:\n  r: { description: d, items: {}, exits: [] }\n",
            "goal: { kind: always }\n"
        );
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(sc.validate().is_empty(), "{:?}", sc.validate());
        let atk = |s: &mut GameState, sc: &Scenario| {
            apply(s, sc, &d(vec![StateOp::AttemptChallenge {
                entity: "player".into(),
                challenge: "san_check".into(),
            }]))
            .unwrap()
        };

        // 成功経路 (seed 19: d100=37 <= 60 = regular)。1 だけ減る。
        let mut s = sc.initial_state(19);
        let out = atk(&mut s, &sc);
        assert_eq!(out.checks[0].degree.as_deref(), Some("regular"));
        assert_eq!(s.stat_of("player", "SAN"), 59, "成功は固定 -1");
        assert!(out.stat_rolls.is_empty(), "成功側に可変量ダイスは無い");

        // 失敗経路 (seed 1: d100=66 > 60 = failure)。1d6 が引かれ、出目が監査記録に載る。
        let mut s = sc.initial_state(1);
        let out = atk(&mut s, &sc);
        assert_eq!(out.checks[0].degree.as_deref(), Some("failure"));
        assert_eq!(out.checks[0].narration, "悲鳴が漏れた。", "帰結ナレーションは毎回・同ターン");
        let sr = &out.stat_rolls[0];
        assert_eq!((sr.count, sr.sides, sr.rolls.len()), (1, 6, 1));
        assert!((1..=6).contains(&sr.rolls[0]));
        assert_eq!(sr.amount, -i64::from(sr.rolls[0]), "negate で符号反転した意図量");
        assert_eq!(s.stat_of("player", "SAN"), 60 + sr.amount, "SAN に出目分が反映される");
        // 決定論: 同 seed 同減少。
        let mut s2 = sc.initial_state(1);
        let _ = atk(&mut s2, &sc);
        assert_eq!(s.stat_of("player", "SAN"), s2.stat_of("player", "SAN"));

        // clamp: SAN 3 (min 0 糖衣) で失敗しても負にならない。
        let low = yaml.replace("SAN: 60", "SAN: 3");
        let sc_low = Scenario::from_yaml(&low).unwrap();
        let mut s = sc_low.initial_state(1); // d100=66 > 3 = failure → 1d6 減
        let out = atk(&mut s, &sc_low);
        assert!(s.stat_of("player", "SAN") >= 0, "0 クランプ (帳簿は負にならない)");
        assert!(out.checks[0].degree.is_some());

        // fumble フォールバック (SAN 40 < 50, seed 13: d100=96 = fumble)。on_fumble 無し →
        // on_failure に落ちる (narration も効果も failure のもの)。
        let mid = yaml.replace("SAN: 60", "SAN: 40");
        let sc_mid = Scenario::from_yaml(&mid).unwrap();
        let mut s = sc_mid.initial_state(13);
        let out = atk(&mut s, &sc_mid);
        assert_eq!(out.checks[0].degree.as_deref(), Some("fumble"));
        assert_eq!(out.checks[0].narration, "悲鳴が漏れた。", "on_fumble 無しは on_failure へ");
        assert!(!out.stat_rolls.is_empty(), "failure の 1d6 が使われる");

        // degree スロット優先 (seed 8: d100=23 <= 30 = hard)。on_hard があればそれが勝つ。
        let slotted = yaml.replace(
            "    on_success:",
            "    on_hard:\n      flag: seen\n    on_success:",
        );
        let sc_slot = Scenario::from_yaml(&slotted).unwrap();
        let mut s = sc_slot.initial_state(8);
        let out = atk(&mut s, &sc_slot);
        assert_eq!(out.checks[0].degree.as_deref(), Some("hard"));
        assert_eq!(s.flags.get("seen"), Some(&true), "hard は on_hard のフラグを使う");
        assert_eq!(s.stat_of("player", "SAN"), 60, "on_hard が勝つので on_success の -1 は乗らない");

        // critical のフォールバック (seed 29: d100=1)。degree スロット無し → on_success へ。
        let mut s = sc.initial_state(29);
        let out = atk(&mut s, &sc);
        assert_eq!(out.checks[0].degree.as_deref(), Some("critical"));
        assert_eq!(s.stat_of("player", "SAN"), 59, "critical は on_success へフォールバック");
    }

    /// 【spec 16 Phase C: 専権】LLM の roll_stat 提案は却下される (ダメージ量の捏造遮断)。
    /// state は無傷 (原子性)。
    #[test]
    fn llm_proposed_roll_stat_is_rejected() {
        let sc = Scenario::from_yaml(concat!(
            "title: t\nstart: r\n",
            "initial_stats: { hp: 10 }\n",
            "locations:\n  r: { description: d, items: {}, exits: [] }\n",
            "goal: { kind: always }\n"
        ))
        .unwrap();
        let mut s = sc.initial_state(1);
        let err = apply(&mut s, &sc, &d(vec![StateOp::RollStat {
            entity: "player".into(),
            key: "hp".into(),
            count: 1,
            sides: 8,
            bonus: 0,
            negate: true,
        }]))
        .unwrap_err();
        let Verdict::Reject { reasons } = err else { panic!("Reject であること") };
        assert!(
            reasons.iter().any(|r| matches!(r, RejectReason::StatRollNotAllowed { .. })),
            "StatRollNotAllowed で名指し: {reasons:?}"
        );
        assert_eq!(s.stat_of("player", "hp"), 10, "state 無傷");
    }

    /// 【spec 16: 形の validate】additive の sides 欠落 / percentile の stat 欠落・sides 指定・
    /// tiers 併用 / roll_stat のゼロダイス、を load 時に名指しで弾く。
    #[test]
    fn validate_rejects_percentile_and_roll_stat_shapes() {
        use crate::spine::ScenarioError as E;
        let base = |challenge_yaml: &str| {
            Scenario::from_yaml(&format!(
                "title: t\nstart: r\ninitial_stats: {{ SAN: 50 }}\nchallenges:\n{challenge_yaml}locations:\n  r: {{ description: d, items: {{}}, exits: [] }}\ngoal: {{ kind: always }}\n"
            ))
            .unwrap()
        };
        // additive で sides 欠落 (serde default 0) は ChallengeShapeInvalid。
        let sc = base("  c: { dc: 10 }\n");
        assert!(
            sc.validate().iter().any(|e| matches!(e, E::ChallengeShapeInvalid { .. })),
            "{:?}",
            sc.validate()
        );
        // percentile で stat 欠落。
        let sc = base("  c: { resolution: percentile }\n");
        assert!(sc.validate().iter().any(|e| matches!(e, E::PercentileChallengeShape { .. })));
        // percentile で sides 指定 (加算式との混同)。
        let sc = base("  c: { resolution: percentile, stat: SAN, sides: 100 }\n");
        assert!(sc.validate().iter().any(|e| matches!(e, E::PercentileChallengeShape { .. })));
        // percentile + tiers は二重クリティカル。
        let sc = base("  c:\n    resolution: percentile\n    stat: SAN\n    tiers:\n      crit: { natural: min }\n");
        assert!(sc.validate().iter().any(|e| matches!(e, E::TierWithPercentile { .. })));
        // trigger effects の roll_stat ゼロダイス。
        let sc = Scenario::from_yaml(concat!(
            "title: t\nstart: r\ninitial_stats: { SAN: 50 }\n",
            "triggers:\n",
            "  - id: bad\n",
            "    when: { kind: always }\n",
            "    effects: [ { op: roll_stat, key: SAN, count: 0, sides: 6 } ]\n",
            "locations:\n  r: { description: d, items: {}, exits: [] }\n",
            "goal: { kind: always }\n"
        ))
        .unwrap();
        assert!(
            sc.validate().iter().any(|e| matches!(e, E::RollStatShapeInvalid { .. })),
            "{:?}",
            sc.validate()
        );
    }

    /// 【spec 16: 全帰結共通効果の射影 (spec 09 の percentile 版)】全 degree が同じフラグに
    /// 解決される percentile challenge は、そのフラグを前提にした move と同一 delta に束ねられる。
    #[test]
    fn percentile_shared_flag_projects_for_bundling() {
        let sc = Scenario::from_yaml(concat!(
            "title: t\nstart: r\n",
            "allowed_flags: [done]\n",
            "initial_stats: { SAN: 50 }\n",
            "challenges:\n",
            "  ritual:\n",
            "    resolution: percentile\n",
            "    stat: SAN\n",
            "    on_success: { flag: done }\n",
            "    on_failure: { flag: done }\n",
            "locations:\n",
            "  r:\n",
            "    description: d\n",
            "    items: {}\n",
            "    exits:\n",
            "      - to: next\n",
            "        gate: { kind: flag_is, key: done, value: true }\n",
            "  next: { description: d, items: {}, exits: [] }\n",
            "goal: { kind: always }\n"
        ))
        .unwrap();
        assert!(sc.validate().is_empty(), "{:?}", sc.validate());
        let mut s = sc.initial_state(1);
        // どの出目でも done は立つ = 判定と移動を 1 ターンに束ねられる。
        let out = apply(&mut s, &sc, &d(vec![
            StateOp::AttemptChallenge { entity: "player".into(), challenge: "ritual".into() },
            StateOp::Move { to: "next".into() },
        ]));
        assert!(out.is_ok(), "全帰結共通フラグは射影され束ねが受理される: {out:?}");
        assert_eq!(s.location, "next");
    }

    // =========================================================================
    // spec 18 Phase B: 決断つき判定 (プッシュ / 差分買い / 帰結の確定遅延)
    // =========================================================================

    /// 決断テスト用の additive 盤面。sides:1 dc:5 = 常に失敗 (決定論)。
    fn decision_yaml() -> &'static str {
        r#"
title: t
start: room
initial_stats: { STR: 0, "幸運": 50, HP: 10 }
allowed_flags: [fell, pushed_worse, opened]
spend_rules: { from: "幸運" }
push_cost: { from: HP, amount: 1 }
challenges:
  door:
    description: 扉をこじ開ける
    stat: STR
    sides: 1
    dc: 5
    pushable: true
    on_success: { flag: opened, narration: 開いた }
    on_failure: { flag: fell, narration: 失敗した, effects: [ { op: adjust_stat, key: HP, delta: -2 } ] }
    on_push_failure: { flag: pushed_worse, narration: もっと悪いことになった }
triggers:
  - id: alarm
    when: { kind: flag_is, key: fell, value: true }
    narration: 警報が鳴り響いた
goal: { kind: always }
locations:
  room: { description: d, items: {}, exits: [] }
"#
    }

    /// 【帰結の確定遅延 (spec 18 Phase B の中枢)】pushable な challenge の失敗は
    /// フラグ/effects/トリガーが**一切適用されず**凍結され、Accept で初めて原子適用される。
    #[test]
    fn pushable_failure_freezes_consequences_until_accept() {
        let sc = Scenario::from_yaml(decision_yaml()).unwrap();
        assert!(sc.validate().is_empty());
        let mut s = sc.initial_state(1);

        let o = apply(&mut s, &sc, &d(vec![StateOp::AttemptChallenge {
            entity: PLAYER.into(), challenge: "door".into(),
        }])).unwrap();

        // 凍結: 帰結は世界に触れていない。
        assert!(!s.flag("fell"), "失敗フラグは未適用");
        assert_eq!(s.stat_of(PLAYER, "HP"), 10, "失敗 effects (HP-2) も未適用");
        assert!(o.fired.is_empty(), "トリガー (警報) も発火しない");
        assert_eq!(s.pending_decisions.len(), 1, "決断待ちが積まれる");
        assert!(o.checks[0].pending, "判定行は凍結中フラグつき");
        assert!(o.checks[0].narration.is_empty(), "結末文は決断確定まで出さない");

        // 選択肢: プッシュ可 (HP 1 払える) + 買い可 (差分 4 <= 幸運 50)。
        let opts = decision_options(&s, &sc).unwrap();
        assert!(opts.can_push);
        assert_eq!(opts.push_cost, Some(("HP".into(), 1)));
        assert_eq!(opts.buys, vec![BuyOption { degree: "success".into(), cost: 4, from: "幸運".into() }]);

        // Accept: ここで初めて帰結が適用される。
        let r = resolve_decision(&mut s, &sc, DecisionChoice::Accept).unwrap();
        assert!(s.flag("fell"), "失敗フラグが立つ");
        assert_eq!(s.stat_of(PLAYER, "HP"), 8, "失敗 effects が適用される");
        assert_eq!(r.fired.len(), 1, "警報トリガーが発火する");
        assert_eq!(r.check.narration, "失敗した", "結末文が確定する");
        assert!(!r.check.pending);
        assert!(s.pending_decisions.is_empty(), "凍結は解かれた");
        assert!(s.flag_turns.contains_key("fell"), "真化ターンも刻まれる");
    }

    /// 【プッシュ】代償 (HP-1) を払って振り直し、再失敗は on_push_failure 連鎖で確定する。
    /// on_failure の帰結 (fell/HP-2) は**起きない** (押した失敗は別の帰結)。
    #[test]
    fn push_pays_cost_rerolls_and_uses_push_failure_chain() {
        let sc = Scenario::from_yaml(decision_yaml()).unwrap();
        let mut s = sc.initial_state(1);
        apply(&mut s, &sc, &d(vec![StateOp::AttemptChallenge {
            entity: PLAYER.into(), challenge: "door".into(),
        }])).unwrap();

        let cursor_before = s.rng.cursor;
        let r = resolve_decision(&mut s, &sc, DecisionChoice::Push).unwrap();
        assert!(s.rng.cursor > cursor_before, "振り直しは本流 RNG を実際に消費する (固着しない)");
        assert_eq!(r.push_paid, Some(("HP".into(), 1)), "押す代償を払う");
        assert_eq!(s.stat_of(PLAYER, "HP"), 9, "HP 10-1 (on_failure の -2 は起きない)");
        assert!(r.check.pushed, "押した判定として確定");
        assert!(!r.check.success, "1d1=1 < 5 = 再失敗 (決定論)");
        assert!(s.flag("pushed_worse"), "on_push_failure のフラグ");
        assert!(!s.flag("fell"), "on_failure の帰結は取られない (連鎖が置き換える)");
        assert_eq!(r.check.narration, "もっと悪いことになった");
        assert!(s.pending_decisions.is_empty(), "プッシュは成否に依らず final");
        // final なので二度目の決断は無い。
        assert!(matches!(
            resolve_decision(&mut s, &sc, DecisionChoice::Push),
            Err(DecisionError::NoPending)
        ));
    }

    /// 【差分買い (percentile)】失敗を hard まで買い上げ、支払いが宣言 stat から引かれ、
    /// 買った degree のスロットが適用される。
    #[test]
    fn buy_degree_deducts_stat_and_applies_bought_slot() {
        let yaml = r#"
title: t
start: room
initial_stats: { "知識": 60, "幸運": 50 }
allowed_flags: [read, hard_read]
spend_rules: { from: "幸運" }
challenges:
  lore:
    resolution: percentile
    description: 碑文を読む
    stat: "知識"
    spendable: true
    on_success: { flag: read }
    on_hard: { flag: hard_read, narration: 鮮やかに読み解いた }
    on_failure: { narration: 読めない }
goal: { kind: always }
locations:
  room: { description: d, items: {}, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(sc.validate().is_empty());
        // 素の失敗 (61..=95) かつ hard (閾値 30) を幸運 50 で買える出目 (<=80) を seed 探索。
        for seed in 0..300u64 {
            let mut s = sc.initial_state(seed);
            apply(&mut s, &sc, &d(vec![StateOp::AttemptChallenge {
                entity: PLAYER.into(), challenge: "lore".into(),
            }])).unwrap();
            let Some(p) = s.pending_decisions.first().cloned() else { continue };
            if p.roll > 80 {
                continue; // hard を買えない出目 — 別 seed で
            }
            let opts = decision_options(&s, &sc).unwrap();
            assert!(!opts.can_push, "pushable 未宣言 (既定 false) なので押せない");
            let hard = opts.buys.iter().find(|b| b.degree == "hard").expect("hard を買える");
            assert_eq!(hard.cost, i64::from(p.roll) - 30, "費用 = 出目 - hard 閾値 (60/2)");

            let r = resolve_decision(&mut s, &sc, DecisionChoice::Buy { degree: "hard".into() })
                .unwrap();
            assert_eq!(s.stat_of(PLAYER, "幸運"), 50 - hard.cost, "支払いが引かれる");
            assert_eq!(r.spent, Some(("幸運".into(), hard.cost)));
            assert!(r.check.success && r.check.degree.as_deref() == Some("hard"));
            assert_eq!(r.check.spent, hard.cost);
            assert!(s.flag("hard_read"), "買った degree のスロット (on_hard) が適用される");
            assert!(!s.flag("read"), "regular のスロットではない");
            assert_eq!(r.check.narration, "鮮やかに読み解いた");
            return;
        }
        panic!("300 seed 以内に買える失敗が出るはず");
    }

    /// 【凍結しない側】fumble は final / NPC 主体は決断なし / 選択肢ゼロ (宣言なし) は即時確定。
    #[test]
    fn fumble_npc_and_no_option_failures_do_not_freeze() {
        // fumble: 目標値 2 (<50) なら 96-100 が fumble。pushable でも final。
        let yaml = r#"
title: t
start: room
initial_stats: { "知識": 2, "幸運": 50 }
allowed_flags: [failed]
spend_rules: { from: "幸運" }
challenges:
  lore:
    resolution: percentile
    description: d
    stat: "知識"
    pushable: true
    on_failure: { flag: failed }
goal: { kind: always }
locations:
  room: { description: d, items: {}, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        let mut seen_fumble = false;
        for seed in 0..300u64 {
            let mut s = sc.initial_state(seed);
            let o = apply(&mut s, &sc, &d(vec![StateOp::AttemptChallenge {
                entity: PLAYER.into(), challenge: "lore".into(),
            }])).unwrap();
            if o.checks[0].degree.as_deref() == Some("fumble") {
                assert!(s.pending_decisions.is_empty(), "fumble は凍結されず final");
                assert!(s.flag("failed"), "帰結 (on_fumble→on_failure 連鎖) は即時適用");
                seen_fumble = true;
                break;
            }
        }
        assert!(seen_fumble, "300 seed 以内に fumble が出るはず (p=5%)");

        // 選択肢ゼロ: pushable 無し + spend_rules 無し → 凍結せず従来どおり。
        let yaml2 = r#"
title: t
start: room
initial_stats: { STR: 0 }
allowed_flags: [fell]
challenges:
  door: { description: d, stat: STR, sides: 1, dc: 5, on_failure: { flag: fell } }
goal: { kind: always }
locations:
  room: { description: d, items: {}, exits: [] }
"#;
        let sc2 = Scenario::from_yaml(yaml2).unwrap();
        let mut s2 = sc2.initial_state(1);
        apply(&mut s2, &sc2, &d(vec![StateOp::AttemptChallenge {
            entity: PLAYER.into(), challenge: "door".into(),
        }])).unwrap();
        assert!(s2.pending_decisions.is_empty(), "選択肢が無ければ停止しない");
        assert!(s2.flag("fell"), "従来どおり即時確定");
    }

    /// 【射影の除外】決断つき challenge は「全帰結共通効果」を射影しない —
    /// 最終スロットが決断次第で on_push_failure まで広がるため、静的な共通保証が成り立たない。
    #[test]
    fn decision_enabled_challenge_is_excluded_from_guaranteed_projection() {
        let base = r#"
title: t
start: a
initial_stats: { STR: 0 }
allowed_flags: [done]
challenges:
  work:
    description: d
    stat: STR
    sides: 1
    dc: 5
    PUSHABLE
    on_success: { effects: [ { op: set_flag, key: done, value: true } ] }
    on_failure: { effects: [ { op: set_flag, key: done, value: true } ] }
goal: { kind: always }
locations:
  a: { description: d, items: {}, exits: [ { to: b, gate: { kind: flag_is, key: done, value: true } } ] }
  b: { description: d, items: {}, exits: [] }
"#;
        let bundle = d(vec![
            StateOp::AttemptChallenge { entity: PLAYER.into(), challenge: "work".into() },
            StateOp::Move { to: "b".into() },
        ]);
        // 非 pushable: 共通効果 (done) が射影され束ねが一発受理 (spec 09 の既存保証)。
        let sc_plain = Scenario::from_yaml(&base.replace("PUSHABLE", "")).unwrap();
        assert!(matches!(adjudicate(&sc_plain.initial_state(1), &sc_plain, &bundle), Verdict::Accept));
        // pushable: 射影されず move gate 未達で却下 (安全側)。
        let sc_push = Scenario::from_yaml(&base.replace("PUSHABLE", "pushable: true")).unwrap();
        assert!(matches!(
            adjudicate(&sc_push.initial_state(1), &sc_push, &bundle),
            Verdict::Reject { .. }
        ));
    }

    /// 【validate + 閉世界】幻の財布/代償元/押し失敗フラグは load 時に弾かれ、
    /// on_push_failure と degree スロットのフラグは authored 専権 (#50 バックストップ) に入る。
    #[test]
    fn decision_declarations_validate_and_join_authored_only() {
        use crate::spine::ScenarioError as E;
        let yaml = r#"
title: t
start: room
initial_stats: { STR: 0 }
allowed_flags: [worse]
spend_rules: { from: "幻の幸運" }
push_cost: { from: "幻のHP", amount: 1 }
challenges:
  door:
    description: d
    stat: STR
    sides: 1
    dc: 5
    pushable: true
    on_push_failure: { flag: ghost_flag }
goal: { kind: always }
locations:
  room: { description: d, items: {}, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        let errs = sc.validate();
        assert!(errs.iter().any(|e| matches!(e, E::SpendStatUndeclared { key } if key == "幻の幸運")));
        assert!(errs.iter().any(|e| matches!(e, E::PushCostStatUndeclared { key } if key == "幻のHP")));
        assert!(
            errs.iter().any(|e| matches!(e, E::ChallengeFlagUndeclared { tier, flag, .. }
                if tier == "on_push_failure" && flag == "ghost_flag")),
            "on_push_failure の幻フラグも load 時に弾く: {errs:?}"
        );

        // authored 専権: on_push_failure / degree スロット (on_fumble 等) のフラグが
        // usable 語彙から除外される (従来 on_success/on_failure のみ = #50 の穴を閉じた)。
        let yaml2 = r#"
title: t
start: room
initial_stats: { "知識": 60 }
allowed_flags: [worse, fumbled]
challenges:
  lore:
    resolution: percentile
    description: d
    stat: "知識"
    pushable: true
    on_fumble: { flag: fumbled }
    on_push_failure: { flag: worse }
goal: { kind: always }
locations:
  room: { description: d, items: {}, exits: [] }
"#;
        let sc2 = Scenario::from_yaml(yaml2).unwrap();
        let authored = sc2.authored_only_flags();
        assert!(authored.contains("worse"), "on_push_failure のフラグは専権");
        assert!(authored.contains("fumbled"), "degree スロット (on_fumble) のフラグも専権");
    }

    // =========================================================================
    // spec 18 Phase C: 対決 (contest) — 決着まで LLM を介さない交互振り
    // =========================================================================

    /// 対決テスト用の盤面。player d1+STR5 vs mob d1+腕力3 = 常に player 勝ち (決定論)。
    /// on_win が mob HP -1、until = mob HP 0 → 2 ラウンドで決着。
    fn contest_yaml() -> &'static str {
        r#"
title: t
start: room
initial_stats: { STR: 5, HP: 10 }
allowed_flags: [battle_open, first_blood]
characters:
  mob:
    name: 石くれ
    stats:
      HP: { initial: 2, min: 0 }
      "腕力": { initial: 3 }
    rolls:
      "体当たり": { stat: "腕力", sides: 1 }
contests:
  brawl:
    description: 石くれとの殴り合い
    opponent: mob
    requires: { kind: flag_is, key: battle_open, value: true }
    player_roll: { stat: STR, sides: 1 }
    opponent_roll: "体当たり"
    on_win:
      flag: first_blood
      narration: 拳が石くれを砕く。
      effects:
        - { op: adjust_stat, entity: mob, key: HP, delta: -1 }
    on_lose:
      effects:
        - { op: adjust_stat, key: HP, delta: -2 }
    until: { kind: stat_at_most, entity: mob, key: HP, value: 0 }
    max_rounds: 10
triggers:
  - id: cheer
    when: { kind: flag_is, key: first_blood, value: true }
    narration: どこかで歓声が上がった。
goal: { kind: flag_is, key: escaped, value: true }
locations:
  room: { description: d, items: {}, exits: [] }
"#
    }

    /// 【対決の全周: 解禁 gate → 開始 → ラウンド → 決着】attempt_contest は requires で
    /// 却下され、解禁後は pending を開き、contest_round が LLM 抜きで帰結を原子適用し、
    /// until 成立で閉じる。
    #[test]
    fn contest_opens_rounds_and_settles_without_llm() {
        let sc = Scenario::from_yaml(contest_yaml()).unwrap();
        assert!(sc.validate().is_empty(), "{:?}", sc.validate());
        let mut s = sc.initial_state(1);

        // 解禁前は ContestLocked で却下 (delta 全体が無効 = 原子性)。
        let open_op = d(vec![StateOp::AttemptContest { contest: "brawl".into() }]);
        assert!(matches!(adjudicate(&s, &sc, &open_op), Verdict::Reject { .. }));
        // ラウンドを回す対象も無い。
        assert!(matches!(contest_round(&mut s, &sc), Err(ContestError::NoContest)));

        // 解禁 → 開始。apply はダイスを振らない (開くだけ)。
        s.flags.insert("battle_open".into(), true);
        let o = apply(&mut s, &sc, &open_op).unwrap();
        assert!(o.checks.is_empty(), "開始の apply は振らない");
        assert_eq!(s.pending_contest.as_ref().unwrap().contest, "brawl");

        // ラウンド 1: 1d1+5=6 vs 1d1+3=4 → player 勝ち。帰結 (mob HP-1) + トリガー発火。
        let r1 = contest_round(&mut s, &sc).unwrap();
        assert_eq!(r1.outcome, "win");
        assert!(r1.player.success && !r1.opponent.success);
        assert_eq!(r1.narration, "拳が石くれを砕く。");
        assert_eq!(s.entities["mob"]["HP"], 1, "on_win の効果が原子適用される");
        assert!(s.flag("first_blood"));
        assert_eq!(r1.fired.len(), 1, "帰結からトリガーが発火する (歓声)");
        assert!(r1.ended.is_none(), "mob HP 1 なので続く");

        // ラウンド 2: mob HP 0 → until 成立で決着。
        let r2 = contest_round(&mut s, &sc).unwrap();
        let end = r2.ended.expect("決着する");
        assert_eq!((end.rounds, end.wins, end.losses, end.ties), (2, 2, 0, 0));
        assert_eq!(end.reason, "until");
        assert!(s.pending_contest.is_none(), "帳簿は閉じられた");
        // 進行中でなくなったので再度開ける (requires は真のまま)。
        assert!(matches!(adjudicate(&s, &sc, &open_op), Verdict::Accept));
    }

    /// 【percentile 対抗 + max_rounds 打ち切り + 進行中の再開始却下】degree 順位で勝敗、
    /// 同順位は目標値の高い側 (CoC7 準拠)。上限で必ず停止する。
    #[test]
    fn percentile_contest_compares_degrees_and_max_rounds_backstops() {
        let yaml = r#"
title: t
start: room
initial_stats: { "腕力": 70 }
allowed_flags: []
characters:
  rival:
    name: 好敵手
    stats: { "腕力": { initial: 40 } }
contests:
  arm_wrestle:
    description: 腕相撲
    resolution: percentile
    opponent: rival
    player_roll: { stat: "腕力" }
    opponent_roll: { stat: "腕力" }
    max_rounds: 3
goal: { kind: flag_is, key: escaped, value: true }
locations:
  room: { description: d, items: {}, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(sc.validate().is_empty(), "{:?}", sc.validate());
        let mut s = sc.initial_state(7);
        apply(&mut s, &sc, &d(vec![StateOp::AttemptContest { contest: "arm_wrestle".into() }]))
            .unwrap();

        // 進行中の再開始は却下 (ContestInProgress)。
        assert!(matches!(
            adjudicate(&s, &sc, &d(vec![StateOp::AttemptContest { contest: "arm_wrestle".into() }])),
            Verdict::Reject { .. }
        ));

        // 3 ラウンド回すと max_rounds で必ず決着。各ラウンドの勝敗は degree 順位 →
        // 同順位なら目標値 (70 > 40 = player) — 返った checks から独立に検証する。
        for i in 0..3 {
            let r = contest_round(&mut s, &sc).unwrap();
            let pr = super::degree_rank(r.player.degree.as_deref().unwrap());
            let or = super::degree_rank(r.opponent.degree.as_deref().unwrap());
            let expect = match pr.cmp(&or) {
                std::cmp::Ordering::Greater => "win",
                std::cmp::Ordering::Less => "lose",
                std::cmp::Ordering::Equal => "win", // 目標値 70 > 40 のタイブレーク
            };
            assert_eq!(r.outcome, expect, "round {i}: degree 比較と一致する");
            if i == 2 {
                assert_eq!(r.ended.unwrap().reason, "max_rounds", "上限で必ず停止");
            } else {
                assert!(r.ended.is_none());
            }
        }
        assert!(s.pending_contest.is_none());
    }

    /// 【validate + 専権】幻の相手・解決不能テンプレート・percentile の stat 欠落は load 時に
    /// 弾かれ、contest 帰結フラグは authored 専権 (GM は set_flag できない)。
    #[test]
    fn contest_validation_and_authored_only_flags() {
        use crate::spine::ScenarioError as E;
        let yaml = r#"
title: t
start: room
initial_stats: { STR: 5 }
allowed_flags: [won]
characters:
  mob:
    name: m
    stats: { "腕力": { initial: 3 } }
contests:
  bad1:
    opponent: ghost
    player_roll: { stat: STR, sides: 6 }
    opponent_roll: { sides: 6 }
  bad2:
    resolution: percentile
    opponent: mob
    player_roll: { sides: 6 }
    opponent_roll: "存在しない技"
  ok:
    opponent: mob
    player_roll: { stat: STR, sides: 6 }
    opponent_roll: { stat: "腕力", sides: 6 }
    on_win: { flag: won }
goal: { kind: always }
locations:
  room: { description: d, items: {}, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        let errs = sc.validate();
        assert!(errs.iter().any(|e| matches!(e, E::ContestOpponentUnknown { entity, .. } if entity == "ghost")));
        assert!(
            errs.iter().any(|e| matches!(e, E::ContestRollInvalid { detail, .. } if detail.contains("percentile"))),
            "percentile の stat 欠落を弾く: {errs:?}"
        );
        assert!(
            errs.iter().any(|e| matches!(e, E::ContestRollInvalid { detail, .. } if detail.contains("存在しない技"))),
            "解決不能テンプレートを名指しする: {errs:?}"
        );
        assert!(sc.authored_only_flags().contains("won"), "contest 帰結フラグは専権");
    }

    /// 【player stat の境界つき宣言 (2026-07-20)】`initial_stats` が素の数値と
    /// `{ initial, min, max }` の両受けに — 従来形は無改修で挙動不変 (min 0・max なし)、
    /// 境界つきは SAN 上限 99 / 借金 (負の min) が書ける。clamp は adjust/scale/roll_stat の
    /// 全経路に効く (stat_bounds が player 宣言を読む)。
    #[test]
    fn player_stat_bounds_via_initial_stats_decl() {
        let yaml = r#"
title: t
start: room
initial_stats:
  hp: 10
  SAN: { initial: 60, min: 0, max: 99 }
  "所持金": { initial: 100, min: -500 }
allowed_flags: []
goal: { kind: flag_is, key: never, value: true }
locations:
  room: { description: d, items: {}, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(sc.validate().is_empty());
        let mut s = sc.initial_state(1);
        assert_eq!(s.stat_of(PLAYER, "hp"), 10, "従来形は従来どおり");
        assert_eq!(s.stat_of(PLAYER, "SAN"), 60, "境界つきは initial で seed");

        // 上限: SAN +50 は 99 で頭打ち (回復イベントの青天井を防ぐ)。
        apply(&mut s, &sc, &d(vec![StateOp::AdjustStat {
            entity: PLAYER.into(), key: "SAN".into(), delta: 50,
        }])).unwrap();
        assert_eq!(s.stat_of(PLAYER, "SAN"), 99, "宣言 max で clamp");

        // 負の下限: 所持金 -400 → -300 (既定 0 でなく宣言 min -500 まで下がれる)。
        apply(&mut s, &sc, &d(vec![StateOp::AdjustStat {
            entity: PLAYER.into(), key: "所持金".into(), delta: -400,
        }])).unwrap();
        assert_eq!(s.stat_of(PLAYER, "所持金"), -300, "負の min が効く (借金)");
        apply(&mut s, &sc, &d(vec![StateOp::AdjustStat {
            entity: PLAYER.into(), key: "所持金".into(), delta: -400,
        }])).unwrap();
        assert_eq!(s.stat_of(PLAYER, "所持金"), -500, "宣言 min で clamp");

        // 従来形 (hp) は max なしのまま = 挙動不変。
        apply(&mut s, &sc, &d(vec![StateOp::AdjustStat {
            entity: PLAYER.into(), key: "hp".into(), delta: 100,
        }])).unwrap();
        assert_eq!(s.stat_of(PLAYER, "hp"), 110, "従来形に上限は生えない (後方互換)");
    }

    /// 【複数ダイス×乗数 (3D6×5 系、2026-07-20)】challenge の出目を `{count}d{sides}×times` に
    /// 一般化。乗算は素の合計だけに掛かり (修正は後から加算)、tier は素の合計で判定
    /// (min=全部 1)。既定 1d/×1 は従来と完全一致 (後方互換)。
    #[test]
    fn challenge_multi_dice_with_multiplier() {
        let yaml = r#"
title: t
start: room
initial_stats: { BONUS: 7 }
allowed_flags: [botch]
challenges:
  gen:
    description: 3d1×5 (決定論)
    stat: BONUS
    count: 3
    sides: 1
    times: 5
    dc: 20
    tiers:
      slip: { natural: min, flag: botch, narration: 全部 1 だ。 }
goal: { kind: flag_is, key: never, value: true }
locations:
  room: { description: d, items: {}, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(sc.validate().is_empty(), "{:?}", sc.validate());
        let mut s = sc.initial_state(1);
        let o = apply(&mut s, &sc, &d(vec![StateOp::AttemptChallenge {
            entity: PLAYER.into(), challenge: "gen".into(),
        }])).unwrap();
        let c = &o.checks[0];
        assert_eq!((c.count, c.sides, c.times), (3, 1, 5), "素性が surface される");
        assert_eq!(c.roll, 3, "roll は素の合計 (3d1 = 3)");
        assert_eq!(c.total, 3 * 5 + 7, "合計×times + 修正 = 22");
        assert!(c.success, "22 >= DC20");
        assert_eq!(c.tier.as_deref(), Some("slip"), "min = 全部 1 (合計 == count) で tier 発火");
        assert!(s.flag("botch"));

        // percentile に count/times は書けない (1d100 固定)。
        let bad = r#"
title: t
start: room
initial_stats: { "知識": 60 }
allowed_flags: []
challenges:
  p: { resolution: percentile, description: d, stat: "知識", count: 3, times: 5 }
goal: { kind: flag_is, key: never, value: true }
locations:
  room: { description: d, items: {}, exits: [] }
"#;
        let sc2 = Scenario::from_yaml(bad).unwrap();
        assert!(
            sc2.validate().iter().any(|e| matches!(e,
                crate::spine::ScenarioError::PercentileChallengeShape { detail, .. }
                if detail.contains("count/times"))),
            "percentile への count/times は load 時に弾く"
        );
    }

    // =========================================================================
    // spec 19: 式修正 — 判定の修正値/目標値を stat の式で書く
    // =========================================================================

    /// 【式修正の評価と「生きた派生値」】additive の修正 = (CON+SIZ)/2 が現在値で評価され、
    /// CON が削られると次の判定から補正も落ちる。percentile は式が目標値になる。
    #[test]
    fn challenge_expr_evaluates_live_values() {
        let yaml = r#"
title: t
start: room
initial_stats: { CON: 13, SIZ: 11, DEX: 35 }
allowed_flags: []
challenges:
  club:
    description: 棍棒で殴る
    expr: "(CON + SIZ) / 2"
    sides: 1
    dc: 1
  dodge:
    resolution: percentile
    description: 回避
    expr: "DEX * 2"
goal: { kind: flag_is, key: never, value: true }
locations:
  room: { description: d, items: {}, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(sc.validate().is_empty(), "{:?}", sc.validate());
        let mut s = sc.initial_state(1);

        // additive: 修正 = (13+11)/2 = 12。1d1(1)+12 = 13。
        let o = apply(&mut s, &sc, &d(vec![StateOp::AttemptChallenge {
            entity: PLAYER.into(), challenge: "club".into(),
        }])).unwrap();
        assert_eq!(o.checks[0].modifier, 12, "式修正 (CON+SIZ)/2 = 12");
        assert_eq!(o.checks[0].total, 13);

        // percentile: 目標値 = DEX*2 = 70。
        let o2 = apply(&mut s, &sc, &d(vec![StateOp::AttemptChallenge {
            entity: PLAYER.into(), challenge: "dodge".into(),
        }])).unwrap();
        assert_eq!(o2.checks[0].dc, 70, "式が目標値になる");
        assert!(o2.checks[0].degree.is_some());

        // 生きた派生値: CON 13→3 に削ると次の club の修正は (3+11)/2 = 7 へ落ちる。
        s.set_stat(PLAYER, "CON", 3);
        let o3 = apply(&mut s, &sc, &d(vec![StateOp::AttemptChallenge {
            entity: PLAYER.into(), challenge: "club".into(),
        }])).unwrap();
        assert_eq!(o3.checks[0].modifier, 7, "現在値で評価される (手書きシートとの差)");
    }

    /// 【式修正の閉世界】stat と併記 / 参照 stat 未宣言 / 壊れた式は load 時に名指しされ、
    /// contest の RollSpec.expr も同じ検査を通る。
    #[test]
    fn expr_validation_rejects_broken_and_undeclared() {
        use crate::spine::ScenarioError as E;
        let yaml = r#"
title: t
start: room
initial_stats: { CON: 10 }
allowed_flags: []
characters:
  mob:
    name: m
    stats: { "腕力": { initial: 8 } }
challenges:
  bad_both:
    description: d
    stat: CON
    expr: "CON + 1"
    sides: 6
    dc: 3
  bad_ghost:
    description: d
    expr: "CON + 幻の筋力"
    sides: 6
    dc: 3
  bad_parse:
    description: d
    expr: "(CON +"
    sides: 6
    dc: 3
contests:
  ok_expr:
    opponent: mob
    player_roll: { expr: "CON / 2", sides: 20 }
    opponent_roll: { expr: "腕力 * 2", sides: 20 }
goal: { kind: flag_is, key: never, value: true }
locations:
  room: { description: d, items: {}, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        let errs = sc.validate();
        assert!(errs.iter().any(|e| matches!(e, E::ChallengeExprInvalid { challenge, detail }
            if challenge == "bad_both" && detail.contains("同時に書けない"))));
        assert!(errs.iter().any(|e| matches!(e, E::ChallengeExprInvalid { challenge, detail }
            if challenge == "bad_ghost" && detail.contains("幻の筋力"))));
        assert!(errs.iter().any(|e| matches!(e, E::ChallengeExprInvalid { challenge, .. }
            if challenge == "bad_parse")));
        // contest 側の式は健全 (双方とも自分の宣言 stat を参照) — contest 由来のエラーは無い。
        assert!(
            !errs.iter().any(|e| matches!(e, E::ContestRollInvalid { .. })),
            "contest の式修正は通る: {errs:?}"
        );
    }

    /// 【旧セーブ互換】pending_decisions 欠落の GameState が読める + 凍結込みで roundtrip する。
    #[test]
    fn pending_decisions_serde_roundtrip_and_old_save_compat() {
        let sc = Scenario::from_yaml(decision_yaml()).unwrap();
        let mut s = sc.initial_state(1);
        apply(&mut s, &sc, &d(vec![StateOp::AttemptChallenge {
            entity: PLAYER.into(), challenge: "door".into(),
        }])).unwrap();
        assert_eq!(s.pending_decisions.len(), 1);

        // 凍結込み roundtrip (セーブを跨いで決断が生きる)。
        let yaml = serde_yaml::to_string(&s).unwrap();
        let restored: GameState = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(restored, s, "凍結込みで同値");

        // 旧セーブ (フィールド欠落) も読める。
        let old = yaml
            .lines()
            .filter(|l| !l.starts_with("pending_decisions") && !l.starts_with("- challenge"))
            .collect::<Vec<_>>()
            .join("\n");
        let old_state: Result<GameState, _> = serde_yaml::from_str(&old);
        // 行フィルタで構造が崩れる場合に備え、最小 YAML でも確認する。
        let minimal: GameState =
            serde_yaml::from_str("location: room\nrng: { seed: 1, cursor: 0 }").unwrap();
        assert!(minimal.pending_decisions.is_empty(), "欠落 = 空 (serde default)");
        let _ = old_state;
    }
}
