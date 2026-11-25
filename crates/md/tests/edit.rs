use iron_sheets_md::*;

/// Parses a fixture where `|` marks the cursor, or `«...»` a selection.
fn fx(s: &str) -> (String, std::ops::Range<usize>) {
    if let Some(a) = s.find('«') {
        let t = s.replacen('«', "", 1);
        let b = t.find('»').unwrap();
        let t = t.replacen('»', "", 1);
        (t, a..b)
    } else {
        let a = s.find('|').unwrap();
        (s.replacen('|', "", 1), a..a)
    }
}

/// Renders the result of an op with the selection markers, for readable asserts.
fn show(text: &str, op: &EditOp) -> String {
    let t = op.apply(text);
    assert!(t.is_char_boundary(op.select.start) && t.is_char_boundary(op.select.end), "{op:?}");
    let (s, e) = (op.select.start, op.select.end);
    if s == e { format!("{}|{}", &t[..s], &t[s..]) } else { format!("{}«{}»{}", &t[..s], &t[s..e], &t[e..]) }
}

fn enter(s: &str) -> Option<String> {
    let (t, c) = fx(s);
    on_enter(&t, c.start).map(|op| show(&t, &op))
}

fn tab(s: &str, outdent: bool) -> Option<String> {
    let (t, c) = fx(s);
    on_tab(&t, c, outdent).map(|op| show(&t, &op))
}

fn wrap(s: &str, m: &str) -> String {
    let (t, c) = fx(s);
    show(&t, &toggle_wrap(&t, c, m))
}

// ---------------------------------------------------------------- on_enter

#[test]
fn enter_continues_bullets() {
    assert_eq!(enter("- item|").unwrap(), "- item\n- |");
    assert_eq!(enter("* item|").unwrap(), "* item\n* |");
    assert_eq!(enter("+ item|").unwrap(), "+ item\n+ |");
    assert_eq!(enter("  - nested|").unwrap(), "  - nested\n  - |");
    assert_eq!(enter("\t- tab|").unwrap(), "\t- tab\n\t- |");
    assert_eq!(enter("-   wide|").unwrap(), "-   wide\n-   |");
}

#[test]
fn enter_splits_the_line() {
    assert_eq!(enter("- ab|cd").unwrap(), "- ab\n- |cd");
    assert_eq!(enter("- |cd").unwrap(), "- \n- |cd");
    assert_eq!(enter("line one\n- ab|cd\nafter").unwrap(), "line one\n- ab\n- |cd\nafter");
}

#[test]
fn enter_ordered_increments() {
    assert_eq!(enter("1. a|").unwrap(), "1. a\n2. |");
    assert_eq!(enter("9. a|").unwrap(), "9. a\n10. |");
    assert_eq!(enter("3) a|").unwrap(), "3) a\n4) |");
    assert_eq!(enter("  12. a|").unwrap(), "  12. a\n  13. |");
}

#[test]
fn enter_tasks_and_quotes() {
    assert_eq!(enter("- [ ] todo|").unwrap(), "- [ ] todo\n- [ ] |");
    assert_eq!(enter("- [x] done|").unwrap(), "- [x] done\n- [ ] |");
    assert_eq!(enter("1. [X] done|").unwrap(), "1. [X] done\n2. [ ] |");
    assert_eq!(enter("> quote|").unwrap(), "> quote\n> |");
    assert_eq!(enter("> > deep|").unwrap(), "> > deep\n> > |");
    assert_eq!(enter(">> deep|").unwrap(), ">> deep\n>> |");
    assert_eq!(enter("> - item|").unwrap(), "> - item\n> - |");
    assert_eq!(enter("> 1. item|").unwrap(), "> 1. item\n> 2. |");
}

#[test]
fn enter_on_empty_item_removes_marker() {
    assert_eq!(enter("- |").unwrap(), "|");
    assert_eq!(enter("-|").unwrap(), "|");
    assert_eq!(enter("  - |").unwrap(), "|");
    assert_eq!(enter("1. |").unwrap(), "|");
    assert_eq!(enter("- [ ] |").unwrap(), "|");
    assert_eq!(enter("> |").unwrap(), "|");
    assert_eq!(enter("> - |").unwrap(), "> |");
    assert_eq!(enter("a\n- x\n- |\nb").unwrap(), "a\n- x\n|\nb");
}

#[test]
fn enter_default_cases() {
    assert_eq!(enter("plain text|"), None);
    assert_eq!(enter("|"), None);
    assert_eq!(enter("|- item"), None); // cursor before the marker
    assert_eq!(enter("-| item"), None); // inside the marker
    assert_eq!(enter("**bold** text|"), None);
    assert_eq!(enter("-nospace|"), None);
    assert_eq!(enter("1.nospace|"), None);
    assert_eq!(enter("2024|"), None);
    assert_eq!(enter("# heading|"), None);
}

#[test]
fn enter_multibyte() {
    assert_eq!(enter("- é日本😀|").unwrap(), "- é日本😀\n- |");
    assert_eq!(enter("- é日|本😀").unwrap(), "- é日\n- |本😀");
    assert_eq!(enter("日本\n- 日本|\n語").unwrap(), "日本\n- 日本\n- |\n語");
    assert_eq!(enter("1. ünï|").unwrap(), "1. ünï\n2. |");
}

#[test]
fn enter_clamps_bad_offsets() {
    assert!(on_enter("- é", 100).is_some());
    assert!(on_enter("- é", 3).is_some()); // inside 'é' (bytes 2..4): clamped, no panic
    assert_eq!(on_enter("", 0), None);
    assert_eq!(on_enter("", 5), None);
}

// ---------------------------------------------------------------- on_tab

#[test]
fn tab_indents_list_item() {
    assert_eq!(tab("- item|", false).unwrap(), "  - item|");
    assert_eq!(tab("- it|em", false).unwrap(), "  - it|em");
    assert_eq!(tab("1. item|", false).unwrap(), "    1. item|");
    assert_eq!(tab("  - item|", false).unwrap(), "    - item|");
    assert_eq!(tab("|- item", false).unwrap(), "  |- item");
    assert_eq!(tab("a\n- x|\nb", false).unwrap(), "a\n  - x|\nb");
    assert_eq!(tab("- [ ] t|", false).unwrap(), "  - [ ] t|");
    assert_eq!(tab("> - q|", false).unwrap(), "  > - q|");
}

#[test]
fn tab_outdents_list_item() {
    assert_eq!(tab("  - item|", true).unwrap(), "- item|");
    assert_eq!(tab("    - item|", true).unwrap(), "  - item|");
    assert_eq!(tab("    1. item|", true).unwrap(), "1. item|");
    assert_eq!(tab(" - item|", true).unwrap(), "- item|");
    assert_eq!(tab("\t- item|", true).unwrap(), "- item|");
    assert_eq!(tab("  - it|em", true).unwrap(), "- it|em");
    assert_eq!(tab("  |- item", true).unwrap(), "|- item"); // caret in the removed region clamps
    assert_eq!(tab("- item|", true), None); // nothing to remove
}

#[test]
fn tab_plain_text_defaults() {
    assert_eq!(tab("plain|", false), None);
    assert_eq!(tab("|", false), None);
    assert_eq!(tab("«sel»ected", false), None); // single-line selection in plain text
    assert_eq!(tab("plain|", true), None);
    assert_eq!(tab("  plain|", true).unwrap(), "plain|"); // outdent still works on indentation
}

#[test]
fn tab_multi_line_selection() {
    assert_eq!(tab("«a\nb\nc»", false).unwrap(), "«  a\n  b\n  c»");
    assert_eq!(tab("a\n«b\nc»\nd", false).unwrap(), "a\n«  b\n  c»\nd");
    assert_eq!(tab("a«b\nc»d", false).unwrap(), "  a«b\n  c»d");
    assert_eq!(tab("«  a\n    b»", true).unwrap(), "«a\n  b»");
    assert_eq!(tab("«- a\n1. b»", false).unwrap(), "«  - a\n    1. b»");
    // blank lines are not padded
    assert_eq!(tab("«a\n\nb»", false).unwrap(), "«  a\n\n  b»");
    // a selection that ends at the start of a line does not touch that line
    assert_eq!(tab("«a\nb\n»c", false).unwrap(), "«  a\n  b\n»c");
    // nothing to outdent
    assert_eq!(tab("«a\nb»", true), None);
}

#[test]
fn tab_multibyte() {
    assert_eq!(tab("- é日|", false).unwrap(), "  - é日|");
    assert_eq!(tab("«日本\n語»", false).unwrap(), "«  日本\n  語»");
    assert_eq!(tab("  - 😀|", true).unwrap(), "- 😀|");
}

#[test]
fn tab_selection_is_remapped_inside_lines() {
    assert_eq!(tab("a«bc\nde»f", false).unwrap(), "  a«bc\n  de»f");
    assert_eq!(tab("  a«bc\n  de»f", true).unwrap(), "a«bc\nde»f");
}

// ---------------------------------------------------------------- toggle_wrap

#[test]
fn wrap_selection() {
    assert_eq!(wrap("a «bold» b", "**"), "a **«bold»** b");
    assert_eq!(wrap("a «it» b", "*"), "a *«it»* b");
    assert_eq!(wrap("a «s» b", "~~"), "a ~~«s»~~ b");
    assert_eq!(wrap("a «c» b", "`"), "a `«c»` b");
    assert_eq!(wrap("a «h» b", "=="), "a ==«h»== b");
    assert_eq!(wrap("«»", "**"), "**|**");
}

#[test]
fn unwrap_selection() {
    assert_eq!(wrap("a «**bold**» b", "**"), "a «bold» b");
    assert_eq!(wrap("a **«bold»** b", "**"), "a «bold» b");
    assert_eq!(wrap("a «`c`» b", "`"), "a «c» b");
    assert_eq!(wrap("a ~~«s»~~ b", "~~"), "a «s» b");
}

#[test]
fn wrap_roundtrip() {
    for m in ["**", "*", "~~", "`", "=="] {
        let (t, c) = fx("x «word» y");
        let op1 = toggle_wrap(&t, c, m);
        let t1 = op1.apply(&t);
        let op2 = toggle_wrap(&t1, op1.select.clone(), m);
        assert_eq!(op2.apply(&t1), t, "marker {m}");
        assert_eq!(op2.select, 2..6, "marker {m}");
    }
}

#[test]
fn italic_vs_bold_runs() {
    // italic on bold text adds a layer instead of stripping one
    assert_eq!(wrap("«**b**»", "*"), "*«**b**»*");
    assert_eq!(wrap("*«**b**»*", "*"), "«**b**»");
    // and removes only its own layer from bold-italic
    assert_eq!(wrap("***«b»***", "*"), "**«b»**");
    assert_eq!(wrap("***«b»***", "**"), "*«b»*");
    assert_eq!(wrap("**«b»**", "*"), "***«b»***");
}

#[test]
fn wrap_empty_selection_uses_word() {
    assert_eq!(wrap("a wo|rd b", "**"), "a **wo|rd** b");
    assert_eq!(wrap("a word| b", "**"), "a **word|** b");
    assert_eq!(wrap("a |word b", "**"), "a **|word** b");
    assert_eq!(wrap("a **wo|rd** b", "**"), "a wo|rd b");
    assert_eq!(wrap("a **|word** b", "**"), "a |word b");
    assert_eq!(wrap("a `wo|rd` b", "`"), "a wo|rd b");
}

#[test]
fn wrap_empty_selection_without_word() {
    assert_eq!(wrap("a | b", "**"), "a **|** b");
    assert_eq!(wrap("|", "*"), "*|*");
    assert_eq!(wrap("a **|** b", "**"), "a | b"); // toggling inside an empty pair removes it
    assert_eq!(wrap("a.|", "`"), "a.`|`");
}

#[test]
fn wrap_multibyte() {
    assert_eq!(wrap("a «é日本» b", "**"), "a **«é日本»** b");
    assert_eq!(wrap("日本|語 x", "~~"), "~~日本|語~~ x");
    assert_eq!(wrap("😀 «😀»", "=="), "😀 ==«😀»==");
    assert_eq!(wrap("**é|**", "**"), "é|");
}

#[test]
fn wrap_edges() {
    assert_eq!(wrap("«abc»", "**"), "**«abc»**");
    assert_eq!(wrap("abc|", "**"), "**abc|**");
    assert_eq!(wrap("|abc", "**"), "**|abc**");
    let op = toggle_wrap("abc", 1..1, "");
    assert_eq!(op.apply("abc"), "abc");
    // reversed / out of range selections are tolerated
    let op = toggle_wrap("abc", std::ops::Range { start: 99, end: 2 }, "*");
    assert_eq!(op.apply("abc"), "ab*c*");
}

// ---------------------------------------------------------------- toggle_task

#[test]
fn task_toggle() {
    let t = "- [ ] a\n- [x] b\n- [X] c\n";
    let op = toggle_task(t, 3).unwrap();
    assert_eq!(op.apply(t), "- [x] a\n- [x] b\n- [X] c\n");
    assert_eq!(op.range, 3..4);
    let op = toggle_task(t, 11).unwrap();
    assert_eq!(op.apply(t), "- [ ] a\n- [ ] b\n- [X] c\n");
    let op = toggle_task(t, 19).unwrap();
    assert_eq!(op.apply(t), "- [ ] a\n- [x] b\n- [ ] c\n");
    // caret ends after the char
    assert_eq!(toggle_task(t, 3).unwrap().select, 4..4);
}

#[test]
fn task_toggle_rejects_non_tasks() {
    let t = "- [ ] a é\n";
    assert_eq!(toggle_task(t, 0), None);
    assert_eq!(toggle_task(t, 1), None);
    assert_eq!(toggle_task(t, 4), None);
    assert_eq!(toggle_task(t, 999), None);
    assert_eq!(toggle_task("", 0), None);
    assert_eq!(toggle_task("é[ ]", 2), None); // not a boundary / no bracket before
    assert!(toggle_task("é[ ]", 3).is_some());
    // multibyte symbol (relaxed tasks): flips to unchecked
    let t = "- [é] a";
    let op = toggle_task(t, 3).unwrap();
    assert_eq!(op.apply(t), "- [ ] a");
}

// ---------------------------------------------------------------- insert_link

#[test]
fn link_insertion() {
    let (t, c) = fx("see «this» now");
    let op = insert_link(&t, c);
    assert_eq!(show(&t, &op), "see [this](|) now");
    let (t, c) = fx("see | now");
    assert_eq!(show(&t, &insert_link(&t, c)), "see [|]() now");
    let (t, c) = fx("«日本»");
    assert_eq!(show(&t, &insert_link(&t, c)), "[日本](|)");
    assert_eq!(show("", &insert_link("", 0..0)), "[|]()");
    assert_eq!(show("x", &insert_link("x", 1..1)), "x[|]()");
}

// ---------------------------------------------------------------- wikilink_context

#[test]
fn wikilink_ctx() {
    let t = "see [[Not";
    assert_eq!(wikilink_context(t, t.len()), Some(WikiCtx { start: 6, query: "Not" }));
    let t = "[[";
    assert_eq!(wikilink_context(t, 2), Some(WikiCtx { start: 2, query: "" }));
    let t = "![[im";
    assert_eq!(wikilink_context(t, 5), Some(WikiCtx { start: 3, query: "im" }));
    // closed link before the cursor
    assert_eq!(wikilink_context("[[a]] b", 7), None);
    // after | or #
    assert_eq!(wikilink_context("[[a|b", 5), None);
    assert_eq!(wikilink_context("[[a#h", 5), None);
    // previous line does not count
    assert_eq!(wikilink_context("[[a\nb", 5), None);
    // cursor in the middle of an existing link body
    assert_eq!(wikilink_context("[[Note]] x", 4), Some(WikiCtx { start: 2, query: "No" }));
    // second link on the line
    let t = "[[a]] and [[b";
    assert_eq!(wikilink_context(t, t.len()), Some(WikiCtx { start: 12, query: "b" }));
    // multibyte query
    let t = "x [[é日";
    assert_eq!(wikilink_context(t, t.len()), Some(WikiCtx { start: 4, query: "é日" }));
    // single bracket, empty text, bad offsets
    assert_eq!(wikilink_context("[a", 2), None);
    assert_eq!(wikilink_context("", 0), None);
    assert_eq!(wikilink_context("[[é", 3), Some(WikiCtx { start: 2, query: "" })); // mid-char clamps
    assert!(wikilink_context("[[é", 99).is_some());
}

// ---------------------------------------------------------------- in_math

fn m(s: &str) -> bool {
    let (t, c) = fx(s);
    in_math(&t, c.start)
}

#[test]
fn math_inline() {
    assert!(m("a $x|$ b"));
    assert!(m("a $|x$ b"));
    assert!(m("a $x+y|"));
    assert!(!m("a |$x$ b"));
    assert!(!m("a $x$| b"));
    assert!(!m("a $x$ b|"));
    assert!(m("$x$ and $y|$"));
    assert!(!m("no math here|"));
    assert!(!m("|"));
    assert!(!m("costs $5 and $6|"));
    assert!(!m("lone $ |sign"));
    assert!(m("esc \\$ then $x|$"));
    assert!(!m("esc \\$x|"));
}

#[test]
fn math_display() {
    assert!(m("$$x|$$"));
    assert!(m("$$\nx^2|\n$$"));
    assert!(m("$$\nline1\nline2|\n$$"));
    assert!(!m("$$\nx\n$$\nafter|"));
    assert!(m("text $$ a|b $$ more"));
    assert!(!m("text $$ ab $$ m|ore"));
    assert!(!m("$|$ x")); // between the two dollars
    // single $ inside display is literal
    assert!(m("$$\na $ b|\n$$"));
}

#[test]
fn math_inline_does_not_span_lines() {
    assert!(!m("a $x\nb|"));
    assert!(m("a $x\n$y|$"));
}

#[test]
fn math_multibyte() {
    assert!(m("日本 $é日|$ 語"));
    assert!(!m("日本 $é日$ |語"));
    assert!(in_math("é$x", 99)); // clamps
    assert!(!in_math("é$x$", 1)); // mid-char clamps to 0
}

// ---------------------------------------------------------------- headings

#[test]
fn heading_detection() {
    assert_eq!(heading_line("# a", 0), Some(1));
    assert_eq!(heading_line("###### a", 3), Some(6));
    assert_eq!(heading_line("####### a", 0), None);
    assert_eq!(heading_line("#a", 0), None);
    assert_eq!(heading_line("#", 0), Some(1));
    assert_eq!(heading_line("  ## a", 0), Some(2));
    assert_eq!(heading_line("    # code", 0), None);
    assert_eq!(heading_line("a\n## b\nc", 4), Some(2));
    assert_eq!(heading_line("a\n## b\nc", 1), None);
    assert_eq!(heading_line("", 0), None);
    assert_eq!(heading_line("# é日", 99), Some(1));
}

fn sh(s: &str, level: u8) -> String {
    let (t, c) = fx(s);
    show(&t, &set_heading(&t, c.start, level))
}

#[test]
fn heading_set_toggle() {
    assert_eq!(sh("title|", 1), "# title|");
    assert_eq!(sh("ti|tle", 2), "## ti|tle");
    assert_eq!(sh("|title", 3), "### |title");
    assert_eq!(sh("# ti|tle", 2), "## ti|tle");
    assert_eq!(sh("## ti|tle", 2), "ti|tle"); // same level again removes it
    assert_eq!(sh("# title|", 0), "title|");
    assert_eq!(sh("title|", 0), "title|");
    assert_eq!(sh("title|", 9), "###### title|");
    assert_eq!(sh("a\nb|\nc", 1), "a\n# b|\nc");
    assert_eq!(sh("|", 1), "# |");
    assert_eq!(sh("#| x", 1), "|x");
    assert_eq!(sh("#|x y", 2), "## #|x y"); // `#x` is a tag, not a heading: prefix added
}

#[test]
fn heading_multibyte() {
    assert_eq!(sh("日本|語", 1), "# 日本|語");
    assert_eq!(sh("# 日本|語", 3), "### 日本|語");
    assert_eq!(sh("##  é|", 1), "# é|");
    let op = set_heading("é", 1, 1); // mid-char cursor clamps
    assert_eq!(op.apply("é"), "# é");
}
