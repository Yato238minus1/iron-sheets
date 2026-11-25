//! Prints the licenses of the two-face syntax definitions the binary embeds (themes are not used).
//! Its output feeds the syntax-definitions section of `THIRD-PARTY-LICENSES.txt`.
fn main() {
    let mut md = String::new();
    for l in two_face::acknowledgement::listing().for_syntaxes() {
        l.write_md(&mut md);
    }
    print!("{md}");
}
