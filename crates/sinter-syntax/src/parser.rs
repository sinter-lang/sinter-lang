//! The Sinter parser: turns `.snt` source into an [`crate::ast::Program`].
//!
//! Built on a `pest` PEG (`grammar/sinter.pest`). The PEG is the normative
//! surface-syntax spec; this module folds its parse tree into the typed AST.

use pest::iterators::Pair;
use pest::Parser as _;

use crate::ast::{
    Assign, AssignKind, BinOp, Dir, Expr, LetBinding, Module, Port, Pos, Program, Reg, Type,
    UnaryOp,
};

/// The `pest`-generated parser and `Rule` enum. Isolated in a submodule so the
/// macro-generated items are exempt from the crate's `missing_docs` lint.
mod generated {
    #![allow(missing_docs)]
    use pest_derive::Parser;

    #[derive(Parser)]
    #[grammar = "grammar/sinter.pest"]
    pub struct SinterParser;
}

use generated::{Rule, SinterParser};

/// An error produced while parsing `.snt` source.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// The source did not match the grammar.
    #[error("syntax error: {0}")]
    Syntax(String),
    /// A type annotation like `u8`/`i16` was malformed.
    #[error("invalid type `{0}` at line {1}")]
    InvalidType(String, usize),
    /// An integer literal could not be represented.
    #[error("invalid integer literal `{0}` at line {1}")]
    InvalidLiteral(String, usize),
}

/// Parse a complete `.snt` compilation unit.
///
/// # Errors
/// Returns [`ParseError`] if the source does not match the grammar or contains
/// a malformed type or literal.
pub fn parse_program(source: &str) -> Result<Program, ParseError> {
    let mut pairs = SinterParser::parse(Rule::program, source)
        .map_err(|e| ParseError::Syntax(e.to_string()))?;
    // `program` is the sole top-level pair.
    let program = pairs
        .next()
        .ok_or_else(|| ParseError::Syntax("empty input".into()))?;

    let mut modules = Vec::new();
    for pair in program.into_inner() {
        match pair.as_rule() {
            Rule::module => modules.push(parse_module(pair)?),
            Rule::EOI => {}
            other => return Err(unexpected(other, "program")),
        }
    }
    Ok(Program { modules })
}

fn pos_of(pair: &Pair<Rule>) -> Pos {
    let (line, col) = pair.line_col();
    Pos { line, col }
}

fn unexpected(rule: Rule, ctx: &str) -> ParseError {
    ParseError::Syntax(format!("unexpected {rule:?} while parsing {ctx}"))
}

fn parse_module(pair: Pair<Rule>) -> Result<Module, ParseError> {
    let pos = pos_of(&pair);
    let mut clock_domain = "sys".to_string();
    let mut name = String::new();
    let mut ports = Vec::new();
    let mut regs = Vec::new();
    let mut lets = Vec::new();
    let mut assigns = Vec::new();

    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::clock_attr => {
                // `#[clock(domain = NAME)]` — the single inner ident is NAME.
                let dom = inner
                    .into_inner()
                    .next()
                    .ok_or_else(|| ParseError::Syntax("malformed clock attribute".into()))?;
                clock_domain = dom.as_str().to_string();
            }
            Rule::ident => name = inner.as_str().to_string(),
            Rule::port_decl => ports.push(parse_port(inner)?),
            Rule::reg_decl => regs.push(parse_reg(inner)?),
            Rule::let_decl => lets.push(parse_let(inner)?),
            Rule::assign_stmt => assigns.push(parse_assign(inner)?),
            other => return Err(unexpected(other, "module body")),
        }
    }

    Ok(Module {
        name,
        clock_domain,
        ports,
        regs,
        lets,
        assigns,
        pos,
    })
}

fn parse_port(pair: Pair<Rule>) -> Result<Port, ParseError> {
    let pos = pos_of(&pair);
    let mut inner = pair.into_inner();
    let dir = match inner.next().map(|p| p.as_str()) {
        Some("in") => Dir::In,
        Some("out") => Dir::Out,
        _ => return Err(ParseError::Syntax("malformed port direction".into())),
    };
    let name = next_ident(&mut inner, "port name")?;
    let ty = parse_type(&next_pair(&mut inner, "port type")?)?;
    Ok(Port { dir, name, ty, pos })
}

fn parse_reg(pair: Pair<Rule>) -> Result<Reg, ParseError> {
    let pos = pos_of(&pair);
    let mut inner = pair.into_inner();
    let name = next_ident(&mut inner, "register name")?;
    let ty = parse_type(&next_pair(&mut inner, "register type")?)?;
    let init = parse_expr(next_pair(&mut inner, "register init")?)?;
    Ok(Reg {
        name,
        ty,
        init,
        pos,
    })
}

fn parse_let(pair: Pair<Rule>) -> Result<LetBinding, ParseError> {
    let pos = pos_of(&pair);
    let mut inner = pair.into_inner();
    let name = next_ident(&mut inner, "let name")?;
    // An optional type annotation precedes the value expression.
    let next = next_pair(&mut inner, "let value")?;
    let (ty, value_pair) = if next.as_rule() == Rule::ty {
        (
            Some(parse_type(&next)?),
            next_pair(&mut inner, "let value")?,
        )
    } else {
        (None, next)
    };
    let value = parse_expr(value_pair)?;
    Ok(LetBinding {
        name,
        ty,
        value,
        pos,
    })
}

fn parse_assign(pair: Pair<Rule>) -> Result<Assign, ParseError> {
    let pos = pos_of(&pair);
    let mut inner = pair.into_inner();
    let target = next_ident(&mut inner, "assignment target")?;
    let kind = match next_pair(&mut inner, "assignment operator")?.as_str() {
        "<=" => AssignKind::NextState,
        "=" => AssignKind::Drive,
        other => {
            return Err(ParseError::Syntax(format!(
                "unknown assignment operator `{other}`"
            )))
        }
    };
    let value = parse_expr(next_pair(&mut inner, "assignment value")?)?;
    Ok(Assign {
        target,
        kind,
        value,
        pos,
    })
}

fn parse_type(pair: &Pair<Rule>) -> Result<Type, ParseError> {
    let text = pair.as_str();
    let (line, _) = pair.line_col();
    let (kind, digits) = text.split_at(1);
    let width: u32 = digits
        .parse()
        .map_err(|_| ParseError::InvalidType(text.to_string(), line))?;
    if width == 0 {
        return Err(ParseError::InvalidType(text.to_string(), line));
    }
    match kind {
        "u" => Ok(Type::Uint(width)),
        "i" => Ok(Type::Int(width)),
        _ => Err(ParseError::InvalidType(text.to_string(), line)),
    }
}

// --- Expressions -----------------------------------------------------------

fn parse_expr(pair: Pair<Rule>) -> Result<Expr, ParseError> {
    match pair.as_rule() {
        Rule::expr => parse_expr(first_inner(pair, "expr")?),
        Rule::or_expr
        | Rule::xor_expr
        | Rule::and_expr
        | Rule::eq_expr
        | Rule::rel_expr
        | Rule::add_expr => fold_binary(pair),
        Rule::unary => parse_unary(pair),
        Rule::primary => parse_primary(pair),
        Rule::if_expr => parse_if(pair),
        Rule::literal => parse_literal(&pair),
        Rule::ident => Ok(Expr::Ident(pair.as_str().to_string())),
        other => Err(unexpected(other, "expression")),
    }
}

/// Fold a left-associative level: `operand (op operand)*`.
fn fold_binary(pair: Pair<Rule>) -> Result<Expr, ParseError> {
    let mut inner = pair.into_inner();
    let mut lhs = parse_expr(next_pair(&mut inner, "operand")?)?;
    while let Some(op_pair) = inner.next() {
        let op = bin_op(op_pair.as_str())?;
        let rhs = parse_expr(next_pair(&mut inner, "operand")?)?;
        lhs = Expr::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        };
    }
    Ok(lhs)
}

fn parse_unary(pair: Pair<Rule>) -> Result<Expr, ParseError> {
    let mut ops = Vec::new();
    let mut operand = None;
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::unary_op => ops.push(unary_op(inner.as_str())?),
            _ => operand = Some(parse_expr(inner)?),
        }
    }
    let mut expr = operand.ok_or_else(|| ParseError::Syntax("unary without operand".into()))?;
    // Apply prefix operators right-to-left.
    for op in ops.into_iter().rev() {
        expr = Expr::Unary {
            op,
            expr: Box::new(expr),
        };
    }
    Ok(expr)
}

fn parse_primary(pair: Pair<Rule>) -> Result<Expr, ParseError> {
    parse_expr(first_inner(pair, "primary")?)
}

fn parse_if(pair: Pair<Rule>) -> Result<Expr, ParseError> {
    let mut inner = pair.into_inner();
    let cond = parse_expr(next_pair(&mut inner, "if condition")?)?;
    let then = parse_block(next_pair(&mut inner, "if then-branch")?)?;
    let els_pair = next_pair(&mut inner, "if else-branch")?;
    let els = match els_pair.as_rule() {
        Rule::if_expr => parse_if(els_pair)?,
        Rule::block_expr => parse_block(els_pair)?,
        other => return Err(unexpected(other, "if else-branch")),
    };
    Ok(Expr::If {
        cond: Box::new(cond),
        then: Box::new(then),
        els: Box::new(els),
    })
}

fn parse_block(pair: Pair<Rule>) -> Result<Expr, ParseError> {
    parse_expr(first_inner(pair, "block")?)
}

fn parse_literal(pair: &Pair<Rule>) -> Result<Expr, ParseError> {
    let text = pair.as_str();
    let (line, _) = pair.line_col();
    let value = if let Some(hex) = text.strip_prefix("0x") {
        u128::from_str_radix(hex, 16)
    } else {
        text.parse::<u128>()
    }
    .map_err(|_| ParseError::InvalidLiteral(text.to_string(), line))?;
    Ok(Expr::Literal(value))
}

fn bin_op(text: &str) -> Result<BinOp, ParseError> {
    Ok(match text {
        "+" => BinOp::Add,
        "-" => BinOp::Sub,
        "&" => BinOp::And,
        "|" => BinOp::Or,
        "^" => BinOp::Xor,
        "==" => BinOp::Eq,
        "!=" => BinOp::Ne,
        "<" => BinOp::Lt,
        ">" => BinOp::Gt,
        "<=" => BinOp::Le,
        ">=" => BinOp::Ge,
        other => {
            return Err(ParseError::Syntax(format!(
                "unknown binary operator `{other}`"
            )))
        }
    })
}

fn unary_op(text: &str) -> Result<UnaryOp, ParseError> {
    Ok(match text {
        "!" => UnaryOp::Not,
        "~" => UnaryOp::BitNot,
        "-" => UnaryOp::Neg,
        other => {
            return Err(ParseError::Syntax(format!(
                "unknown unary operator `{other}`"
            )))
        }
    })
}

// --- Small pair helpers ----------------------------------------------------

fn first_inner<'a>(pair: Pair<'a, Rule>, ctx: &str) -> Result<Pair<'a, Rule>, ParseError> {
    pair.into_inner()
        .next()
        .ok_or_else(|| ParseError::Syntax(format!("missing child while parsing {ctx}")))
}

fn next_pair<'a>(
    inner: &mut pest::iterators::Pairs<'a, Rule>,
    ctx: &str,
) -> Result<Pair<'a, Rule>, ParseError> {
    inner
        .next()
        .ok_or_else(|| ParseError::Syntax(format!("missing {ctx}")))
}

fn next_ident(
    inner: &mut pest::iterators::Pairs<'_, Rule>,
    ctx: &str,
) -> Result<String, ParseError> {
    Ok(next_pair(inner, ctx)?.as_str().to_string())
}
