//! 式修正 (spec 19) — 判定の修正値/目標値を **stat の式** で書く authored 専権の演算。
//!
//! `(CON + SIZ) / 2` のような整数式を engine が**判定のたびに現在値で評価**する。
//! 手書きの派生値と違い、CON が削られれば補正も落ちる (生きたシート)。
//!
//! 北極星整合:
//! - **authored 専権**: 式を書けるのは scenario/character の YAML だけ。LLM は式を持てない
//!   (challenge/contest を「選ぶ」だけ — 既存の閉世界と同じ線)。
//! - **閉世界**: 式が参照する stat は判定主体の宣言済みキーのみ (load 時 validate + 裁定時検査)。
//! - **決定論**: 整数演算のみ。除算は 0 方向への切り捨て (CoC の端数切り捨て準拠)、
//!   ゼロ除算は 0 (載せ算の安全側 — validate が静的な /0 は弾く)。

use crate::state::GameState;
use std::collections::BTreeSet;

/// 式の AST。パースは載せ替え可能なよう文字列から都度行う (式は数十文字・判定は低頻度)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Num(i64),
    /// stat 参照 (判定主体の現在値を読む)。
    Stat(String),
    Add(Box<Expr>, Box<Expr>),
    Sub(Box<Expr>, Box<Expr>),
    Mul(Box<Expr>, Box<Expr>),
    Div(Box<Expr>, Box<Expr>),
    Neg(Box<Expr>),
}

impl Expr {
    /// 式を評価する (stat は `entity` の現在値)。ゼロ除算は 0 (安全側・validate が静的分は弾く)。
    pub fn eval(&self, state: &GameState, entity: &str) -> i64 {
        match self {
            Expr::Num(n) => *n,
            Expr::Stat(key) => state.stat_of(entity, key),
            Expr::Add(a, b) => a.eval(state, entity).wrapping_add(b.eval(state, entity)),
            Expr::Sub(a, b) => a.eval(state, entity).wrapping_sub(b.eval(state, entity)),
            Expr::Mul(a, b) => a.eval(state, entity).wrapping_mul(b.eval(state, entity)),
            Expr::Div(a, b) => {
                let d = b.eval(state, entity);
                if d == 0 { 0 } else { a.eval(state, entity) / d }
            }
            Expr::Neg(a) => -a.eval(state, entity),
        }
    }

    /// 式が参照する stat キーの集合 (閉世界検査の素)。
    pub fn stats(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut BTreeSet<String>) {
        match self {
            Expr::Num(_) => {}
            Expr::Stat(k) => {
                out.insert(k.clone());
            }
            Expr::Add(a, b) | Expr::Sub(a, b) | Expr::Mul(a, b) | Expr::Div(a, b) => {
                a.collect(out);
                b.collect(out);
            }
            Expr::Neg(a) => a.collect(out),
        }
    }
}

/// 式をパースする。文法 (これだけ):
/// ```text
/// expr   := term (('+'|'-') term)*
/// term   := factor (('*'|'/') factor)*
/// factor := 整数 | stat名 | '(' expr ')' | '-' factor
/// ```
/// stat 名は演算子・括弧・空白・数字始まり以外の連続文字 (日本語キー可)。
pub fn parse_expr(src: &str) -> Result<Expr, String> {
    let tokens = tokenize(src)?;
    let mut pos = 0;
    let e = parse_add(&tokens, &mut pos)?;
    if pos != tokens.len() {
        return Err(format!("式の途中に解釈できない残りがある: {src}"));
    }
    // 静的な /0 (リテラル除算) は load 時に落とす。stat が実行時に 0 のケースは eval が 0 に倒す。
    if has_literal_div_zero(&e) {
        return Err(format!("ゼロ除算を含む式: {src}"));
    }
    Ok(e)
}

fn has_literal_div_zero(e: &Expr) -> bool {
    match e {
        Expr::Div(a, b) => {
            matches!(**b, Expr::Num(0)) || has_literal_div_zero(a) || has_literal_div_zero(b)
        }
        Expr::Add(a, b) | Expr::Sub(a, b) | Expr::Mul(a, b) => {
            has_literal_div_zero(a) || has_literal_div_zero(b)
        }
        Expr::Neg(a) => has_literal_div_zero(a),
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Num(i64),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    LParen,
    RParen,
}

fn tokenize(src: &str) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' => i += 1,
            '+' => {
                out.push(Tok::Plus);
                i += 1;
            }
            '-' => {
                out.push(Tok::Minus);
                i += 1;
            }
            '*' | '×' => {
                out.push(Tok::Star);
                i += 1;
            }
            '/' | '÷' => {
                out.push(Tok::Slash);
                i += 1;
            }
            '(' | '（' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' | '）' => {
                out.push(Tok::RParen);
                i += 1;
            }
            '0'..='9' => {
                let start = i;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                let s: String = chars[start..i].iter().collect();
                out.push(Tok::Num(s.parse().map_err(|_| format!("数値が大きすぎる: {s}"))?));
            }
            _ => {
                // stat 名: 演算子/括弧/空白以外の連続文字 (日本語可・数字も 2 文字目以降は可)。
                let start = i;
                while i < chars.len()
                    && !matches!(
                        chars[i],
                        ' ' | '\t' | '+' | '-' | '*' | '×' | '/' | '÷' | '(' | '（' | ')' | '）'
                    )
                {
                    i += 1;
                }
                out.push(Tok::Ident(chars[start..i].iter().collect()));
            }
        }
    }
    if out.is_empty() {
        return Err("空の式".to_string());
    }
    Ok(out)
}

fn parse_add(t: &[Tok], pos: &mut usize) -> Result<Expr, String> {
    let mut left = parse_mul(t, pos)?;
    while *pos < t.len() {
        match t[*pos] {
            Tok::Plus => {
                *pos += 1;
                left = Expr::Add(Box::new(left), Box::new(parse_mul(t, pos)?));
            }
            Tok::Minus => {
                *pos += 1;
                left = Expr::Sub(Box::new(left), Box::new(parse_mul(t, pos)?));
            }
            _ => break,
        }
    }
    Ok(left)
}

fn parse_mul(t: &[Tok], pos: &mut usize) -> Result<Expr, String> {
    let mut left = parse_factor(t, pos)?;
    while *pos < t.len() {
        match t[*pos] {
            Tok::Star => {
                *pos += 1;
                left = Expr::Mul(Box::new(left), Box::new(parse_factor(t, pos)?));
            }
            Tok::Slash => {
                *pos += 1;
                left = Expr::Div(Box::new(left), Box::new(parse_factor(t, pos)?));
            }
            _ => break,
        }
    }
    Ok(left)
}

fn parse_factor(t: &[Tok], pos: &mut usize) -> Result<Expr, String> {
    let Some(tok) = t.get(*pos) else {
        return Err("式が途中で終わっている".to_string());
    };
    match tok {
        Tok::Num(n) => {
            *pos += 1;
            Ok(Expr::Num(*n))
        }
        Tok::Ident(name) => {
            *pos += 1;
            Ok(Expr::Stat(name.clone()))
        }
        Tok::Minus => {
            *pos += 1;
            Ok(Expr::Neg(Box::new(parse_factor(t, pos)?)))
        }
        Tok::LParen => {
            *pos += 1;
            let e = parse_add(t, pos)?;
            if t.get(*pos) != Some(&Tok::RParen) {
                return Err("閉じ括弧が無い".to_string());
            }
            *pos += 1;
            Ok(e)
        }
        other => Err(format!("式のこの位置に置けない要素: {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::GameState;

    fn state_with(stats: &[(&str, i64)]) -> GameState {
        let mut s = GameState::new("r", 1);
        for (k, v) in stats {
            s.set_stat("player", k, *v);
        }
        s
    }

    /// 【文法と評価】四則・括弧・優先順位・日本語 stat 名・全角演算子・切り捨て除算。
    #[test]
    fn parses_and_evaluates_coc_style_formulas() {
        let s = state_with(&[("CON", 13), ("SIZ", 12), ("DEX", 70), ("筋力", 9)]);
        let cases = [
            ("(CON + SIZ) / 2", 12),          // 25/2 = 12 (切り捨て = CoC 準拠)
            ("(CON+SIZ)/10", 2),              // 耐久力の式
            ("DEX * 2", 140),
            ("筋力 + 3", 12),                 // 日本語 stat 名
            ("（CON＿unknown + 2）", 2),      // 未宣言 stat は 0 (裁定/validate が事前に弾く)
            ("10 - CON / 13", 9),             // 優先順位: 10 - (13/13)
            ("-筋力 + 20", 11),               // 単項マイナス
            ("CON × 2 ÷ 4", 6),               // 全角演算子も受ける
        ];
        for (src, want) in cases {
            let e = parse_expr(src).unwrap_or_else(|err| panic!("{src}: {err}"));
            assert_eq!(e.eval(&s, "player"), want, "{src}");
        }
    }

    /// 【閉世界の素材と静的エラー】参照 stat の列挙 / 壊れた式・/0 は load 時に落とせる。
    #[test]
    fn collects_stats_and_rejects_broken_or_div_zero() {
        let e = parse_expr("(CON + SIZ) / 2 + 幸運").unwrap();
        let stats = e.stats();
        assert!(stats.contains("CON") && stats.contains("SIZ") && stats.contains("幸運"));
        assert_eq!(stats.len(), 3, "数値リテラルは含まない");

        for broken in ["", "CON +", "(CON", "CON / 0", "1 / (2 - 2)"] {
            // "1 / (2-2)" のような畳み込み前の /0 は実行時 0 に倒れる仕様だが、
            // リテラル直書きの "/ 0" は静的に弾く。
            let r = parse_expr(broken);
            if broken == "1 / (2 - 2)" {
                assert!(r.is_ok(), "静的検査はリテラル /0 のみ (実行時は 0 に倒す)");
            } else {
                assert!(r.is_err(), "{broken} は弾く");
            }
        }
    }
}
