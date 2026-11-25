//! Prints the block list of a Markdown file: `cargo run -p iron-sheets-md --example dump -- note.md`
//! (reads stdin when no path is given). Add `--src` to also print the first and last source
//! line of every block (handy to check line ranges).

use iron_sheets_md::*;
use std::io::Read;

fn short(s: &str, n: usize) -> String {
    let mut t: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        t.push('…');
    }
    t.replace('\n', "\\n")
}

fn inl(i: &Inlines) -> String {
    let mut s = format!("{:?}", short(&i.text, 70));
    let styled: Vec<String> = i
        .spans
        .iter()
        .filter(|sp| !sp.style.is_empty() || sp.link.is_some())
        .map(|sp| {
            let l = sp.link.map(|l| format!(" -> {:?}", i.links[l as usize].target)).unwrap_or_default();
            format!("[{}..{} {:?}{}]", sp.range.start, sp.range.end, sp.style, l)
        })
        .collect();
    if !styled.is_empty() {
        s.push(' ');
        s.push_str(&short(&styled.join(" "), 600));
    }
    for o in &i.objects {
        s.push_str(&format!(" <obj@{} {:?}>", o.at, o.kind));
    }
    s
}

fn main() {
    let mut text = String::new();
    let show_src = std::env::args().any(|a| a == "--src");
    match std::env::args().skip(1).find(|a| !a.starts_with("--")) {
        Some(p) => text = std::fs::read_to_string(&p).expect("read file"),
        None => {
            std::io::stdin().read_to_string(&mut text).expect("read stdin");
        }
    }
    let text = text.replace("\r\n", "\n");
    let t = std::time::Instant::now();
    let doc = parse(&text);
    let el = t.elapsed();
    println!("{} blocks, {} lines, parsed in {:?}", doc.blocks.len(), doc.line_count, el);
    for (i, b) in doc.blocks.iter().enumerate() {
        let desc = match &b.kind {
            BlockKind::Paragraph(x) => format!("Paragraph {}", inl(x)),
            BlockKind::Heading { level, content, slug } => {
                format!("Heading h{level} #{slug} {}", inl(content))
            }
            BlockKind::Code { lang, text } => format!("Code lang={lang:?} {:?}", short(text, 50)),
            BlockKind::Math { source } => format!("Math {:?}", short(source, 50)),
            BlockKind::Typst { source } => format!("Typst {:?}", short(source, 50)),
            BlockKind::Table { aligns, header, rows } => {
                format!("Table aligns={aligns:?} header=[{}] rows={}", header.iter().map(|h| short(&h.text, 20)).collect::<Vec<_>>().join(" | "), rows.len())
            }
            BlockKind::Rule => "Rule".into(),
            BlockKind::Image { src, alt, title, wiki } => {
                format!("Image src={src:?} alt={alt:?} title={title:?} wiki={wiki}")
            }
            BlockKind::Embed { target } => format!("Embed {target:?}"),
            BlockKind::Html { text } => format!("Html {:?}", short(text, 60)),
            BlockKind::FrontMatter { text } => format!("FrontMatter {:?}", short(text, 60)),
        };
        let cont: Vec<String> = b
            .containers
            .iter()
            .map(|c| {
                let k = match &c.kind {
                    ContainerKind::Quote => "Quote".to_string(),
                    ContainerKind::Alert { kind, title } => format!("Alert({kind:?},{title:?})"),
                    ContainerKind::ListItem { ordered, task } => match (ordered, task) {
                        (Some(n), None) => format!("Item#{n}"),
                        (None, None) => "Item".to_string(),
                        (o, Some(t)) => format!("Task{}@{}{}", if t.checked { "[x]" } else { "[ ]" }, t.offset, o.map(|n| format!("#{n}")).unwrap_or_default()),
                    },
                    ContainerKind::Footnote { label } => format!("Footnote({label})"),
                };
                format!("{k}:{}{}", c.id, if c.first { "*" } else { "" })
            })
            .collect();
        println!("{i:>4} L{}..{} {:?} {:016x} [{}] {desc}", b.lines.start, b.lines.end, b.spacing, b.hash, cont.join(" > "));
        if show_src {
            let lines: Vec<&str> = text.split('\n').collect();
            let first = lines.get(b.lines.start as usize).copied().unwrap_or("<out of range>");
            let last = lines.get(b.lines.end.saturating_sub(1) as usize).copied().unwrap_or("<out of range>");
            println!("       first: {:?}   last: {:?}", short(first, 60), short(last, 60));
        }
    }
}
