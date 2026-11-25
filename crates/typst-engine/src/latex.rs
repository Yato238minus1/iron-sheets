//! A small LaTeX-math to Typst-math converter for the common subset.
//!
//! Pure Rust, no dependencies, no allocation beyond the output; it only runs when a formula is
//! rendered in LaTeX mode (on the math worker thread). Output is plain Typst math without any
//! package, so it compiles with the same world as native Typst formulas.
//!
//! Design notes (the reasons for the odd looking output):
//! * Every identifier is emitted as a separate, space-separated token. LaTeX `ab` is a product of
//!   two letters; in Typst `ab` would be an unknown variable.
//! * `,` `;` `/` and the brackets are emitted as `comma`, `semi`, `\/`, `paren.l`, `bracket.r`...
//!   so that a formula never splits the argument list of the Typst function it is nested in and
//!   unbalanced delimiters such as `[0, 1)` stay harmless. (Plain brackets do not scale, as in LaTeX.)
//! * `\left ... \right` becomes `lr(...)`, which does scale.
//! * Unknown commands are an error (`unsupported LaTeX command \foo`), shown like a Typst error.

use std::fmt::Write as _;

/// Converts the body of a LaTeX math formula (no surrounding `$`) to Typst math source.
pub fn latex_to_typst(src: &str) -> Result<String, String> {
    let mut p = Parser { s: src.as_bytes(), src, i: 0, depth: 0 };
    let toks = p.seq(&End::Eof)?;
    Ok(toks.join(" "))
}

const MAX_DEPTH: u32 = 64;

struct Parser<'a> {
    s: &'a [u8],
    src: &'a str,
    i: usize,
    depth: u32,
}

#[derive(PartialEq)]
enum End {
    Eof,
    /// `}` of a brace group.
    Brace,
    /// `\right`.
    Right,
    /// `\end{...}`.
    Env,
    /// `]` of an optional argument.
    Bracket,
}

/// One parsed atom plus a pending `\underbrace`/`\overbrace` that wants its label.
struct Atom {
    text: String,
    brace: Option<(&'static str, String)>,
}

impl Atom {
    fn new(text: String) -> Self {
        Atom { text, brace: None }
    }
    fn finish(self) -> String {
        match self.brace {
            Some((f, body)) => format!("{f}({body})"),
            None => self.text,
        }
    }
}

enum Tok {
    Eof,
    /// A control sequence (`\foo`) or control symbol (`\,`); the name without the backslash.
    Cmd(usize, usize),
    Char(char),
}

impl<'a> Parser<'a> {
    fn skip_ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn peek(&mut self) -> Tok {
        self.skip_ws();
        let Some(&c) = self.s.get(self.i) else { return Tok::Eof };
        if c == b'\\' {
            let st = self.i + 1;
            let mut e = st;
            while e < self.s.len() && self.s[e].is_ascii_alphabetic() {
                e += 1;
            }
            if e == st {
                // control symbol: one (possibly multibyte) char
                if let Some(ch) = self.src[st.min(self.src.len())..].chars().next() {
                    e = st + ch.len_utf8();
                }
            }
            return Tok::Cmd(st, e);
        }
        match self.src[self.i..].chars().next() {
            Some(ch) => Tok::Char(ch),
            None => Tok::Eof,
        }
    }

    fn cmd_name(&self, st: usize, e: usize) -> &'a str {
        &self.src[st..e]
    }

    /// Sequence of atoms until `end` (which is consumed except for `Eof`).
    fn seq(&mut self, end: &End) -> Result<Vec<String>, String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err("LaTeX formula is nested too deeply".into());
        }
        let mut out: Vec<String> = Vec::new();
        loop {
            match self.peek() {
                Tok::Eof => {
                    self.depth -= 1;
                    return match end {
                        End::Eof => Ok(out),
                        End::Brace => Err("missing `}`".into()),
                        End::Right => Err("`\\left` without `\\right`".into()),
                        End::Env => Err("`\\begin` without `\\end`".into()),
                        End::Bracket => Err("missing `]`".into()),
                    };
                }
                Tok::Char('}') => {
                    if *end == End::Brace {
                        self.i += 1;
                        self.depth -= 1;
                        return Ok(out);
                    }
                    return Err("unmatched `}`".into());
                }
                Tok::Char(']') if *end == End::Bracket => {
                    self.i += 1;
                    self.depth -= 1;
                    return Ok(out);
                }
                Tok::Cmd(st, e) => {
                    let name = self.cmd_name(st, e);
                    if name == "right" {
                        if *end == End::Right {
                            self.i = e;
                            self.depth -= 1;
                            return Ok(out);
                        }
                        return Err("`\\right` without `\\left`".into());
                    }
                    if name == "end" {
                        if *end == End::Env {
                            self.depth -= 1;
                            return Ok(out); // caller reads the `\end{..}`
                        }
                        return Err("`\\end` without `\\begin`".into());
                    }
                }
                _ => {}
            }
            self.item(&mut out)?;
        }
    }

    /// One atom with its scripts appended to `out`.
    fn item(&mut self, out: &mut Vec<String>) -> Result<(), String> {
        // Row/column separators and spacing are items without scripts.
        match self.peek() {
            Tok::Char('&') => {
                self.i += 1;
                out.push("&".into());
                return Ok(());
            }
            Tok::Char('\'') => {
                self.i += 1;
                match out.last_mut() {
                    Some(last) => last.push('\''),
                    None => out.push("'".into()),
                }
                return Ok(());
            }
            Tok::Cmd(st, e) if self.cmd_name(st, e) == "\\" => {
                self.i = e;
                // optional `[len]` after `\\`
                self.skip_ws();
                if self.s.get(self.i) == Some(&b'[')
                    && let Some(p) = self.src[self.i..].find(']')
                {
                    self.i += p + 1;
                }
                out.push("\\".into());
                return Ok(());
            }
            Tok::Char('^' | '_') => {
                // script without a base: attach to an empty group
                let mut a = Atom::new(String::new());
                self.scripts(&mut a, out)?;
                return Ok(());
            }
            _ => {}
        }
        let mut atom = self.atom()?;
        let Some(atom_ready) = atom.take() else { return Ok(()) };
        let mut a = atom_ready;
        self.scripts(&mut a, out)
    }

    /// Reads `^`/`_` scripts (and `\limits`) after `a`, pushes the finished atom onto `out`.
    fn scripts(&mut self, a: &mut Atom, out: &mut Vec<String>) -> Result<(), String> {
        let mut sub: Option<String> = None;
        let mut sup: Option<String> = None;
        loop {
            match self.peek() {
                Tok::Char(c @ ('^' | '_')) => {
                    self.i += 1;
                    let arg = self.arg()?;
                    let slot = if c == '^' { &mut sup } else { &mut sub };
                    if slot.is_some() {
                        return Err("double script".into());
                    }
                    *slot = Some(arg);
                }
                Tok::Cmd(st, e) if matches!(self.cmd_name(st, e), "limits" | "nolimits") => {
                    let no = self.cmd_name(st, e) == "nolimits";
                    self.i = e;
                    let base = std::mem::take(&mut a.text);
                    a.text = format!("{}({base})", if no { "scripts" } else { "limits" });
                }
                _ => break,
            }
        }
        // `\underbrace{x}_{y}` / `\overbrace{x}^{y}`: the label belongs to the brace.
        if let Some((f, body)) = a.brace.take() {
            let label = if f == "underbrace" { sub.take() } else { sup.take() };
            a.text = match label {
                Some(l) => format!("{f}({body}, {l})"),
                None => format!("{f}({body})"),
            };
        }
        let mut s = std::mem::take(&mut a.text);
        if let Some(b) = sub {
            let _ = write!(s, "_({b})");
        }
        if let Some(p) = sup {
            let _ = write!(s, "^({p})");
        }
        out.push(s);
        Ok(())
    }

    /// A script/command argument: a `{group}` or a single token.
    fn arg(&mut self) -> Result<String, String> {
        match self.peek() {
            Tok::Eof => Err("missing argument".into()),
            Tok::Char('{') => {
                self.i += 1;
                Ok(self.seq(&End::Brace)?.join(" "))
            }
            Tok::Char('}') => Err("missing argument".into()),
            // LaTeX takes exactly one character: `\frac12` is 1 over 2
            Tok::Char(c) if c.is_ascii_digit() => {
                self.i += 1;
                Ok(c.to_string())
            }
            _ => {
                let mut v = Vec::new();
                match self.atom()? {
                    Some(a) => v.push(a.finish()),
                    None => return Err("missing argument".into()),
                }
                Ok(v.join(" "))
            }
        }
    }

    /// Optional `[...]` argument, raw tokens.
    fn opt_arg(&mut self) -> Result<Option<String>, String> {
        self.skip_ws();
        if self.s.get(self.i) == Some(&b'[') {
            self.i += 1;
            return Ok(Some(self.seq(&End::Bracket)?.join(" ")));
        }
        Ok(None)
    }

    /// Raw text of a `{...}` argument (no nesting interpretation beyond brace matching).
    fn raw_group(&mut self) -> Result<&'a str, String> {
        self.skip_ws();
        if self.s.get(self.i) != Some(&b'{') {
            return Err("expected `{`".into());
        }
        let st = self.i + 1;
        let mut depth = 1usize;
        let mut k = st;
        while k < self.s.len() {
            match self.s[k] {
                b'\\' => k += 1,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        self.i = k + 1;
                        return Ok(&self.src[st..k]);
                    }
                }
                _ => {}
            }
            k += 1;
        }
        Err("missing `}`".into())
    }

    /// `None` for items that produce nothing (e.g. `\displaystyle`).
    fn atom(&mut self) -> Result<Option<Atom>, String> {
        self.depth += 1;
        let r = if self.depth > MAX_DEPTH { Err("LaTeX formula is nested too deeply".to_string()) } else { self.atom_inner() };
        self.depth -= 1;
        r
    }

    fn atom_inner(&mut self) -> Result<Option<Atom>, String> {
        match self.peek() {
            Tok::Eof => Err("unexpected end of formula".into()),
            Tok::Char(c) => {
                self.i += c.len_utf8();
                self.char_atom(c).map(|t| Some(Atom::new(t)))
            }
            Tok::Cmd(st, e) => {
                let name = self.cmd_name(st, e);
                self.i = e;
                self.command(name)
            }
        }
    }

    fn char_atom(&mut self, c: char) -> Result<String, String> {
        Ok(match c {
            '{' => self.seq(&End::Brace)?.join(" "),
            c if c.is_ascii_digit() => {
                // number with optional decimal part
                let mut s = String::from(c);
                while let Some(&d) = self.s.get(self.i) {
                    if d.is_ascii_digit() {
                        s.push(d as char);
                        self.i += 1;
                    } else if d == b'.' && self.s.get(self.i + 1).is_some_and(u8::is_ascii_digit) {
                        s.push('.');
                        self.i += 1;
                    } else {
                        break;
                    }
                }
                s
            }
            c if c.is_ascii_alphabetic() => c.to_string(),
            '+' | '-' | '=' | '<' | '>' | '!' | '?' | '*' | '@' => match c {
                '*' => "ast".into(),
                '-' => "minus".into(),
                _ => c.to_string(),
            },
            '(' => "paren.l".into(),
            ')' => "paren.r".into(),
            '[' => "bracket.l".into(),
            ']' => "bracket.r".into(),
            '|' => "bar.v".into(),
            ',' => "comma".into(),
            ';' => "semi".into(),
            ':' => "colon".into(),
            '.' => "dot.basic".into(),
            '/' => "\\/".into(),
            '~' => "space.nobreak".into(),
            '#' => "\\#".into(),
            '$' => "dollar".into(),
            '%' => "percent".into(),
            '"' => "\"\\\"\"".into(),
            '`' => "\\`".into(),
            '&' => "&".into(),
            '^' | '_' => return Err(format!("misplaced `{c}`")),
            c if (c as u32) >= 0x80 => c.to_string(),
            c => return Err(format!("unsupported character `{c}` in LaTeX formula")),
        })
    }

    fn command(&mut self, name: &str) -> Result<Option<Atom>, String> {
        let simple = |s: &str| Ok(Some(Atom::new(s.to_string())));
        if let Some(t) = symbol(name) {
            return simple(t);
        }
        match name {
            // ----- ignored declarations
            "displaystyle" | "textstyle" | "scriptstyle" | "scriptscriptstyle" | "bigl" | "bigr" | "Bigl" | "Bigr" | "biggl" | "biggr" | "Biggl" | "Biggr"
            | "big" | "Big" | "bigg" | "Bigg" | "relax" | "protect" => {
                if matches!(name, "bigl" | "bigr" | "Bigl" | "Bigr" | "biggl" | "biggr" | "Biggl" | "Biggr" | "big" | "Big" | "bigg" | "Bigg") {
                    // sized delimiter: keep the delimiter itself
                    return self.delim_atom();
                }
                Ok(None)
            }
            // ----- spacing
            "," | "thinspace" => simple("thin"),
            ":" | "medspace" | " " => simple("med"),
            ";" | "thickspace" => simple("thick"),
            "!" => simple("#h(-0.17em)"),
            "quad" => simple("quad"),
            "qquad" => simple("wide"),
            "\\" => simple("\\"),
            // ----- escapes
            "{" => simple("brace.l"),
            "}" => simple("brace.r"),
            "|" => simple("bar.v.double"),
            "#" => simple("\\#"),
            "$" => simple("dollar"),
            "%" => simple("percent"),
            "&" => simple("\\&"),
            "_" => simple("\\_"),
            "/" => Ok(None),
            // ----- structures
            "frac" | "dfrac" | "tfrac" | "cfrac" => {
                let (a, b) = (self.arg()?, self.arg()?);
                Ok(Some(Atom::new(format!("frac({a}, {b})"))))
            }
            "binom" | "dbinom" | "tbinom" => {
                let (a, b) = (self.arg()?, self.arg()?);
                Ok(Some(Atom::new(format!("binom({a}, {b})"))))
            }
            "sqrt" => {
                let n = self.opt_arg()?;
                let a = self.arg()?;
                Ok(Some(Atom::new(match n {
                    Some(n) => format!("root({n}, {a})"),
                    None => format!("sqrt({a})"),
                })))
            }
            "overline" | "bar" | "widebar" => self.wrap1("overline"),
            "underline" => self.wrap1("underline"),
            "hat" | "widehat" => self.wrap1("hat"),
            "tilde" | "widetilde" => self.wrap1("tilde"),
            "vec" | "overrightarrow" => self.wrap1("arrow"),
            "dot" => self.wrap1("dot"),
            "ddot" => self.wrap1("dot.double"),
            "dddot" => self.wrap1("dot.triple"),
            "acute" => self.wrap1("acute"),
            "grave" => self.wrap1("grave"),
            "breve" => self.wrap1("breve"),
            "check" => self.wrap1("caron"),
            "cancel" => self.wrap1("cancel"),
            "mathbb" => self.wrap_font("bb"),
            "mathbf" | "boldsymbol" | "bm" | "pmb" => self.wrap_font("bold"),
            "mathcal" | "mathscr" => self.wrap_font("cal"),
            "mathfrak" => self.wrap_font("frak"),
            "mathsf" => self.wrap_font("sans"),
            "mathtt" => self.wrap_font("mono"),
            "mathit" => self.wrap_font("italic"),
            "mathrm" | "rm" | "operatorname" | "mathop" => {
                let star = name == "operatorname" && self.s.get(self.i) == Some(&b'*');
                if star {
                    self.i += 1;
                }
                let g = self.arg_text_or_math()?;
                if name == "operatorname" || name == "mathop" {
                    let q = quote_if_word(&g);
                    return Ok(Some(Atom::new(if star { format!("op({q}, limits: #true)") } else { format!("op({q})") })));
                }
                Ok(Some(Atom::new(format!("upright({})", quote_if_word(&g)))))
            }
            "text" | "textrm" | "mbox" | "hbox" | "textnormal" => {
                let raw = self.raw_group()?;
                Ok(Some(Atom::new(quote(raw))))
            }
            "textbf" => {
                let raw = self.raw_group()?;
                Ok(Some(Atom::new(format!("bold({})", quote(raw)))))
            }
            "textit" | "emph" => {
                let raw = self.raw_group()?;
                Ok(Some(Atom::new(format!("italic({})", quote(raw)))))
            }
            "underbrace" | "overbrace" => {
                let body = self.arg()?;
                let f = if name == "underbrace" { "underbrace" } else { "overbrace" };
                Ok(Some(Atom { text: String::new(), brace: Some((f, body)) }))
            }
            "left" => {
                let open = self.delim()?;
                let body = self.seq(&End::Right)?;
                let close = self.delim()?;
                let mut parts: Vec<String> = Vec::new();
                if !open.is_empty() {
                    parts.push(open);
                }
                parts.extend(body);
                if !close.is_empty() {
                    parts.push(close);
                }
                Ok(Some(Atom::new(format!("lr({})", parts.join(" ")))))
            }
            "middle" => {
                let d = self.delim()?;
                Ok(Some(Atom::new(format!("mid({d})"))))
            }
            "begin" => self.environment(),
            "color" | "textcolor" => {
                // `\color{red}` applies to what follows; the colour is ignored (the theme's text
                // colour is used), `\textcolor{red}{x}` keeps its argument.
                let _ = self.raw_group()?;
                if name == "textcolor" {
                    let a = self.arg()?;
                    return Ok(Some(Atom::new(a)));
                }
                Ok(None)
            }
            "boxed" => {
                let a = self.arg()?;
                Ok(Some(Atom::new(format!("#box(stroke: 0.5pt, inset: 3pt, $ {a} $)"))))
            }
            "stackrel" | "overset" => {
                let (a, b) = (self.arg()?, self.arg()?);
                Ok(Some(Atom::new(format!("limits(attach({b}, t: {a}))"))))
            }
            "underset" => {
                let (a, b) = (self.arg()?, self.arg()?);
                Ok(Some(Atom::new(format!("limits(attach({b}, b: {a}))"))))
            }
            "pmod" => {
                let a = self.arg()?;
                Ok(Some(Atom::new(format!("quad paren.l \"mod\" med {a} paren.r"))))
            }
            "bmod" => simple("\"mod\""),
            "mod" => simple("quad \"mod\""),
            "label" | "tag" | "nonumber" | "notag" => {
                if name != "nonumber" && name != "notag" {
                    let _ = self.raw_group()?;
                }
                Ok(None)
            }
            _ => Err(format!("unsupported LaTeX command `\\{name}`")),
        }
    }

    fn wrap1(&mut self, f: &str) -> Result<Option<Atom>, String> {
        let a = self.arg()?;
        Ok(Some(Atom::new(format!("{f}({a})"))))
    }

    /// Font commands: single letters are separate arguments (`bb(R)`), words stay tokens.
    fn wrap_font(&mut self, f: &str) -> Result<Option<Atom>, String> {
        let a = self.arg()?;
        Ok(Some(Atom::new(format!("{f}({a})"))))
    }

    /// Argument of `\mathrm`/`\operatorname`: its raw letters when it is plain text, otherwise the
    /// converted math.
    fn arg_text_or_math(&mut self) -> Result<String, String> {
        self.skip_ws();
        if self.s.get(self.i) == Some(&b'{') {
            let save = self.i;
            let raw = self.raw_group()?;
            if !raw.is_empty() && raw.bytes().all(|c| c.is_ascii_alphabetic()) {
                return Ok(raw.to_string());
            }
            self.i = save;
        }
        self.arg()
    }

    /// Delimiter after `\left`, `\right`, `\middle`: a Typst symbol name, or empty for `.`.
    fn delim(&mut self) -> Result<String, String> {
        match self.peek() {
            Tok::Eof => Err("missing delimiter".into()),
            Tok::Char(c) => {
                self.i += c.len_utf8();
                Ok(match c {
                    '(' => "paren.l".into(),
                    ')' => "paren.r".into(),
                    '[' => "bracket.l".into(),
                    ']' => "bracket.r".into(),
                    '|' => "bar.v".into(),
                    '/' => "\\/".into(),
                    '<' => "chevron.l".into(),
                    '>' => "chevron.r".into(),
                    '.' => String::new(),
                    _ => return Err(format!("unsupported delimiter `{c}`")),
                })
            }
            Tok::Cmd(st, e) => {
                let n = self.cmd_name(st, e);
                self.i = e;
                Ok(match n {
                    "{" | "lbrace" => "brace.l".into(),
                    "}" | "rbrace" => "brace.r".into(),
                    "|" | "Vert" | "lVert" | "rVert" => "bar.v.double".into(),
                    "vert" | "lvert" | "rvert" => "bar.v".into(),
                    "langle" => "chevron.l".into(),
                    "rangle" => "chevron.r".into(),
                    "lfloor" => "floor.l".into(),
                    "rfloor" => "floor.r".into(),
                    "lceil" => "ceil.l".into(),
                    "rceil" => "ceil.r".into(),
                    "backslash" => "backslash".into(),
                    "uparrow" => "arrow.t".into(),
                    "downarrow" => "arrow.b".into(),
                    _ => return Err(format!("unsupported delimiter `\\{n}`")),
                })
            }
        }
    }

    fn delim_atom(&mut self) -> Result<Option<Atom>, String> {
        let d = self.delim()?;
        Ok(Some(Atom::new(d.trim().to_string())))
    }

    fn environment(&mut self) -> Result<Option<Atom>, String> {
        let env = self.raw_group()?.trim().to_string();
        let base = env.trim_end_matches('*');
        let matrix_delim = match base {
            "matrix" | "smallmatrix" | "array" => Some("#none"),
            "pmatrix" => Some("\"(\""),
            "bmatrix" => Some("\"[\""),
            "Bmatrix" => Some("\"{\""),
            "vmatrix" => Some("\"|\""),
            "Vmatrix" => Some("\"||\""),
            _ => None,
        };
        let is_cases = matches!(base, "cases" | "dcases" | "rcases");
        let is_aligned = matches!(base, "aligned" | "align" | "alignat" | "split" | "gathered" | "gather" | "eqnarray" | "equation" | "multline" | "flalign");
        if matrix_delim.is_none() && !is_cases && !is_aligned {
            return Err(format!("unsupported LaTeX environment `{env}`"));
        }
        if base == "array" || base == "alignat" {
            let _ = self.raw_group()?; // column spec / count
        }
        // Body as rows of cells.
        let mut rows: Vec<Vec<Vec<String>>> = vec![vec![Vec::new()]];
        loop {
            match self.peek() {
                Tok::Eof => return Err(format!("missing `\\end{{{env}}}`")),
                Tok::Cmd(st, e) if self.cmd_name(st, e) == "end" => {
                    self.i = e;
                    let closing = self.raw_group()?.trim().to_string();
                    if closing != env {
                        return Err(format!("`\\begin{{{env}}}` closed by `\\end{{{closing}}}`"));
                    }
                    break;
                }
                Tok::Cmd(st, e) if self.cmd_name(st, e) == "\\" => {
                    self.i = e;
                    self.skip_ws();
                    if self.s.get(self.i) == Some(&b'[')
                        && let Some(p) = self.src[self.i..].find(']')
                    {
                        self.i += p + 1;
                    }
                    rows.push(vec![Vec::new()]);
                }
                Tok::Char('&') => {
                    self.i += 1;
                    if let Some(r) = rows.last_mut() {
                        r.push(Vec::new());
                    }
                }
                Tok::Cmd(st, e) if matches!(self.cmd_name(st, e), "hline" | "toprule" | "midrule" | "bottomrule") => {
                    self.i = e;
                }
                _ => {
                    self.depth += 1;
                    if self.depth > MAX_DEPTH {
                        return Err("LaTeX formula is nested too deeply".into());
                    }
                    let mut tmp = Vec::new();
                    let r = self.item(&mut tmp);
                    self.depth -= 1;
                    r?;
                    if let Some(cell) = rows.last_mut().and_then(|r| r.last_mut()) {
                        cell.extend(tmp);
                    }
                }
            }
        }
        // Drop a trailing empty row (`\\` before `\end`).
        if rows.len() > 1 && rows.last().is_some_and(|r| r.len() == 1 && r[0].is_empty()) {
            rows.pop();
        }
        let cell = |c: &Vec<String>| c.join(" ");
        let text = if let Some(delim) = matrix_delim {
            let body: Vec<String> = rows.iter().map(|r| r.iter().map(cell).collect::<Vec<_>>().join(", ")).collect();
            if delim == "#none" { format!("mat(delim: #none, {})", body.join("; ")) } else { format!("mat(delim: {delim}, {})", body.join("; ")) }
        } else if is_cases {
            let body: Vec<String> = rows.iter().map(|r| r.iter().map(cell).collect::<Vec<_>>().join(" quad ")).collect();
            if base == "rcases" { format!("cases(reverse: #true, {})", body.join(", ")) } else { format!("cases({})", body.join(", ")) }
        } else {
            let body: Vec<String> = rows.iter().map(|r| r.iter().map(cell).collect::<Vec<_>>().join(" & ")).collect();
            body.join(" \\ ")
        };
        Ok(Some(Atom::new(text)))
    }
}

/// `abc` -> `"abc"`; anything else is left as converted math.
fn quote_if_word(s: &str) -> String {
    if !s.is_empty() && s.bytes().all(|c| c.is_ascii_alphabetic()) && s.len() > 1 { format!("\"{s}\"") } else { s.to_string() }
}

/// Typst string literal.
fn quote(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' | '\r' => o.push(' '),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// Commands that map to a Typst symbol or function-free word.
fn symbol(name: &str) -> Option<&'static str> {
    Some(match name {
        // greek
        "alpha" => "alpha",
        "beta" => "beta",
        "gamma" => "gamma",
        "delta" => "delta",
        "epsilon" => "epsilon.alt",
        "varepsilon" => "epsilon",
        "zeta" => "zeta",
        "eta" => "eta",
        "theta" => "theta",
        "vartheta" => "theta.alt",
        "iota" => "iota",
        "kappa" => "kappa",
        "varkappa" => "kappa.alt",
        "lambda" => "lambda",
        "mu" => "mu",
        "nu" => "nu",
        "xi" => "xi",
        "pi" => "pi",
        "varpi" => "pi.alt",
        "rho" => "rho",
        "varrho" => "rho.alt",
        "sigma" => "sigma",
        "varsigma" => "sigma.alt",
        "tau" => "tau",
        "upsilon" => "upsilon",
        "phi" => "phi.alt",
        "varphi" => "phi",
        "chi" => "chi",
        "psi" => "psi",
        "omega" => "omega",
        "Gamma" => "Gamma",
        "Delta" => "Delta",
        "Theta" => "Theta",
        "Lambda" => "Lambda",
        "Xi" => "Xi",
        "Pi" => "Pi",
        "Sigma" => "Sigma",
        "Upsilon" => "Upsilon",
        "Phi" => "Phi",
        "Psi" => "Psi",
        "Omega" => "Omega",
        // big operators
        "sum" => "sum",
        "prod" => "product",
        "coprod" => "product.co",
        "int" => "integral",
        "iint" => "integral.double",
        "iiint" => "integral.triple",
        "oint" => "integral.cont",
        "bigcup" => "union.big",
        "bigcap" => "inter.big",
        "bigoplus" => "plus.o.big",
        "bigotimes" => "times.o.big",
        "bigvee" => "or.big",
        "bigwedge" => "and.big",
        // named functions
        "sin" => "sin",
        "cos" => "cos",
        "tan" => "tan",
        "cot" => "cot",
        "sec" => "sec",
        "csc" => "csc",
        "arcsin" => "arcsin",
        "arccos" => "arccos",
        "arctan" => "arctan",
        "sinh" => "sinh",
        "cosh" => "cosh",
        "tanh" => "tanh",
        "coth" => "coth",
        "log" => "log",
        "ln" => "ln",
        "lg" => "lg",
        "exp" => "exp",
        "min" => "min",
        "max" => "max",
        "sup" => "sup",
        "inf" => "inf",
        "det" => "det",
        "gcd" => "gcd",
        "lim" => "lim",
        "limsup" => "limsup",
        "liminf" => "liminf",
        "ker" => "ker",
        "dim" => "dim",
        "deg" => "deg",
        "arg" => "arg",
        "hom" => "hom",
        "Pr" => "Pr",
        // relations
        "leq" | "le" => "<=",
        "geq" | "ge" => ">=",
        "neq" | "ne" => "!=",
        "ll" => "<<",
        "gg" => ">>",
        "approx" => "approx",
        "equiv" => "equiv",
        "sim" => "tilde.op",
        "simeq" => "tilde.eq",
        "cong" => "tilde.equiv",
        "propto" => "prop",
        "doteq" => "dot(eq)",
        "prec" => "prec",
        "succ" => "succ",
        "preceq" => "prec.eq",
        "succeq" => "succ.eq",
        "in" => "in",
        "notin" => "in.not",
        "ni" => "in.rev",
        "subset" => "subset",
        "subseteq" => "subset.eq",
        "supset" => "supset",
        "supseteq" => "supset.eq",
        "nsubseteq" => "subset.eq.not",
        "perp" => "perp",
        "parallel" => "parallel",
        "mid" => "bar.v",
        "nmid" => "divides.not",
        "models" => "tack.r.double",
        "vdash" => "tack.r",
        "dashv" => "tack.l",
        // arrows
        "to" | "rightarrow" => "->",
        "leftarrow" | "gets" => "<-",
        "leftrightarrow" => "<->",
        "Rightarrow" => "=>",
        "Leftarrow" => "arrow.l.double",
        "Leftrightarrow" => "<=>",
        "implies" => "==>",
        "iff" => "<==>",
        "mapsto" => "|->",
        "longrightarrow" => "-->",
        "longleftarrow" => "<--",
        "Longrightarrow" => "==>",
        "Longleftrightarrow" => "<==>",
        "uparrow" => "arrow.t",
        "downarrow" => "arrow.b",
        "hookrightarrow" => "arrow.r.hook",
        "rightharpoonup" => "harpoon.rt",
        "leadsto" => "arrow.r.squiggly",
        // binary operators
        "cdot" => "dot.op",
        "times" => "times",
        "div" => "div",
        "pm" => "plus.minus",
        "mp" => "minus.plus",
        "ast" => "ast",
        "star" => "star",
        "circ" => "compose",
        "bullet" => "bullet",
        "oplus" => "plus.o",
        "otimes" => "times.o",
        "ominus" => "minus.o",
        "odot" => "dot.o",
        "cup" => "union",
        "cap" => "inter",
        "setminus" => "without",
        "wedge" | "land" => "and",
        "vee" | "lor" => "or",
        "sqcup" => "union.sq",
        "sqcap" => "inter.sq",
        "wr" => "wreath",
        "dagger" => "dagger",
        "ddagger" => "dagger.double",
        // misc symbols
        "infty" => "oo",
        "partial" => "partial",
        "nabla" => "nabla",
        "emptyset" | "varnothing" => "emptyset",
        "forall" => "forall",
        "exists" => "exists",
        "nexists" => "exists.not",
        "neg" | "lnot" => "not",
        "ldots" | "dots" | "dotsc" | "dotsb" | "dotsm" | "dotsi" | "hdots" => "dots.h",
        "cdots" => "dots.h.c",
        "vdots" => "dots.v",
        "ddots" => "dots.down",
        "angle" => "angle",
        "triangle" => "triangle.stroked.t",
        "square" | "Box" => "square.stroked",
        "prime" => "prime",
        "hbar" | "hslash" => "planck",
        "ell" => "ell",
        "Re" => "Re",
        "Im" => "Im",
        "aleph" => "aleph",
        "wp" => "pee",
        "top" => "top",
        "bot" => "bot",
        "therefore" => "therefore",
        "because" => "because",
        "degree" => "degree",
        "checkmark" => "checkmark",
        "langle" => "chevron.l",
        "rangle" => "chevron.r",
        "lfloor" => "floor.l",
        "rfloor" => "floor.r",
        "lceil" => "ceil.l",
        "rceil" => "ceil.r",
        "lbrace" => "brace.l",
        "rbrace" => "brace.r",
        "lbrack" => "bracket.l",
        "rbrack" => "bracket.r",
        "vert" => "bar.v",
        "Vert" => "bar.v.double",
        "backslash" => "backslash",
        "surd" => "sqrt(\"\")",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(s: &str) -> String {
        latex_to_typst(s).unwrap_or_else(|e| format!("ERR {e}"))
    }

    #[test]
    fn letters_digits_and_operators() {
        assert_eq!(c("ab+12"), "a b + 12");
        assert_eq!(c("x=1.5"), "x = 1.5");
        assert_eq!(c("a/b"), "a \\/ b");
        assert_eq!(c("a,b;c"), "a comma b semi c");
        assert_eq!(c("(a)[b]"), "paren.l a paren.r bracket.l b bracket.r");
    }

    #[test]
    fn scripts() {
        assert_eq!(c("x^2"), "x^(2)");
        assert_eq!(c("x_{i,j}^{n+1}"), "x_(i comma j)^(n + 1)");
        assert_eq!(c("e^{-x}"), "e^(minus x)");
        assert_eq!(c("f'(x)"), "f' paren.l x paren.r");
        assert_eq!(c("\\sum_{i=1}^n a_i"), "sum_(i = 1)^(n) a_(i)");
    }

    #[test]
    fn fractions_and_roots() {
        assert_eq!(c("\\frac{a}{b}"), "frac(a, b)");
        assert_eq!(c("\\frac12"), "frac(1, 2)");
        assert_eq!(c("\\sqrt{x^2+y^2}"), "sqrt(x^(2) + y^(2))");
        assert_eq!(c("\\sqrt[3]{x}"), "root(3, x)");
        assert_eq!(c("\\binom{n}{k}"), "binom(n, k)");
    }

    #[test]
    fn fonts_and_text() {
        assert_eq!(c("\\mathbb{R}"), "bb(R)");
        assert_eq!(c("\\mathbf{v}"), "bold(v)");
        assert_eq!(c("\\mathrm{d}x"), "upright(d) x");
        assert_eq!(c("\\mathrm{sin}"), "upright(\"sin\")");
        assert_eq!(c("\\text{if } x"), "\"if \" x");
        assert_eq!(c("\\operatorname{rank} A"), "op(\"rank\") A");
        assert_eq!(c("\\text{a \"q\"}"), "\"a \\\"q\\\"\"");
    }

    #[test]
    fn symbols() {
        assert_eq!(c("\\alpha\\leq\\beta"), "alpha <= beta");
        assert_eq!(c("a\\cdot b\\times c"), "a dot.op b times c");
        assert_eq!(c("\\int_0^\\infty e^{-x}\\,dx"), "integral_(0)^(oo) e^(minus x) thin d x");
        assert_eq!(c("A\\to B"), "A -> B");
    }

    #[test]
    fn left_right() {
        assert_eq!(c("\\left( a \\right)"), "lr(paren.l a paren.r)");
        assert_eq!(c("\\left\\{ a, b \\right."), "lr(brace.l a comma b)");
        assert_eq!(c("\\left[0,1\\right)"), "lr(bracket.l 0 comma 1 paren.r)");
        assert_eq!(c("\\left\\langle x \\right\\rangle"), "lr(chevron.l x chevron.r)");
        assert!(c("\\left( a").starts_with("ERR"));
        assert!(c("a \\right)").starts_with("ERR"));
    }

    #[test]
    fn environments() {
        assert_eq!(c("\\begin{pmatrix} a & b \\\\ c & d \\end{pmatrix}"), "mat(delim: \"(\", a, b; c, d)");
        assert_eq!(c("\\begin{matrix}1&2\\\\3&4\\\\\\end{matrix}"), "mat(delim: #none, 1, 2; 3, 4)");
        assert_eq!(c("\\begin{cases} x & x>0 \\\\ 0 & \\text{else} \\end{cases}"), "cases(x quad x > 0, 0 quad \"else\")");
        assert_eq!(c("\\begin{aligned} a &= b \\\\ c &= d \\end{aligned}"), "a & = b \\ c & = d");
        assert!(c("\\begin{pmatrix} a").starts_with("ERR"));
        assert!(c("\\begin{foo} a \\end{foo}").starts_with("ERR"));
        assert!(c("\\begin{pmatrix} a \\end{bmatrix}").starts_with("ERR"));
    }

    #[test]
    fn braces_and_limits() {
        assert_eq!(c("\\underbrace{a+b}_{n}"), "underbrace(a + b, n)");
        assert_eq!(c("\\overbrace{a+b}^{n}"), "overbrace(a + b, n)");
        assert_eq!(c("\\sum\\limits_{i}"), "limits(sum)_(i)");
        assert_eq!(c("{a+b}^2"), "a + b^(2)");
    }

    #[test]
    fn errors() {
        assert_eq!(c("\\foo"), "ERR unsupported LaTeX command `\\foo`");
        assert!(c("a}").starts_with("ERR"));
        assert!(c("{a").starts_with("ERR"));
        assert!(c("x^").starts_with("ERR"));
        assert!(c("x^2^3").starts_with("ERR"));
        assert_eq!(c(""), "");
        assert_eq!(c("   "), "");
    }

    #[test]
    fn never_panics_on_junk() {
        let mut seed = 12345u64;
        let alphabet: Vec<&str> = vec![
            "\\",
            "{",
            "}",
            "^",
            "_",
            "&",
            "\\\\",
            "\\left",
            "\\right",
            "\\begin{pmatrix}",
            "\\end{pmatrix}",
            "\\frac",
            "a",
            "1",
            " ",
            "é",
            "[",
            "]",
            "\\sqrt",
            "\\text{",
            "'",
            "$",
            "\\underbrace",
        ];
        for _ in 0..3000 {
            let mut s = String::new();
            for _ in 0..12 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                s.push_str(alphabet[(seed >> 33) as usize % alphabet.len()]);
            }
            let _ = latex_to_typst(&s);
        }
        let deep = "{".repeat(5000);
        assert!(latex_to_typst(&deep).is_err());
        let deep = "\\frac".repeat(5000);
        let _ = latex_to_typst(&deep);
    }
}
