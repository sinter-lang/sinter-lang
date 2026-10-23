//! The Sinter parser: turns `.snt` source into an [`crate::ast::Program`].
//!
//! Built on a `pest` PEG (`grammar/sinter.pest`). The PEG is the normative
//! surface-syntax spec; this module folds its parse tree into the typed AST.

use std::cell::Cell;

use pest::iterators::Pair;
use pest::Parser as _;

use crate::ast::{
    Assign, AssignKind, Attr, AttrArg, AttrValue, BinOp, Block, CastMode, ChannelKind, ConstItem,
    Dir, EnumItem, EnumVariant, Expr, FieldInit, FnColor, Func, ImplItem, Item, LetBinding,
    MatchArm, ModItem, Module, Param, PathQualifier, Pattern, Port, Pos, Program, Reg, Stmt,
    StructField, StructItem, Type, TypeRef, UnaryOp, UseItem, UsePath, Visibility,
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
    /// A width-suffixed literal does not fit its suffix type (DESIGN §6.10).
    #[error("literal `{0}` does not fit in `{1}` at line {2}")]
    LiteralRange(String, Type, usize),
}

thread_local! {
    /// The file index stamped on every [`Pos`] built by the current parse (see
    /// [`parse_program_in`]). pest hands `pos_of` no user context, so the index
    /// rides in a thread-local scoped to one parse.
    static FILE: Cell<u32> = const { Cell::new(0) };
}

/// Parse a complete `.snt` compilation unit.
///
/// # Errors
/// Returns [`ParseError`] if the source does not match the grammar or contains
/// a malformed type or literal.
pub fn parse_program(source: &str) -> Result<Program, ParseError> {
    parse_program_in(source, 0)
}

/// Like [`parse_program`], but stamps every [`Pos`] with source-file index
/// `file`, so diagnostics in a multi-file project can name the file.
///
/// # Errors
/// Returns [`ParseError`] if the source does not match the grammar or contains
/// a malformed type or literal.
pub fn parse_program_in(source: &str, file: u32) -> Result<Program, ParseError> {
    let prev = FILE.replace(file);
    let result = parse_unit(source);
    FILE.set(prev);
    result
}

fn parse_unit(source: &str) -> Result<Program, ParseError> {
    let mut pairs = SinterParser::parse(Rule::program, source)
        .map_err(|e| ParseError::Syntax(e.to_string()))?;
    // `program` is the sole top-level pair.
    let program = pairs
        .next()
        .ok_or_else(|| ParseError::Syntax("empty input".into()))?;

    let mut modules = Vec::new();
    let mut items = Vec::new();
    for pair in program.into_inner() {
        match pair.as_rule() {
            Rule::module => modules.push(parse_module(pair)?),
            Rule::item => items.push(parse_item(pair)?),
            Rule::EOI => {}
            other => return Err(unexpected(other, "program")),
        }
    }
    Ok(Program { modules, items })
}

fn pos_of(pair: &Pair<Rule>) -> Pos {
    let (line, col) = pair.line_col();
    Pos {
        line,
        col,
        file: FILE.get(),
    }
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
            Rule::kw_module => {}
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
    // Skip the leading `reg` keyword token.
    next_pair(&mut inner, "`reg` keyword")?;
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
        // Functional surface (the legacy chain is left exactly as above).
        Rule::f_expr => parse_expr(first_inner(pair, "f_expr")?),
        Rule::f_concat
        | Rule::f_or
        | Rule::f_xor
        | Rule::f_and
        | Rule::f_eq
        | Rule::f_rel
        | Rule::f_shift
        | Rule::f_add
        | Rule::f_mul => fold_binary(pair),
        Rule::f_cast => parse_fcast(pair),
        // `f_unary` has the same `unary_op* ~ operand` shape as the legacy unary.
        Rule::f_unary => parse_unary(pair),
        Rule::f_postfix => parse_fpostfix(pair),
        Rule::f_primary => parse_expr(first_inner(pair, "f_primary")?),
        Rule::f_if => parse_fif(pair),
        Rule::f_block => parse_block_expr(pair),
        Rule::f_match => parse_fmatch(pair),
        Rule::f_some => Ok(Expr::Some(Box::new(parse_expr(first_inner(
            pair,
            "Some payload",
        )?)?))),
        Rule::f_none => Ok(Expr::None),
        Rule::f_struct_lit => parse_struct_lit(pair),
        Rule::f_path => parse_fpath(pair),
        Rule::f_group_or_tuple => parse_group_or_tuple(pair),
        Rule::f_array => parse_array(pair),
        Rule::f_array_repeat => {
            let mut inner = pair.into_inner();
            let value = parse_expr(next_pair(&mut inner, "repeated value")?)?;
            let len = parse_mem_depth(next_pair(&mut inner, "repeat length")?)?;
            Ok(Expr::ArrayRepeat {
                value: Box::new(value),
                len: Box::new(len),
            })
        }
        Rule::f_include_hex => {
            let s = first_inner(pair, "include_hex path")?;
            Ok(Expr::IncludeHex(unquote(s.as_str())))
        }
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

/// A literal expression. A width-suffixed literal (`63_u7`, `0x80_i8`) asserts
/// its type (DESIGN §6.10): it must fit, and it desugars to a `wrap` cast of the
/// bare literal to that type, so it carries the type into any context.
/// Decimal values must lie in the type's range; `0x` / `0b` values are bit
/// patterns and need only fit its width (`0x80_i8` is -128).
fn parse_literal(pair: &Pair<Rule>) -> Result<Expr, ParseError> {
    let value = literal_value(pair)?;
    let lit = Expr::Literal(value);
    let raw = pair.as_str();
    let Some(ty) = width_suffix(raw) else {
        return Ok(lit);
    };
    let w = ty.width();
    let pattern = raw.starts_with("0x") || raw.starts_with("0b");
    let limit_bits = if ty.is_signed() && !pattern { w - 1 } else { w };
    if w == 0 || (limit_bits < 128 && value >> limit_bits != 0) {
        let (line, _) = pair.line_col();
        return Err(ParseError::LiteralRange(raw.to_string(), ty, line));
    }
    Ok(Expr::Cast {
        expr: Box::new(lit),
        ty: TypeRef::Scalar(ty),
        mode: CastMode::Wrap,
    })
}

/// The type named by a literal's `_uN` / `_iN` width suffix, if it has one.
fn width_suffix(raw: &str) -> Option<Type> {
    let body = strip_width_suffix(raw);
    let suffix = raw.get(body.len() + 1..)?;
    let width: u32 = suffix.get(1..)?.parse().ok()?;
    match suffix.as_bytes().first()? {
        b'u' => Some(Type::Uint(width)),
        b'i' => Some(Type::Int(width)),
        _ => None,
    }
}

/// Decode a `literal` pair's text to its integer value. Strips an optional
/// Rust-style width suffix (`_u7`/`_i16`) and `_` digit separators, then parses
/// the `0x` / `0b` / decimal body. The suffix's asserted width is not yet
/// represented in the AST (DESIGN §6.10 — that is a semantic concern).
fn literal_value(pair: &Pair<Rule>) -> Result<u128, ParseError> {
    let raw = pair.as_str();
    let (line, _) = pair.line_col();
    let body = strip_width_suffix(raw);
    let cleaned: String = body.chars().filter(|&c| c != '_').collect();
    let parsed = if let Some(hex) = cleaned.strip_prefix("0x") {
        u128::from_str_radix(hex, 16)
    } else if let Some(bin) = cleaned.strip_prefix("0b") {
        u128::from_str_radix(bin, 2)
    } else {
        cleaned.parse::<u128>()
    };
    parsed.map_err(|_| ParseError::InvalidLiteral(raw.to_string(), line))
}

/// Return the literal body with any trailing `_u<digits>` / `_i<digits>` width
/// suffix removed. `u`/`i` are not digits in any supported base, so the first
/// such marker followed only by digits unambiguously begins the suffix.
fn strip_width_suffix(raw: &str) -> &str {
    for marker in ["_u", "_i"] {
        if let Some(p) = raw.find(marker) {
            let rest = &raw[p + marker.len()..];
            if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()) {
                return &raw[..p];
            }
        }
    }
    raw
}

fn bin_op(text: &str) -> Result<BinOp, ParseError> {
    Ok(match text {
        "+" => BinOp::Add,
        "-" => BinOp::Sub,
        "*" => BinOp::Mul,
        "<<" => BinOp::Shl,
        ">>" => BinOp::Shr,
        "@" => BinOp::Concat,
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

/// The text of a `string` token without its surrounding double quotes. No
/// escape processing in the MVP (a file path or message rarely needs one).
fn unquote(text: &str) -> String {
    text.strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(text)
        .to_string()
}

fn next_ident(
    inner: &mut pest::iterators::Pairs<'_, Rule>,
    ctx: &str,
) -> Result<String, ParseError> {
    Ok(next_pair(inner, ctx)?.as_str().to_string())
}

// --- Functional surface (DESIGN §2.1) --------------------------------------

fn parse_item(pair: Pair<Rule>) -> Result<Item, ParseError> {
    let inner = first_inner(pair, "item")?;
    match inner.as_rule() {
        Rule::const_item => Ok(Item::Const(parse_const(inner)?)),
        Rule::struct_item => Ok(Item::Struct(parse_struct(inner)?)),
        Rule::enum_item => Ok(Item::Enum(parse_enum(inner)?)),
        Rule::impl_item => Ok(Item::Impl(parse_impl(inner)?)),
        Rule::fn_item => Ok(Item::Func(parse_fn(inner)?)),
        Rule::mod_item => Ok(Item::Mod(parse_mod(inner)?)),
        Rule::use_item => Ok(Item::Use(parse_use(inner)?)),
        other => Err(unexpected(other, "item")),
    }
}

/// Consume a leading `vis` pair if present, returning the visibility (DESIGN §10).
fn take_vis(inner: &mut pest::iterators::Pairs<'_, Rule>) -> Visibility {
    if inner.peek().map(|p| p.as_rule()) == Some(Rule::vis) {
        inner.next();
        Visibility::Public
    } else {
        Visibility::Private
    }
}

fn parse_mod(pair: Pair<Rule>) -> Result<ModItem, ParseError> {
    let pos = pos_of(&pair);
    let mut vis = Visibility::Private;
    let mut name = String::new();
    let mut body = None;
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::vis => vis = Visibility::Public,
            Rule::kw_mod => {}
            Rule::ident => name = inner.as_str().to_string(),
            Rule::mod_inline => {
                body = Some(
                    inner
                        .into_inner()
                        .map(parse_item)
                        .collect::<Result<Vec<_>, _>>()?,
                );
            }
            other => return Err(unexpected(other, "mod")),
        }
    }
    Ok(ModItem {
        vis,
        name,
        body,
        pos,
    })
}

fn parse_use(pair: Pair<Rule>) -> Result<UseItem, ParseError> {
    let pos = pos_of(&pair);
    let mut vis = Visibility::Private;
    let mut path = None;
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::vis => vis = Visibility::Public,
            Rule::kw_use => {}
            Rule::use_path => path = Some(parse_use_path(inner)?),
            other => return Err(unexpected(other, "use")),
        }
    }
    let path = path.ok_or_else(|| ParseError::Syntax("`use` without a path".into()))?;
    Ok(UseItem { vis, path, pos })
}

/// Interpret a `use_path`'s raw segments into a leading qualifier, the remaining
/// segments, and a trailing `*` glob flag. `root` / `self` / `super` are only
/// qualifiers in leading position; `*` is only valid as the final segment.
fn parse_use_path(pair: Pair<Rule>) -> Result<UsePath, ParseError> {
    let raw: Vec<String> = pair.into_inner().map(|p| p.as_str().to_string()).collect();
    let mut idx = 0;
    let qualifier = match raw.first().map(String::as_str) {
        Some("root") => {
            idx = 1;
            PathQualifier::Root
        }
        Some("self") => {
            idx = 1;
            PathQualifier::SelfMod
        }
        Some("super") => {
            let mut n = 0;
            while raw.get(idx).map(String::as_str) == Some("super") {
                n += 1;
                idx += 1;
            }
            PathQualifier::Super(n)
        }
        _ => PathQualifier::None,
    };
    let mut segments: Vec<String> = raw[idx..].to_vec();
    let glob = segments.last().map(String::as_str) == Some("*");
    if glob {
        segments.pop();
    }
    if segments.iter().any(|s| s == "*") {
        return Err(ParseError::Syntax(
            "`*` may only appear as the final segment of a `use` path".into(),
        ));
    }
    if segments
        .iter()
        .any(|s| matches!(s.as_str(), "root" | "self" | "super"))
    {
        return Err(ParseError::Syntax(
            "`root` / `self` / `super` may only lead a `use` path".into(),
        ));
    }
    Ok(UsePath {
        qualifier,
        segments,
        glob,
    })
}

fn parse_const(pair: Pair<Rule>) -> Result<ConstItem, ParseError> {
    let pos = pos_of(&pair);
    let mut inner = pair.into_inner();
    let vis = take_vis(&mut inner);
    let name = next_ident(&mut inner, "const name")?;
    let ty = parse_typeref(next_pair(&mut inner, "const type")?)?;
    let value = parse_expr(next_pair(&mut inner, "const value")?)?;
    Ok(ConstItem {
        vis,
        name,
        ty,
        value,
        pos,
    })
}

fn parse_struct(pair: Pair<Rule>) -> Result<StructItem, ParseError> {
    let pos = pos_of(&pair);
    let mut attrs = Vec::new();
    let mut vis = Visibility::Private;
    let mut name = String::new();
    let mut fields = Vec::new();
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::attr => attrs.push(parse_attr(inner)?),
            Rule::vis => vis = Visibility::Public,
            Rule::ident => name = inner.as_str().to_string(),
            Rule::struct_field => fields.push(parse_struct_field(inner)?),
            other => return Err(unexpected(other, "struct")),
        }
    }
    Ok(StructItem {
        attrs,
        vis,
        name,
        fields,
        pos,
    })
}

fn parse_struct_field(pair: Pair<Rule>) -> Result<StructField, ParseError> {
    let pos = pos_of(&pair);
    let mut inner = pair.into_inner();
    let vis = take_vis(&mut inner);
    let name = next_ident(&mut inner, "field name")?;
    let ty = parse_typeref(next_pair(&mut inner, "field type")?)?;
    let default = match inner.next() {
        Some(p) => Some(parse_expr(p)?),
        None => None,
    };
    Ok(StructField {
        vis,
        name,
        ty,
        default,
        pos,
    })
}

fn parse_enum(pair: Pair<Rule>) -> Result<EnumItem, ParseError> {
    let pos = pos_of(&pair);
    let mut attrs = Vec::new();
    let mut vis = Visibility::Private;
    let mut name = String::new();
    let mut variants = Vec::new();
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::attr => attrs.push(parse_attr(inner)?),
            Rule::vis => vis = Visibility::Public,
            Rule::ident => name = inner.as_str().to_string(),
            Rule::enum_variant => variants.push(parse_enum_variant(inner)?),
            other => return Err(unexpected(other, "enum")),
        }
    }
    Ok(EnumItem {
        attrs,
        vis,
        name,
        variants,
        pos,
    })
}

fn parse_enum_variant(pair: Pair<Rule>) -> Result<EnumVariant, ParseError> {
    let pos = pos_of(&pair);
    let mut inner = pair.into_inner();
    let name = next_ident(&mut inner, "variant name")?;
    let discriminant = match inner.next() {
        Some(p) => Some(parse_expr(p)?),
        None => None,
    };
    Ok(EnumVariant {
        name,
        discriminant,
        pos,
    })
}

fn parse_impl(pair: Pair<Rule>) -> Result<ImplItem, ParseError> {
    let pos = pos_of(&pair);
    let mut type_name = String::new();
    let mut methods = Vec::new();
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::ident => type_name = inner.as_str().to_string(),
            Rule::fn_item => methods.push(parse_fn(inner)?),
            other => return Err(unexpected(other, "impl")),
        }
    }
    Ok(ImplItem {
        type_name,
        methods,
        pos,
    })
}

fn parse_fn(pair: Pair<Rule>) -> Result<Func, ParseError> {
    let pos = pos_of(&pair);
    let mut attrs = Vec::new();
    let mut vis = Visibility::Private;
    let mut color = FnColor::Comb;
    let mut name = String::new();
    let mut params = Vec::new();
    let mut ret = None;
    let mut body = None;
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::attr => attrs.push(parse_attr(inner)?),
            Rule::vis => vis = Visibility::Public,
            Rule::fn_color => {
                color = match inner.as_str() {
                    "pipeline" => FnColor::Pipeline,
                    "multiplex" => FnColor::Multiplex,
                    other => {
                        return Err(ParseError::Syntax(format!(
                            "unknown function color `{other}`"
                        )))
                    }
                };
            }
            Rule::ident => name = inner.as_str().to_string(),
            Rule::param => params.push(parse_param(inner)?),
            Rule::f_typeref => ret = Some(parse_typeref(inner)?),
            Rule::f_block => body = Some(parse_fblock(inner)?),
            other => return Err(unexpected(other, "function")),
        }
    }
    let ret = ret.ok_or_else(|| ParseError::Syntax("function has no return type".into()))?;
    let body = body.ok_or_else(|| ParseError::Syntax("function has no body".into()))?;
    Ok(Func {
        attrs,
        vis,
        color,
        name,
        params,
        ret,
        body,
        pos,
    })
}

fn parse_param(pair: Pair<Rule>) -> Result<Param, ParseError> {
    let pos = pos_of(&pair);
    let inner = first_inner(pair, "parameter")?;
    match inner.as_rule() {
        Rule::self_param => Ok(Param::SelfRef { pos }),
        Rule::typed_param => {
            let mut it = inner.into_inner();
            let name = next_ident(&mut it, "parameter name")?;
            let ty = parse_typeref(next_pair(&mut it, "parameter type")?)?;
            Ok(Param::Typed { name, ty, pos })
        }
        other => Err(unexpected(other, "parameter")),
    }
}

fn parse_attr(pair: Pair<Rule>) -> Result<Attr, ParseError> {
    let pos = pos_of(&pair);
    let mut name = String::new();
    let mut args = Vec::new();
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::ident => name = inner.as_str().to_string(),
            Rule::attr_args => {
                for arg in inner.into_inner() {
                    args.push(parse_attr_arg(arg)?);
                }
            }
            other => return Err(unexpected(other, "attribute")),
        }
    }
    Ok(Attr { name, args, pos })
}

fn parse_attr_arg(pair: Pair<Rule>) -> Result<AttrArg, ParseError> {
    let mut inner = pair.into_inner();
    let key = next_ident(&mut inner, "attribute argument")?;
    let value = match inner.next() {
        Some(val) => {
            let v = first_inner(val, "attribute value")?;
            Some(match v.as_rule() {
                Rule::ident => AttrValue::Ident(v.as_str().to_string()),
                Rule::literal => AttrValue::Literal(literal_value(&v)?),
                Rule::string => AttrValue::Str(unquote(v.as_str())),
                other => return Err(unexpected(other, "attribute value")),
            })
        }
        None => None,
    };
    Ok(AttrArg { key, value })
}

fn parse_fblock(pair: Pair<Rule>) -> Result<Block, ParseError> {
    let pos = pos_of(&pair);
    let mut stmts = Vec::new();
    let mut tail = None;
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::f_let => stmts.push(parse_flet(inner)?),
            Rule::f_yield => stmts.push(parse_fyield(inner)?),
            Rule::f_assert => stmts.push(parse_fassert(inner)?),
            Rule::f_expr => tail = Some(parse_expr(inner)?),
            other => return Err(unexpected(other, "block")),
        }
    }
    let tail = tail.ok_or_else(|| ParseError::Syntax("block has no tail expression".into()))?;
    Ok(Block {
        stmts,
        tail: Box::new(tail),
        pos,
    })
}

fn parse_block_expr(pair: Pair<Rule>) -> Result<Expr, ParseError> {
    Ok(Expr::Block(parse_fblock(pair)?))
}

fn parse_fyield(pair: Pair<Rule>) -> Result<Stmt, ParseError> {
    let pos = pos_of(&pair);
    let mut value = None;
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::kw_yield => {}
            Rule::f_expr => value = Some(parse_expr(inner)?),
            other => return Err(unexpected(other, "yield")),
        }
    }
    Ok(Stmt::Yield { value, pos })
}

fn parse_fassert(pair: Pair<Rule>) -> Result<Stmt, ParseError> {
    let pos = pos_of(&pair);
    let mut inner = pair.into_inner();
    // The atomic keyword token (`assert!` or `debug_assert!`) leads the rule.
    let kw = next_pair(&mut inner, "assert keyword")?;
    let debug = kw.as_str().starts_with("debug_");
    let cond = parse_expr(next_pair(&mut inner, "assert condition")?)?;
    let msg = unquote(next_pair(&mut inner, "assert message")?.as_str());
    Ok(Stmt::Assert {
        debug,
        cond,
        msg,
        pos,
    })
}

fn parse_flet(pair: Pair<Rule>) -> Result<Stmt, ParseError> {
    let pos = pos_of(&pair);
    let mut inner = pair.into_inner();
    let pattern = parse_pattern(next_pair(&mut inner, "let pattern")?)?;
    let next = next_pair(&mut inner, "let value")?;
    let (ty, value_pair) = if next.as_rule() == Rule::f_typeref {
        (
            Some(parse_typeref(next)?),
            next_pair(&mut inner, "let value")?,
        )
    } else {
        (None, next)
    };
    let value = parse_expr(value_pair)?;
    Ok(Stmt::Let {
        pattern,
        ty,
        value,
        pos,
    })
}

fn parse_pattern(pair: Pair<Rule>) -> Result<Pattern, ParseError> {
    let inner = first_inner(pair, "pattern")?;
    match inner.as_rule() {
        Rule::f_tuple_pat => Ok(Pattern::Tuple(
            inner
                .into_inner()
                .map(parse_pattern)
                .collect::<Result<Vec<_>, _>>()?,
        )),
        Rule::f_some_pat => Ok(Pattern::Some(Box::new(parse_pattern(first_inner(
            inner,
            "Some pattern",
        )?)?))),
        Rule::f_none_pat => Ok(Pattern::None),
        Rule::f_wildcard => Ok(Pattern::Wildcard),
        Rule::f_lit_pat => Ok(Pattern::Literal(literal_value(&first_inner(
            inner,
            "literal pattern",
        )?)?)),
        Rule::f_path_pat => Ok(Pattern::Path(
            inner.into_inner().map(|p| p.as_str().to_string()).collect(),
        )),
        Rule::f_binding => Ok(Pattern::Binding(
            first_inner(inner, "binding")?.as_str().to_string(),
        )),
        other => Err(unexpected(other, "pattern")),
    }
}

fn parse_typeref(pair: Pair<Rule>) -> Result<TypeRef, ParseError> {
    let inner = first_inner(pair, "type")?;
    match inner.as_rule() {
        Rule::f_array_ty => {
            let mut it = inner.into_inner();
            let elem = parse_typeref(next_pair(&mut it, "array element type")?)?;
            let len = parse_expr(next_pair(&mut it, "array length")?)?;
            Ok(TypeRef::Array {
                elem: Box::new(elem),
                len: Box::new(len),
            })
        }
        Rule::f_tuple_ty => Ok(TypeRef::Tuple(
            inner
                .into_inner()
                .map(parse_typeref)
                .collect::<Result<Vec<_>, _>>()?,
        )),
        Rule::f_mem_ty | Rule::f_rom_ty => {
            // `mem<T, N>` (1R1W) / `rom<T, N>` (read-only: 1R, 0W). Both reuse the
            // memory type; the port counts distinguish them (and leave room for
            // `mem<T, N, R, W>` later).
            let write_ports = u32::from(inner.as_rule() == Rule::f_mem_ty);
            let mut it = inner.into_inner();
            let elem = parse_typeref(next_pair(&mut it, "memory element type")?)?;
            let depth = parse_mem_depth(next_pair(&mut it, "memory depth")?)?;
            Ok(TypeRef::Mem {
                elem: Box::new(elem),
                depth: Box::new(depth),
                read_ports: 1,
                write_ports,
            })
        }
        Rule::f_fifo_ty | Rule::f_ring_ty => {
            // `fifo<T, N>` / `ring<T, N>` (DESIGN §6.11 / §9.8) — the same
            // `<elem, depth>` shape as `mem`, differing only in loss policy; a
            // source-to-source desugar lowers both later.
            let kind = if inner.as_rule() == Rule::f_fifo_ty {
                ChannelKind::Fifo
            } else {
                ChannelKind::Ring
            };
            let mut it = inner.into_inner();
            let elem = parse_typeref(next_pair(&mut it, "channel element type")?)?;
            let depth = parse_mem_depth(next_pair(&mut it, "channel capacity")?)?;
            Ok(TypeRef::Channel {
                kind,
                elem: Box::new(elem),
                depth: Box::new(depth),
            })
        }
        Rule::f_flow_ty => {
            // `flow<T>` (DESIGN §6.11 / §9.8) — depth-1 newest-wins, so source
            // names no capacity and we synthesize the `1` the type implies.
            let elem = parse_typeref(first_inner(inner, "flow element type")?)?;
            Ok(TypeRef::Channel {
                kind: ChannelKind::Flow,
                elem: Box::new(elem),
                depth: Box::new(Expr::Literal(1)),
            })
        }
        Rule::f_scalar_ty => parse_scalar_typeref(&inner),
        Rule::f_option_ty => Ok(TypeRef::Option(Box::new(parse_typeref(first_inner(
            inner,
            "Option type argument",
        )?)?))),
        Rule::ident => Ok(TypeRef::Named(inner.as_str().to_string())),
        other => Err(unexpected(other, "type")),
    }
}

/// Parse a `f_mem_depth` token: a constant memory depth (a literal or a `const`
/// name). Restricted to these forms so the closing `>` of `mem<T, N>` is never
/// swallowed by a relational operator in the full expression grammar.
fn parse_mem_depth(pair: Pair<Rule>) -> Result<Expr, ParseError> {
    let inner = first_inner(pair, "memory depth")?;
    match inner.as_rule() {
        Rule::literal => Ok(Expr::Literal(literal_value(&inner)?)),
        Rule::ident => Ok(Expr::Ident(inner.as_str().to_string())),
        other => Err(unexpected(other, "memory depth")),
    }
}

/// Parse a `f_scalar_ty` token: `uN` / `iN`, or fixed-point `uI.F` / `iI.F`.
fn parse_scalar_typeref(pair: &Pair<Rule>) -> Result<TypeRef, ParseError> {
    let text = pair.as_str();
    let (line, _) = pair.line_col();
    let invalid = || ParseError::InvalidType(text.to_string(), line);
    let signed = match text.as_bytes().first() {
        Some(b'u') => false,
        Some(b'i') => true,
        _ => return Err(invalid()),
    };
    let rest = &text[1..];
    if let Some((int_s, frac_s)) = rest.split_once('.') {
        let int_bits: u32 = int_s.parse().map_err(|_| invalid())?;
        let frac_bits: u32 = frac_s.parse().map_err(|_| invalid())?;
        if int_bits == 0 && frac_bits == 0 {
            return Err(invalid());
        }
        Ok(TypeRef::Fixed {
            signed,
            int_bits,
            frac_bits,
        })
    } else {
        let width: u32 = rest.parse().map_err(|_| invalid())?;
        if width == 0 {
            return Err(invalid());
        }
        Ok(TypeRef::Scalar(if signed {
            Type::Int(width)
        } else {
            Type::Uint(width)
        }))
    }
}

fn parse_fcast(pair: Pair<Rule>) -> Result<Expr, ParseError> {
    let mut inner = pair.into_inner();
    let expr = parse_expr(next_pair(&mut inner, "cast operand")?)?;
    match inner.next() {
        Some(ty_pair) => {
            let ty = parse_typeref(ty_pair)?;
            let mode = parse_cast_mode(&next_pair(&mut inner, "cast mode")?)?;
            Ok(Expr::Cast {
                expr: Box::new(expr),
                ty,
                mode,
            })
        }
        None => Ok(expr),
    }
}

fn parse_cast_mode(pair: &Pair<Rule>) -> Result<CastMode, ParseError> {
    Ok(match pair.as_str() {
        "sat" => CastMode::Sat,
        "wrap" => CastMode::Wrap,
        "trunc" => CastMode::Trunc,
        "round" => CastMode::Round,
        other => return Err(ParseError::Syntax(format!("unknown cast mode `{other}`"))),
    })
}

fn parse_fpostfix(pair: Pair<Rule>) -> Result<Expr, ParseError> {
    let mut inner = pair.into_inner();
    let mut expr = parse_expr(next_pair(&mut inner, "postfix base")?)?;
    for op in inner {
        expr = match op.as_rule() {
            Rule::f_dot => {
                let mut di = op.into_inner();
                let name = next_ident(&mut di, "field or method name")?;
                match di.next() {
                    Some(args_pair) => Expr::MethodCall {
                        receiver: Box::new(expr),
                        method: name,
                        args: parse_args(args_pair)?,
                    },
                    None => Expr::Field {
                        base: Box::new(expr),
                        field: name,
                    },
                }
            }
            Rule::f_call => Expr::Call {
                func: Box::new(expr),
                args: parse_args(first_inner(op, "call arguments")?)?,
            },
            Rule::f_index => {
                // `[i]` is an array index; `[hi:lo]` (a second bound) is a slice.
                let mut it = op.into_inner();
                let first = parse_expr(next_pair(&mut it, "index")?)?;
                match it.next() {
                    Some(second) => Expr::Slice {
                        base: Box::new(expr),
                        hi: Box::new(first),
                        lo: Box::new(parse_expr(second)?),
                    },
                    None => Expr::Index {
                        base: Box::new(expr),
                        index: Box::new(first),
                    },
                }
            }
            Rule::f_try => Expr::Try(Box::new(expr)),
            other => return Err(unexpected(other, "postfix")),
        };
    }
    Ok(expr)
}

fn parse_args(pair: Pair<Rule>) -> Result<Vec<Expr>, ParseError> {
    pair.into_inner().map(parse_expr).collect()
}

fn parse_fif(pair: Pair<Rule>) -> Result<Expr, ParseError> {
    let mut inner = pair.into_inner();
    let cond = parse_expr(next_pair(&mut inner, "if condition")?)?;
    let then = parse_block_expr(next_pair(&mut inner, "if then-branch")?)?;
    let els_pair = next_pair(&mut inner, "if else-branch")?;
    let els = match els_pair.as_rule() {
        Rule::f_if => parse_fif(els_pair)?,
        Rule::f_block => parse_block_expr(els_pair)?,
        other => return Err(unexpected(other, "if else-branch")),
    };
    Ok(Expr::If {
        cond: Box::new(cond),
        then: Box::new(then),
        els: Box::new(els),
    })
}

fn parse_fmatch(pair: Pair<Rule>) -> Result<Expr, ParseError> {
    let mut inner = pair.into_inner();
    let scrutinee = parse_expr(next_pair(&mut inner, "match scrutinee")?)?;
    let mut arms = Vec::new();
    for arm in inner {
        let mut ai = arm.into_inner();
        let pattern = parse_pattern(next_pair(&mut ai, "match pattern")?)?;
        let body = parse_expr(next_pair(&mut ai, "match arm body")?)?;
        arms.push(MatchArm { pattern, body });
    }
    Ok(Expr::Match {
        scrutinee: Box::new(scrutinee),
        arms,
    })
}

fn parse_struct_lit(pair: Pair<Rule>) -> Result<Expr, ParseError> {
    let mut name = String::new();
    let mut fields = Vec::new();
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::ident => name = inner.as_str().to_string(),
            Rule::f_field_init => {
                let mut fi = inner.into_inner();
                let fname = next_ident(&mut fi, "field name")?;
                let value = parse_expr(next_pair(&mut fi, "field value")?)?;
                fields.push(FieldInit { name: fname, value });
            }
            other => return Err(unexpected(other, "struct literal")),
        }
    }
    Ok(Expr::StructLit { name, fields })
}

fn parse_fpath(pair: Pair<Rule>) -> Result<Expr, ParseError> {
    Ok(Expr::Path(
        pair.into_inner().map(|p| p.as_str().to_string()).collect(),
    ))
}

fn parse_group_or_tuple(pair: Pair<Rule>) -> Result<Expr, ParseError> {
    let mut exprs: Vec<Expr> = pair
        .into_inner()
        .map(parse_expr)
        .collect::<Result<_, _>>()?;
    if exprs.len() == 1 {
        // A single parenthesized expression is just grouping.
        Ok(exprs.swap_remove(0))
    } else {
        Ok(Expr::Tuple(exprs))
    }
}

fn parse_array(pair: Pair<Rule>) -> Result<Expr, ParseError> {
    Ok(Expr::Array(
        pair.into_inner()
            .map(parse_expr)
            .collect::<Result<_, _>>()?,
    ))
}
