//! `match` exhaustiveness, for patterns nested to any depth.
//!
//! The usefulness algorithm of Maranget, "Warnings for pattern matching"
//! (JFP 2007): a `match` is exhaustive when a wildcard row would be useless
//! after its arms, and when it isn't, the algorithm builds a value no arm
//! matches, which the diagnostic shows.
//!
//! The checker describes each value's [`Shape`] (its finite set of
//! constructors, or infinitely many) and each arm as a [`Pat`].

/// The values of a type, as far as patterns can tell them apart.
#[derive(Debug, Clone)]
pub(crate) enum Shape {
    /// Finitely many constructors, each with its fields' shapes: an enum's
    /// variants, `Some` / `None`, a tuple's single constructor, a `u1`'s 0
    /// and 1.
    Finite(Vec<Ctor>),
    /// Too many values to list (a `u8`); only a catch-all covers them all.
    Infinite,
}

/// One constructor of a [`Shape::Finite`].
#[derive(Debug, Clone)]
pub(crate) struct Ctor {
    /// The name patterns use for it (`Some`, `Idle`, `1`, or `()` for a
    /// tuple).
    pub key: String,
    /// How a counterexample shows it (`Phase::Idle`).
    pub display: String,
    /// Its fields' shapes.
    pub fields: Vec<Shape>,
}

/// A pattern: a catch-all, or a constructor with sub-patterns.
#[derive(Debug, Clone)]
pub(crate) enum Pat {
    Wild,
    Ctor(String, Vec<Pat>),
}

/// A value no row matches, as a list of patterns (one per column).
type Witness = Vec<String>;

fn ctors(shape: &Shape) -> Option<&[Ctor]> {
    match shape {
        Shape::Finite(cs) => Some(cs),
        Shape::Infinite => None,
    }
}

/// Rows whose first column matches constructor `key` (of `arity` fields),
/// with that column replaced by its fields.
fn specialize(rows: &[Vec<Pat>], key: &str, arity: usize) -> Vec<Vec<Pat>> {
    rows.iter()
        .filter_map(|row| {
            let (head, rest) = row.split_first()?;
            let mut out = match head {
                Pat::Wild => vec![Pat::Wild; arity],
                Pat::Ctor(k, fields) if k == key => fields.clone(),
                Pat::Ctor(..) => return None,
            };
            out.extend_from_slice(rest);
            Some(out)
        })
        .collect()
}

/// `display` applied to its first `n` field witnesses.
fn show(c: &Ctor, fields: &[String]) -> String {
    if c.key == "()" {
        format!("({})", fields.join(", "))
    } else if fields.is_empty() {
        c.display.clone()
    } else {
        format!("{}({})", c.display, fields.join(", "))
    }
}

/// A witness that a row of wildcards over `shapes` is useful after `rows`:
/// a value (one pattern per column) that no row matches, if there is one.
fn useful(rows: &[Vec<Pat>], shapes: &[Shape]) -> Option<Witness> {
    let Some((shape, rest)) = shapes.split_first() else {
        // No columns left: useful iff nothing matched already.
        return rows.is_empty().then(Vec::new);
    };
    let heads: Vec<&str> = rows
        .iter()
        .filter_map(|r| match r.first() {
            Some(Pat::Ctor(k, _)) => Some(k.as_str()),
            _ => None,
        })
        .collect();
    match ctors(shape) {
        // Every constructor appears: one of them must leave a gap.
        Some(cs) if cs.iter().all(|c| heads.contains(&c.key.as_str())) => cs.iter().find_map(|c| {
            let arity = c.fields.len();
            let mut sub_shapes = c.fields.clone();
            sub_shapes.extend_from_slice(rest);
            let w = useful(&specialize(rows, &c.key, arity), &sub_shapes)?;
            let (fields, tail) = w.split_at(arity);
            let mut out = vec![show(c, fields)];
            out.extend_from_slice(tail);
            Some(out)
        }),
        // Some constructor (or, for an infinite shape, some value) is missing
        // from the first column: only the catch-all rows can match it.
        _ => {
            let defaults: Vec<Vec<Pat>> = rows
                .iter()
                .filter(|r| matches!(r.first(), Some(Pat::Wild)))
                .map(|r| r[1..].to_vec())
                .collect();
            let w = useful(&defaults, rest)?;
            // With no constructor in the column at all, any value is missing.
            let missing = match ctors(shape) {
                Some(cs) if !heads.is_empty() => cs
                    .iter()
                    .find(|c| !heads.contains(&c.key.as_str()))
                    .map(|c| show(c, &vec!["_".to_string(); c.fields.len()]))
                    .unwrap_or_else(|| "_".to_string()),
                _ => "_".to_string(),
            };
            let mut out = vec![missing];
            out.extend(w);
            Some(out)
        }
    }
}

/// A value of `shape` that none of `arms` matches, shown as a pattern, or
/// `None` if the arms are exhaustive.
pub(crate) fn missing(arms: &[Pat], shape: &Shape) -> Option<String> {
    let rows: Vec<Vec<Pat>> = arms.iter().map(|p| vec![p.clone()]).collect();
    useful(&rows, std::slice::from_ref(shape)).and_then(|w| w.into_iter().next())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(key: &str) -> Ctor {
        Ctor {
            key: key.to_string(),
            display: key.to_string(),
            fields: Vec::new(),
        }
    }

    fn bit() -> Shape {
        Shape::Finite(vec![unit("0"), unit("1")])
    }

    fn option(inner: Shape) -> Shape {
        Shape::Finite(vec![
            Ctor {
                key: "Some".to_string(),
                display: "Some".to_string(),
                fields: vec![inner],
            },
            unit("None"),
        ])
    }

    fn tuple(fields: Vec<Shape>) -> Shape {
        Shape::Finite(vec![Ctor {
            key: "()".to_string(),
            display: "()".to_string(),
            fields,
        }])
    }

    fn c(key: &str, fields: Vec<Pat>) -> Pat {
        Pat::Ctor(key.to_string(), fields)
    }

    #[test]
    fn all_four_bit_pairs_are_exhaustive() {
        let arms: Vec<Pat> = [("0", "0"), ("0", "1"), ("1", "0"), ("1", "1")]
            .iter()
            .map(|(a, b)| c("()", vec![c(a, vec![]), c(b, vec![])]))
            .collect();
        assert_eq!(missing(&arms, &tuple(vec![bit(), bit()])), None);
    }

    #[test]
    fn a_missing_pair_is_reported() {
        let arms = vec![
            c("()", vec![c("1", vec![]), Pat::Wild]),
            c("()", vec![c("0", vec![]), c("0", vec![])]),
        ];
        assert_eq!(
            missing(&arms, &tuple(vec![bit(), bit()])),
            Some("(0, 1)".to_string())
        );
    }

    #[test]
    fn nested_options() {
        let shape = option(option(Shape::Infinite));
        let arms = vec![
            c("Some", vec![c("Some", vec![Pat::Wild])]),
            c("None", vec![]),
        ];
        assert_eq!(missing(&arms, &shape), Some("Some(None)".to_string()));
        let mut all = arms;
        all.push(c("Some", vec![c("None", vec![])]));
        assert_eq!(missing(&all, &shape), None);
    }

    #[test]
    fn an_infinite_shape_needs_a_catch_all() {
        let arms = vec![c("3", vec![])];
        assert_eq!(missing(&arms, &Shape::Infinite), Some("_".to_string()));
        assert_eq!(
            missing(&[c("3", vec![]), Pat::Wild], &Shape::Infinite),
            None
        );
    }
}
