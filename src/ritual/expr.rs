use super::*;

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Lit(Value),
    Ref(Vec<String>),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Eq(Box<Expr>, Box<Expr>),
    Ne(Box<Expr>, Box<Expr>),
    Order(Box<Expr>, Box<Expr>, Compare),
    Call(Func, Vec<Expr>),
}

/// The functions an expression may call: pure, bounded string helpers. None
/// converts its arguments; a non-string argument is an error.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Func {
    Lower,
    Upper,
    Trim,
    Contains,
    StartsWith,
    EndsWith,
}

impl Func {
    fn named(name: &str) -> Option<Func> {
        Some(match name {
            "lower" => Func::Lower,
            "upper" => Func::Upper,
            "trim" => Func::Trim,
            "contains" => Func::Contains,
            "starts_with" => Func::StartsWith,
            "ends_with" => Func::EndsWith,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Func::Lower => "lower",
            Func::Upper => "upper",
            Func::Trim => "trim",
            Func::Contains => "contains",
            Func::StartsWith => "starts_with",
            Func::EndsWith => "ends_with",
        }
    }
    pub fn arity(self) -> usize {
        match self {
            Func::Lower | Func::Upper | Func::Trim => 1,
            Func::Contains | Func::StartsWith | Func::EndsWith => 2,
        }
    }
    /// True when the function returns a boolean, false when a string.
    pub fn returns_bool(self) -> bool {
        self.arity() == 2
    }
    fn apply(self, args: &[Value]) -> Result<Value, String> {
        let mut s = Vec::with_capacity(args.len());
        for (i, a) in args.iter().enumerate() {
            match a {
                Value::String(x) => s.push(x.as_str()),
                other => {
                    let kind = match other {
                        Value::Null => "null",
                        Value::Bool(_) => "a boolean",
                        Value::Number(_) => "a number",
                        Value::Array(_) => "a list",
                        _ => "an object",
                    };
                    return Err(format!("argument {} of `{}` must be a string, found {kind}", i + 1, self.name()));
                }
            }
        }
        Ok(match self {
            Func::Lower => Value::String(s[0].to_lowercase()),
            Func::Upper => Value::String(s[0].to_uppercase()),
            Func::Trim => Value::String(s[0].trim().to_string()),
            Func::Contains => Value::Bool(s[0].contains(s[1])),
            Func::StartsWith => Value::Bool(s[0].starts_with(s[1])),
            Func::EndsWith => Value::Bool(s[0].ends_with(s[1])),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Compare {
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Word(String),
    Str(String),
    Num(Value),
    Order(Compare),
    Eq,
    Ne,
    Open,
    Close,
    Comma,
}

fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '(' {
            out.push(Tok::Open);
            i += 1;
        } else if c == ')' {
            out.push(Tok::Close);
            i += 1;
        } else if c == ',' {
            out.push(Tok::Comma);
            i += 1;
        } else if c == '=' && chars.get(i + 1) == Some(&'=') {
            out.push(Tok::Eq);
            i += 2;
        } else if c == '!' && chars.get(i + 1) == Some(&'=') {
            out.push(Tok::Ne);
            i += 2;
        } else if c == '<' || c == '>' {
            let equal = chars.get(i + 1) == Some(&'=');
            out.push(Tok::Order(match (c, equal) {
                ('<', false) => Compare::Lt,
                ('<', true) => Compare::Le,
                ('>', false) => Compare::Gt,
                _ => Compare::Ge,
            }));
            i += if equal { 2 } else { 1 };
        } else if c == '"' || c == '\'' {
            let mut s = String::new();
            i += 1;
            loop {
                match chars.get(i) {
                    Some(&ch) if ch == c => break,
                    Some(&ch) => s.push(ch),
                    None => return Err(format!("unterminated string in `{src}`")),
                }
                i += 1;
            }
            i += 1;
            out.push(Tok::Str(s));
        } else if c.is_ascii_digit()
            || (c == '-' && chars.get(i + 1).is_some_and(char::is_ascii_digit))
        {
            let start = i;
            i += 1;
            while i < chars.len()
                && (chars[i].is_ascii_digit() || matches!(chars[i], '.' | 'e' | 'E' | '+' | '-'))
            {
                i += 1;
            }
            let s: String = chars[start..i].iter().collect();
            let n: Value = serde_json::from_str(&s).map_err(|_| format!("bad number `{s}`"))?;
            if !n.is_number() {
                return Err(format!("bad number `{s}`"));
            }
            out.push(Tok::Num(n));
        } else if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || matches!(chars[i], '_' | '-' | '.'))
            {
                i += 1;
            }
            out.push(Tok::Word(chars[start..i].iter().collect()));
        } else {
            return Err(format!("unexpected `{c}` in `{src}`"));
        }
    }
    Ok(out)
}

struct P {
    t: Vec<Tok>,
    i: usize,
}

impl P {
    fn word(&self, w: &str) -> bool {
        matches!(self.t.get(self.i), Some(Tok::Word(x)) if x == w)
    }
    fn or(&mut self) -> Result<Expr, String> {
        let mut l = self.and()?;
        while self.word("or") {
            self.i += 1;
            l = Expr::Or(Box::new(l), Box::new(self.and()?));
        }
        Ok(l)
    }
    fn and(&mut self) -> Result<Expr, String> {
        let mut l = self.not()?;
        while self.word("and") {
            self.i += 1;
            l = Expr::And(Box::new(l), Box::new(self.not()?));
        }
        Ok(l)
    }
    fn not(&mut self) -> Result<Expr, String> {
        if self.word("not") {
            self.i += 1;
            return Ok(Expr::Not(Box::new(self.not()?)));
        }
        self.cmp()
    }
    fn cmp(&mut self) -> Result<Expr, String> {
        let l = self.atom()?;
        match self.t.get(self.i) {
            Some(Tok::Eq) => {
                self.i += 1;
                Ok(Expr::Eq(Box::new(l), Box::new(self.atom()?)))
            }
            Some(Tok::Ne) => {
                self.i += 1;
                Ok(Expr::Ne(Box::new(l), Box::new(self.atom()?)))
            }
            Some(Tok::Order(op)) => {
                let op = *op;
                self.i += 1;
                Ok(Expr::Order(Box::new(l), Box::new(self.atom()?), op))
            }
            _ => Ok(l),
        }
    }
    fn atom(&mut self) -> Result<Expr, String> {
        let t = self.t.get(self.i).cloned().ok_or("expression ends early")?;
        self.i += 1;
        Ok(match t {
            Tok::Str(s) => Expr::Lit(Value::String(s)),
            Tok::Num(n) => Expr::Lit(n),
            Tok::Word(w) => match w.as_str() {
                "true" => Expr::Lit(Value::Bool(true)),
                "false" => Expr::Lit(Value::Bool(false)),
                "null" => Expr::Lit(Value::Null),
                "and" | "or" | "not" => return Err(format!("misplaced `{w}`")),
                _ if self.t.get(self.i) == Some(&Tok::Open) => {
                    let f = Func::named(&w).ok_or_else(|| {
                        format!("unknown function `{w}`; expressions offer lower, upper, trim, contains, starts_with and ends_with")
                    })?;
                    self.i += 1;
                    let mut args = Vec::new();
                    if self.t.get(self.i) != Some(&Tok::Close) {
                        loop {
                            args.push(self.or()?);
                            match self.t.get(self.i) {
                                Some(Tok::Comma) => self.i += 1,
                                Some(Tok::Close) => break,
                                _ => return Err(format!("missing `)` after the arguments of `{w}`")),
                            }
                        }
                    }
                    self.i += 1;
                    if args.len() != f.arity() {
                        let n = f.arity();
                        return Err(format!("`{w}` takes {n} argument{}, given {}", if n == 1 { "" } else { "s" }, args.len()));
                    }
                    Expr::Call(f, args)
                }
                _ => Expr::Ref(w.split('.').map(String::from).collect()),
            },
            Tok::Open => {
                let e = self.or()?;
                if self.t.get(self.i) != Some(&Tok::Close) {
                    return Err("missing `)`".into());
                }
                self.i += 1;
                e
            }
            other => return Err(format!("unexpected {other:?}")),
        })
    }
}

pub fn parse_expr(src: &str) -> Result<Expr, String> {
    if src.len() > 4096 {
        return Err("expression exceeds 4096 bytes".into());
    }
    let tokens = lex(src)?;
    if tokens.len() > 256 {
        return Err("expression exceeds 256 tokens".into());
    }
    let mut p = P { t: tokens, i: 0 };
    let e = p.or()?;
    if p.i != p.t.len() {
        return Err(format!("unexpected trailing text in `{src}`"));
    }
    Ok(e)
}

pub fn expr_refs(e: &Expr, out: &mut Vec<Vec<String>>) {
    match e {
        Expr::Lit(_) => {}
        Expr::Ref(p) => out.push(p.clone()),
        Expr::Not(a) => expr_refs(a, out),
        Expr::Call(_, args) => args.iter().for_each(|a| expr_refs(a, out)),
        Expr::And(a, b)
        | Expr::Or(a, b)
        | Expr::Eq(a, b)
        | Expr::Ne(a, b)
        | Expr::Order(a, b, _) => {
            expr_refs(a, out);
            expr_refs(b, out);
        }
    }
}

pub trait Env {
    fn lookup(&self, path: &[String]) -> Result<Value, String>;
}

fn as_bool(v: Value, what: &str) -> Result<bool, String> {
    match v {
        Value::Bool(b) => Ok(b),
        other => Err(format!("{what} must be true or false, found {other}")),
    }
}

pub fn eval(e: &Expr, env: &dyn Env) -> Result<Value, String> {
    Ok(match e {
        Expr::Lit(v) => v.clone(),
        Expr::Ref(p) => env.lookup(p)?,
        Expr::Not(a) => Value::Bool(!as_bool(eval(a, env)?, "operand of `not`")?),
        Expr::Call(f, args) => {
            let vals = args.iter().map(|a| eval(a, env)).collect::<Result<Vec<_>, _>>()?;
            f.apply(&vals)?
        }
        Expr::And(a, b) => Value::Bool(
            as_bool(eval(a, env)?, "operand of `and`")?
                && as_bool(eval(b, env)?, "operand of `and`")?,
        ),
        Expr::Or(a, b) => Value::Bool(
            as_bool(eval(a, env)?, "operand of `or`")?
                || as_bool(eval(b, env)?, "operand of `or`")?,
        ),
        Expr::Eq(a, b) => Value::Bool(values_equal(&eval(a, env)?, &eval(b, env)?)),
        Expr::Ne(a, b) => Value::Bool(!values_equal(&eval(a, env)?, &eval(b, env)?)),
        Expr::Order(a, b, op) => {
            let (a, b) = (eval(a, env)?, eval(b, env)?);
            let order = compare_values(&a, &b)?;
            Value::Bool(match op {
                Compare::Lt => order.is_lt(),
                Compare::Le => !order.is_gt(),
                Compare::Gt => order.is_gt(),
                Compare::Ge => !order.is_lt(),
            })
        }
    })
}

pub fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            if let (Some(xi), Some(yi)) = (
                x.as_i64().map(i128::from).or_else(|| x.as_u64().map(i128::from)),
                y.as_i64().map(i128::from).or_else(|| y.as_u64().map(i128::from)),
            ) {
                xi == yi
            } else {
                x.as_f64().is_some_and(|xf| y.as_f64() == Some(xf))
            }
        }
        _ => a == b,
    }
}

pub fn compare_values(a: &Value, b: &Value) -> Result<std::cmp::Ordering, String> {
    let integer = |v: &Value| {
        v.as_i64()
            .map(i128::from)
            .or_else(|| v.as_u64().map(i128::from))
    };
    match (a, b) {
        (Value::String(a), Value::String(b)) => Ok(a.cmp(b)),
        _ if integer(a).is_some() && integer(b).is_some() => Ok(integer(a).cmp(&integer(b))),
        (Value::Number(a), Value::Number(b)) if a.is_f64() && b.is_f64() => a
            .as_f64()
            .unwrap()
            .partial_cmp(&b.as_f64().unwrap())
            .ok_or_else(|| "unordered numbers".into()),
        _ => Err("ordering requires two integers, two floating-point numbers, or two strings; no implicit conversion".into()),
    }
}

pub fn eval_condition(src: &str, env: &dyn Env) -> Result<bool, String> {
    as_bool(eval(&parse_expr(src)?, env)?, "a condition")
}

/// Walk a value by path. A null along the way stays null (a skipped step has a
/// null output); a missing key in an object is an error.
pub fn descend(mut v: Value, path: &[String], full: &[String]) -> Result<Value, String> {
    for seg in path {
        v = match v {
            Value::Null => Value::Null,
            Value::Object(mut m) => m
                .remove(seg)
                .ok_or_else(|| format!("`{}` has no field `{seg}`", full.join(".")))?,
            Value::Array(mut a) => {
                let i: usize = seg
                    .parse()
                    .map_err(|_| format!("`{}`: `{seg}` is not a list index", full.join(".")))?;
                if i >= a.len() {
                    return Err(format!("`{}`: index {i} is out of range", full.join(".")));
                }
                a.swap_remove(i)
            }
            _ => {
                return Err(format!(
                    "`{}`: cannot look inside a plain value at `{seg}`",
                    full.join(".")
                ))
            }
        };
    }
    Ok(v)
}
