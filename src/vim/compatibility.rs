//! Compare every documented editor command with an isolated Neovim reference session.

use super::{
    Action, Mode, Register, Vim,
    register::Kind,
    run_filter,
    shortcuts::{CATALOG, NAMED_KEYS},
};
use crate::document::Document;
use serde::{Deserialize, Serialize};
use std::{fs, path::Path, process::Command};
use unicode_segmentation::UnicodeSegmentation;

/// Buffer used by the counted motions and scrolling examples.
const ROWS: &str = "  01 abcdefgh ijklmnop\n  02 abcdefgh ijklmnop\n  03 abcdefgh ijklmnop\n  04 abcdefgh ijklmnop\n  05 abcdefgh ijklmnop\n  06 abcdefgh ijklmnop\n  07 abcdefgh ijklmnop\n  08 abcdefgh ijklmnop\n  09 abcdefgh ijklmnop\n  10 abcdefgh ijklmnop\n  11 abcdefgh ijklmnop\n  12 abcdefgh ijklmnop\n  13 abcdefgh ijklmnop\n  14 abcdefgh ijklmnop\n  15 abcdefgh ijklmnop\n  16 abcdefgh ijklmnop\n  17 abcdefgh ijklmnop\n  18 abcdefgh ijklmnop\n  19 abcdefgh ijklmnop\n  20 abcdefgh ijklmnop\n  21 abcdefgh ijklmnop\n  22 abcdefgh ijklmnop\n  23 abcdefgh ijklmnop\n  24 abcdefgh ijklmnop\n  25 abcdefgh ijklmnop\n  26 abcdefgh ijklmnop\n  27 abcdefgh ijklmnop\n  28 abcdefgh ijklmnop\n  29 abcdefgh ijklmnop\n  30 abcdefgh ijklmnop\n  31 abcdefgh ijklmnop\n  32 abcdefgh ijklmnop\n  33 abcdefgh ijklmnop\n  34 abcdefgh ijklmnop\n  35 abcdefgh ijklmnop\n  36 abcdefgh ijklmnop\n  37 abcdefgh ijklmnop\n  38 abcdefgh ijklmnop\n  39 abcdefgh ijklmnop\n  40 abcdefgh ijklmnop";
/// Indented words for motion, operator, put and insertion examples.
const WORDS: &str = "  alpha beta gamma delta epsilon zeta\n  one two three four five six\n  red green blue\n  last word";
/// Sentence punctuation and an empty paragraph separator.
const PARAGRAPHS: &str = "First sentence.  Second? Third!\n\nNext paragraph.\nFinal line";
/// Nested class and method braces for [m and ]m.
const METHODS: &str =
    "class Example {\n  void first() {\n    run();\n  }\n  void second() {\n    run();\n  }\n}";
/// Case viewport height, matched by the headless Neovim window.
const HEIGHT: usize = 8;

/// Reproduce one help example or an edge case using real modal input.
#[derive(Serialize)]
struct Case {
    /// Help key whose behavior is exercised.
    shortcut: &'static str,
    /// Text, keys and zero-based initial caret in bytes.
    text: &'static str,
    keys: &'static str,
    row: usize,
    col: usize,
    /// Explicit initial top for screen-relative commands.
    top: Option<usize>,
    /// Both engines close a scratch file and report the disk result.
    close: bool,
}

impl Case {
    /// Build a case starting at the beginning of `text`.
    const fn new(shortcut: &'static str, text: &'static str, keys: &'static str) -> Self {
        Self {
            shortcut,
            text,
            keys,
            row: 0,
            col: 0,
            top: None,
            close: false,
        }
    }

    /// Start the caret at `row` and byte `col`.
    const fn at(mut self, row: usize, col: usize) -> Self {
        self.row = row;
        self.col = col;
        self
    }

    /// Also compare the viewport, beginning at `top`.
    const fn view(mut self, top: usize) -> Self {
        self.top = Some(top);
        self
    }

    /// Observe the file on disk after the requested close.
    const fn closing(mut self) -> Self {
        self.close = true;
        self
    }
}

/// Include the listed command, a usable example and relevant mode/count variations.
fn cases() -> Vec<Case> {
    use Case as C;
    vec![
        C::new("h", WORDS, "3h").at(0, 7),
        C::new("j", ROWS, "12j").at(4, 7),
        C::new("k", ROWS, "5k").at(13, 7),
        C::new("l", WORDS, "3l").at(0, 3),
        C::new("w", WORDS, "3w").at(0, 2),
        C::new("b", WORDS, "2b").at(0, 16),
        C::new("e", WORDS, "2e").at(0, 4),
        C::new("0", WORDS, "0").at(0, 8),
        C::new("^", WORDS, "^").at(0, 8),
        C::new("$", WORDS, "2$").at(0, 8),
        C::new("g", WORDS, "gg").at(2, 5),
        C::new("gg", WORDS, "3gg").at(0, 7),
        C::new("G", WORDS, "G").at(0, 7),
        C::new("G", WORDS, "3G").at(0, 7),
        C::new("G", "one\ntwo\n", "G").at(0, 1),
        C::new("i", WORDS, "3i中<Esc>").at(0, 2),
        C::new("a", WORDS, "a中<Esc>").at(0, 2),
        C::new("I", WORDS, "I中<Esc>").at(0, 8),
        C::new("A", WORDS, "A中<Esc>").at(0, 8),
        C::new("o", WORDS, "2o中<Esc>").at(0, 8),
        C::new("O", WORDS, "2O中<Esc>").at(0, 8),
        C::new("x", "a中猫xyz", "3x").at(0, 1),
        C::new("X", WORDS, "2X").at(0, 6),
        C::new("s", WORDS, "s中<Esc>").at(0, 2),
        C::new("S", WORDS, "Schanged<Esc>").at(1, 4),
        C::new("D", WORDS, "2D").at(0, 8),
        C::new("C", WORDS, "Cchanged<Esc>").at(0, 8),
        C::new("Y", WORDS, "Y").at(0, 8),
        C::new("d", WORDS, "2d2w").at(0, 2),
        C::new("c", WORDS, "ciwchanged<Esc>").at(0, 4),
        C::new("y", WORDS, "y2w").at(0, 2),
        C::new("dd", WORDS, "2dd").at(1, 8),
        C::new("dd", WORDS, "dd").at(3, 7),
        C::new("cc", WORDS, "ccchanged<Esc>").at(1, 8),
        C::new("yy", WORDS, "2yy").at(0, 8),
        C::new("dw", "first\nsecond", "dw"),
        C::new("cw", WORDS, "cwchanged<Esc>").at(0, 6),
        C::new("yw", WORDS, "yw").at(0, 2),
        C::new("ciw", "zero café last", "ciw中<Esc>").at(0, 7),
        C::new("ciw", "alpha beta", "2ciwX<Esc>").at(0, 2),
        C::new("ciw", "  alpha\n\nbeta", "2ciwX<Esc>").at(0, 4),
        C::new("diw", "abc.def next", "2diw"),
        C::new("diw", "alpha   beta gamma", "2diw").at(0, 6),
        C::new("diw", "alpha\nbeta gamma", "2diw").at(0, 2),
        C::new("yiw", WORDS, "yiw").at(0, 4),
        C::new("yiw", "alpha  \nbeta", "2yiw").at(0, 2),
        C::new("yiw", "alpha\n  beta", "2yiw").at(0, 2),
        C::new("yiw", "xx alpha\n\nbeta", "2yiw").at(0, 5),
        C::new("yiw", "alpha", "2yiw").at(0, 2),
        C::new("yiw", "alpha   beta gamma", "3yiw").at(0, 2),
        C::new("yiw", "alpha beta gamma", "v2iw").at(0, 2),
        C::new("d$", WORDS, "d$").at(1, 6),
        C::new("c$", WORDS, "c$changed<Esc>").at(1, 6),
        C::new("y$", WORDS, "y$").at(1, 6),
        C::new("p", WORDS, "yyjp").at(0, 4),
        C::new("P", "abcde", "yl$P"),
        C::new("p", "abcde", "yl$vp"),
        C::new("v", WORDS, "v2l").at(0, 4),
        C::new("V", WORDS, "Vj").at(0, 4),
        C::new("u", WORDS, "3xu").at(0, 4),
        C::new("U", WORDS, "xjU").at(0, 4),
        C::new("J", WORDS, "3J").at(0, 4),
        C::new("/", "word text word\nlast word", "/word<CR>"),
        C::new("?", "word text word\nlast word", "?word<CR>").at(1, 5),
        C::new("n", "word text word\nlast word", "?word<CR>n").at(1, 5),
        C::new("N", "word text word\nlast word", "?word<CR>N").at(1, 5),
        C::new(":", "old old\nold other", ":%s/old/new/g<CR>").at(0, 4),
        C::new(".", "a b c d e f g h", "d2w3."),
        C::new(".", "one two three\nfour five six", "ciw中<Esc>j0."),
        C::new("q", WORDS, "qadwq").at(0, 2),
        C::new("@", WORDS, "qaxq@a@@").at(0, 2),
        C::new("Q", WORDS, "qaxqj0Q").at(0, 2),
        C::new("m", WORDS, "maj`a").at(0, 6),
        C::new("'", WORDS, "maj'a").at(0, 6),
        C::new("`", WORDS, "maj`a").at(0, 6),
        C::new("r", "café abc", "2r中").at(0, 1),
        C::new("R", "🙂a cat", "R中文<BS><Esc>"),
        C::new("f", "a,ba,ca,da", "2fa"),
        C::new("F", "a,ba,ca,da", "2Fa").at(0, 9),
        C::new("t", "a,ba,ca,da", "2t,"),
        C::new("T", "a,ba,ca,da", "2T,").at(0, 9),
        C::new(";", "a,ba,ca,da", "t,;"),
        C::new(",", "a,ba,ca,da", "f,;,").at(0, 2),
        C::new("%", "call([nested(x), y]) tail\nnext", "%"),
        C::new("%", ROWS, "50%").at(0, 7),
        C::new("*", "cat cats cat\ncat", "*").at(0, 1),
        C::new("#", "cat cats cat\ncat", "#").at(0, 10),
        C::new("(", PARAGRAPHS, "2(").at(0, 28),
        C::new(")", PARAGRAPHS, "2)").at(0, 1),
        C::new("{", PARAGRAPHS, "{").at(3, 1),
        C::new("}", PARAGRAPHS, "}").at(1, 0),
        C::new("<", WORDS, "<lt><lt>").at(1, 6),
        C::new("<<", WORDS, "2<lt><lt>").at(1, 6),
        C::new(">", WORDS, "2>>").at(1, 6),
        C::new(">>", WORDS, "2>>").at(1, 9),
        C::new("=", "if (true) {\nx();\n  }", "=ip"),
        C::new("=ip", "if (true) {\nx();\n  }\n\n// next", "=ip"),
        C::new("-", WORDS, "-").at(1, 6),
        C::new("+", WORDS, "+").at(1, 6),
        C::new("_", WORDS, "3_").at(0, 6),
        C::new("|", "abc\t猫yz", "7|"),
        C::new("~", "aBçZ", "3~"),
        C::new("&", "old old\nold old", ":s/old/new/g<CR>j&"),
        C::new("!", "zeta\nalpha\nrest", "2!!sort<CR>"),
        C::new("!!", "zeta\nalpha\nrest", "2!!sort<CR>"),
        C::new("\"", WORDS, "\"ayyj\"ap").at(0, 4),
        C::new("[", METHODS, "[m").at(5, 2),
        C::new("]", METHODS, "]m").at(0, 0),
        C::new("[m", METHODS, "[m").at(2, 4),
        C::new("[m", METHODS, "2[m").at(5, 4),
        C::new("]m", METHODS, "2]m").at(0, 0),
        C::new("]m", METHODS, "]m").at(2, 4),
        C::new("z", ROWS, "zz").at(14, 6).view(11),
        C::new("zz", ROWS, "zz").at(15, 6).view(11),
        C::new("zz", ROWS, "12zz").at(15, 6).view(11),
        C::new("zt", ROWS, "zt").at(14, 6).view(11),
        C::new("zt", ROWS, "40zt").at(14, 6).view(11),
        C::new("zb", ROWS, "zb").at(14, 6).view(11),
        C::new("z", ROWS, "z<CR>").at(14, 6).view(11),
        C::new("z", ROWS, "z.").at(14, 6).view(11),
        C::new("z", ROWS, "z-").at(14, 6).view(11),
        C::new("z", ROWS, "z+").at(14, 6).view(11),
        C::new("z", ROWS, "z^").at(14, 6).view(11),
        C::new("z", ROWS, "16z^").at(14, 6).view(11),
        C::new("z", ROWS, "40z^").at(14, 6).view(11),
        C::new("\\", WORDS, "\\").at(0, 5),
        C::new("B", " foo-bar baz.qux end", "B").at(0, 13),
        C::new("W", " foo-bar baz.qux end", "W").at(0, 3),
        C::new("E", " foo-bar baz.qux end", "E").at(0, 3),
        C::new("H", ROWS, "H").at(14, 6).view(11),
        C::new("M", ROWS, "M").at(15, 6).view(11),
        C::new("L", ROWS, "L").at(14, 6).view(11),
        C::new("Z", WORDS, "xZZ").at(0, 2).closing(),
        C::new("Z", WORDS, "xZQ").at(0, 2).closing(),
        C::new("ZZ", WORDS, "ZZ").at(0, 2).closing(),
        C::new("ZQ", WORDS, "xZQ").at(0, 2).closing(),
        C::new("Ctrl+s", WORDS, "<C-s>").at(0, 4),
        C::new("Ctrl+r", WORDS, "3xu<C-r>").at(0, 4),
        C::new("Ctrl+r", WORDS, "\"ayiwji<C-r>a<Esc>").at(0, 2),
        C::new("Ctrl+w", "one tail", "A two three<C-w><Esc>"),
        C::new("Ctrl+w", "one tail", "A two three<C-w><Esc>u"),
        C::new("Ctrl+u", "one tail", "A inserted<C-u><Esc>"),
        C::new("Ctrl+u", ROWS, "<C-u>").at(14, 6).view(11),
        C::new("Ctrl+d", ROWS, "<C-d>").at(14, 6).view(11),
        C::new("Ctrl+d", ROWS, "2<C-d><C-d>").at(14, 6).view(11),
        C::new("Ctrl+v", "abcde\nABCDE", "<C-v>jl").at(0, 1),
        C::new("Ctrl+v", "abcde\nABCDE", "<C-v>jlcy<Esc>").at(0, 1),
        C::new("Ctrl+[", WORDS, "i中<C-[>").at(0, 2),
        C::new("Ctrl+h", WORDS, "<C-h>").at(0, 5),
        C::new("Ctrl+l", WORDS, "<C-l>").at(0, 5),
        C::new("Ctrl+j", WORDS, "<C-j>").at(0, 5),
        C::new("Ctrl+k", WORDS, "<C-k>").at(0, 5),
        C::new("Ctrl+a", "dec -9 hex 0x0f bin 0b010", "3<C-a>"),
        C::new("Ctrl+a", "dec -9 hex 0x0f bin 0b010", "<C-a>").at(0, 11),
        C::new("Ctrl+x", "dec -9 hex 0x0f bin 0b010", "2<C-x>").at(0, 20),
        C::new("Ctrl+b", ROWS, "<C-b>").at(14, 6).view(11),
        C::new("Ctrl+f", ROWS, "<C-f>").at(14, 6).view(11),
        C::new("Ctrl+e", ROWS, "<C-e>").at(14, 6).view(11),
        C::new("Ctrl+y", ROWS, "<C-y>").at(14, 6).view(11),
        C::new("Ctrl+o", WORDS, "G<C-o>").at(0, 4),
        C::new("Ctrl+i", WORDS, "G<C-o><C-i>").at(0, 4),
        C::new("Insert", "cat", "<Insert>X<Insert>Y<Esc>"),
        C::new("Insert", "abcdefg", "RX<Insert>Y<Insert><BS><BS>Z<Esc>"),
        C::new("Insert", "abcdefg", "2iX<Insert>Y<Esc>"),
        C::new("Insert", "abcdefg", "2RX<Insert>Y<Esc>"),
        C::new("Insert", "abcdefg", "2<Insert>X<Insert>Y<Insert>Z<Esc>"),
        C::new("Insert", "abcd abcd", "iX<Insert>Y<Insert>Z<Esc>w."),
        C::new("Insert", "abcd", "iX<Insert>Y<Insert>Z<Esc>u"),
        C::new("Insert", "abcde", "vl<Insert>"),
        C::new("Backspace", "", "iab<BS><Esc>"),
        C::new("Delete", "cat", "<Del>"),
        C::new("Enter", WORDS, "<CR>").at(0, 6),
        C::new("Tab", WORDS, "G<C-o><Tab>").at(0, 4),
        C::new("Home", WORDS, "<Home>").at(0, 6),
        C::new("End", WORDS, "<End>").at(0, 6),
        C::new("PageUp", ROWS, "<PageUp>").at(14, 6).view(11),
        C::new("PageDown", ROWS, "<PageDown>").at(14, 6).view(11),
        C::new("Left", WORDS, "2<Left>").at(0, 6),
        C::new("Right", WORDS, "2<Right>").at(0, 6),
        C::new("Up", ROWS, "2<Up>").at(14, 6),
        C::new("Down", ROWS, "2<Down>").at(14, 6),
        C::new("1", ROWS, "1j"),
        C::new("2", ROWS, "2j"),
        C::new("3", ROWS, "3j"),
        C::new("4", ROWS, "4j"),
        C::new("5", ROWS, "5j"),
        C::new("6", ROWS, "6j"),
        C::new("7", ROWS, "7j"),
        C::new("8", ROWS, "8j"),
        C::new("9", ROWS, "9j"),
        C::new("0", ROWS, "10j"),
        C::new("j", "a中bbbb\nx\nabcdef", "jj").at(0, 5),
        C::new("w", "foo-bar\n\n  beta gamma", "3w"),
        C::new("b", "foo-bar\n\n  beta gamma", "3b").at(2, 6),
        C::new("e", "foo-bar\n\n  beta gamma", "e").at(2, 11),
        C::new("|", "short\n012345678901234", "12|j"),
        C::new("$", WORDS, "2$").at(3, 2),
        C::new("o", WORDS, "o<Esc>").at(0, 4),
        C::new("o", WORDS, "2o<Esc>").at(0, 4),
        C::new("o", WORDS, "o<CR><Esc>").at(0, 4),
        C::new("O", WORDS, "O<Esc>").at(0, 4),
        C::new("cc", WORDS, "cc<Esc>").at(1, 8),
        C::new("v", WORDS, "v3lo").at(0, 4),
        C::new("V", WORDS, "VjO").at(0, 4),
        C::new("Ctrl+v", "abcde\nABCDE", "<C-v>jlO").at(0, 1),
        C::new("u", WORDS, "xjxuu").at(0, 7),
        C::new("U", WORDS, "xjUU").at(0, 4),
        C::new("@", WORDS, "qaxlxq@au").at(0, 3),
        C::new("@", WORDS, "qaxlxq2@au").at(0, 3),
        C::new("y", WORDS, "yk").at(1, 4),
        C::new("p", WORDS, "GyyP").at(0, 6),
        C::new("p", WORDS, "Gyy p").at(0, 6),
        C::new("r", "abcd", "vr中").at(0, 3),
        C::new("%", "call(\")\", x)", "%"),
        C::new("%", "call(\")\", x)", "%").at(0, 4),
        C::new("%", "call(\\), x)", "%"),
        C::new("}", PARAGRAPHS, "}").at(2, 1),
        C::new("{", PARAGRAPHS, "{").at(2, 1),
        C::new(
            "[m",
            "class X {\nvoid f() {\nif (yes) {\nrun();\n}\n}\n}",
            "[m",
        )
        .at(3, 0),
        C::new(
            "]",
            "class X {\nvoid f() {\nif (yes) {\nrun();\n}\n}\n}",
            "]m",
        )
        .at(2, 0),
        C::new("\"", WORDS, "\"ayyj\"Ayyj\"ap").at(0, 4),
        C::new("\"", WORDS, "yy\"_ddp").at(0, 4),
        C::new("Ctrl+w", "one two\nline", "Gi<C-w><Esc>"),
        C::new("Ctrl+u", "one two\nline", "Gi<C-u><Esc>"),
        C::new("Ctrl+u", "one tail", "A inserted<C-u><Esc>u"),
        C::new("Ctrl+u", "one tail", "A inserted<C-u><C-u><Esc>"),
        C::new("Ctrl+w", "abcd tail", "Rxy<C-w><Esc>"),
        C::new("Ctrl+u", "abcd tail", "Rxy<C-u><Esc>"),
        C::new("Ctrl+u", ROWS, "<C-u>").at(2, 6).view(0),
        C::new("Ctrl+d", ROWS, "<C-d>").at(38, 6).view(32),
        C::new("Ctrl+d", ROWS, "20<C-d><C-u>").at(14, 6).view(11),
        C::new("Ctrl+f", ROWS, "<C-f>").at(38, 6).view(32),
        C::new("Ctrl+b", ROWS, "<C-b>").at(3, 6).view(0),
        C::new("Ctrl+f", ROWS, "2<C-f>").at(14, 6).view(11),
        C::new("Ctrl+b", ROWS, "2<C-b>").at(14, 6).view(11),
        C::new("Ctrl+e", ROWS, "5<C-e>").at(38, 6).view(32),
        C::new("Ctrl+y", ROWS, "5<C-y>").at(3, 6).view(0),
        C::new("zz", WORDS, "zz").at(3, 6).view(0),
        C::new("zb", ROWS, "40zb").at(14, 6).view(11),
        C::new("zy", WORDS, "zyw").at(0, 2),
        C::new("zy", WORDS, "zyzy").at(0, 0),
        C::new("zy", WORDS, "zyy").at(0, 0),
        C::new("gU", "one Two\nthree FOUR", "gUiw").at(0, 1),
        C::new("gu", "one Two\nthree FOUR", "gugu").at(0, 1),
        C::new("g~", "one Two\nthree FOUR", "g~~").at(0, 1),
        C::new("g?", "ready Or Not\nanother LINE", "g??"),
        C::new("gJ", "first \n  next\nend", "3gJ"),
        C::new("gI", "  foo\n bar", "gIX<Esc>").at(0, 3),
        C::new("gi", "foo\nbar", "aX<Esc>jgiY<Esc>"),
        C::new("gE", "foo-bar baz\nquux", "gE").at(0, 8),
        C::new("ge", "foo-bar baz\nquux", "2ge").at(0, 8),
        C::new("go", "猫 one\nfoo", "2go").at(1, 0),
        C::new("gM", "0123456789", "70gM"),
        C::new("gD", "prefix item\nvalue item", "gD").at(1, 7),
        C::new("g'", "one\ntwo\nthree", "maGg'a"),
        C::new("g`", "one\ntwo\nthree", "maGg`a").at(0, 1),
        C::new("gv", "hello world", "v3l<Esc>Wgv"),
        C::new("g*", "foo foobar foo", "g*"),
        C::new("g#", "foo foobar foo", "g#"),
        C::new("gn", "foo foobar foo", "/foo<CR>gn"),
        C::new("gN", "foo foobar foo", "/foo<CR>gN"),
        C::new("gn", "foo foobar foo", "/foo<CR>dgn"),
        C::new("g&", "foo foo\nfoo foo", ":s/foo/bar/g<CR>g&"),
        C::new("g-", "abc", "aX<Esc>A1<Esc>uA2<Esc>g-"),
        C::new("g+", "abc", "aX<Esc>A1<Esc>uA2<Esc>g-g+"),
        C::new("gq", "one two\n  three four", "gqj"),
        C::new("gw", "one two\n  three four", "gwj").at(0, 2),
        C::new("gp", "one two\n  three four", "ywWgp"),
        C::new("gP", "one two\n  three four", "ywWgP"),
        C::new("gr", "abc def", "2grX"),
        C::new("gR", "abc def", "gRXY<Esc>"),
        C::new("gR", "a\tbcd", "gRxy<Esc>").at(0, 1),
        C::new("gR", "abc\nfgh", "gRX<CR>Y<Esc>"),
        C::new("gr", "abc def", "2grX").at(0, 2),
        C::new("gr", "abc def", "gr<CR>").at(0, 2),
        C::new("gu", "DOG CAT END", "guiww."),
        C::new("gD", "// item\nlet item = 0;\nitem", "gD").at(2, 0),
        C::new("g;", WORDS, "x2jxjg;g;"),
        C::new("g,", WORDS, "x2jxjg;g;g,"),
        C::new("g&", "foo bar\nfoo bar", ":s/foo/out/<CR>/bar<CR>g&"),
        C::new("gN", "foo foobar foo", "/foo<CR>2gN"),
        C::new("gn", "foo foobar foo", "/foo<CR>gngn"),
        C::new("gq", "one two\n\n  three four\n  five six", "gqG"),
        C::new("d", "abcd\nefgh", "dvl"),
        C::new("d", "abcd\nefgh\nlast", "dVw"),
        C::new("c", "abcd\nefgh\nlast", "cVwX<Esc>"),
        C::new("y", "abcd\nefgh\nlast", "y<C-v>j").at(0, 1),
        C::new("c", "<div><b>hello</b></div>", "citworld<Esc>").at(0, 10),
        C::new("d", "<div><b>hello</b></div>", "dat").at(0, 10),
        C::new("y", "First sentence.  Second sentence. Third.", "yis").at(0, 4),
        C::new("d", "alpha beta gamma", "d/beta<CR>"),
        C::new("y", "alpha beta gamma alpha", "/alpha<CR>0yn"),
        C::new("i", "", "iab<C-g>ucd<Esc>u"),
        C::new("i", "first\nsecond", "ia<C-\\><C-n>"),
        C::new("y", "one two\nthree", "ywG`["),
        C::new("y", "one two\nthree", "ywG`]"),
        C::new("d", "<article><b>hello</b></article>", "d2at").at(0, 14),
        C::new(
            "c",
            "First sentence.  Second sentence. Third.",
            "cisNew.<Esc>",
        )
        .at(0, 5),
        C::new("zy", "ab  tail\na   tail", "<C-v>j3lzy"),
        C::new(
            "zp",
            "ab  tail\na   tail\nABCDEFG\n0123456",
            "<C-v>j3lzy2jzp",
        ),
        C::new(
            "zP",
            "ab  tail\na   tail\nABCDEFG\n0123456",
            "<C-v>j3lzy2j2zP",
        ),
    ]
}

/// Observable text and shape of a named or unnamed register.
#[derive(Debug, Deserialize, PartialEq, Eq)]
struct RegisterState(String, String);

/// Compare user-visible results without inspecting either implementation's command state.
#[derive(Debug, Deserialize)]
struct State {
    /// Buffer text or saved scratch-file contents for ZZ and ZQ.
    text: String,
    /// Zero-based caret row and UTF-8 byte column.
    caret: [usize; 2],
    /// Normal, Visual, Visual Line, Visual Block, or closed.
    mode: String,
    /// Fixed Visual endpoint in the same units as the caret.
    anchor: Option<[usize; 2]>,
    /// The unnamed register and named register a.
    unnamed: RegisterState,
    named: RegisterState,
    /// First visible line for screen-relative cases.
    top: Option<usize>,
    /// Initial viewport after Neovim has applied window constraints.
    initial_top: usize,
}

/// Neovim version and results from the isolated test process.
#[derive(Deserialize)]
struct Reference {
    version: String,
    states: Vec<State>,
}

/// Launch the reference once and collect the result of each input sequence.
fn reference(cases: &[Case], directory: &Path) -> Reference {
    let script = directory.join("reference.lua");
    let input = directory.join("cases.json");
    let output = directory.join("result.json");
    fs::write(&script, include_str!("compatibility_reference.lua")).unwrap();
    fs::write(&input, serde_json::to_vec(cases).unwrap()).unwrap();
    let process = Command::new("nvim")
        .args(["--headless", "--clean", "-i", "NONE", "-n", "-l"])
        .args([&script, &input, &output])
        .current_dir(directory)
        .output()
        .expect("start Neovim 0.12 for comparison");
    assert!(
        process.status.success(),
        "{}",
        String::from_utf8_lossy(&process.stderr)
    );
    serde_json::from_slice(&fs::read(output).unwrap()).unwrap()
}

/// Translate a Neovim-style test key sequence to the editor's normal input calls.
fn strokes(mut keys: &str) -> Vec<String> {
    let mut output = Vec::new();
    while !keys.is_empty() {
        if let Some(token) = keys.strip_prefix('<')
            && let Some((name, remainder)) = token.split_once('>')
        {
            let key = match name {
                "Esc" => "Escape".into(),
                "CR" => "Enter".into(),
                "BS" => "Backspace".into(),
                "Del" => "Delete".into(),
                "lt" => "<".into(),
                "Insert" | "Tab" | "Home" | "End" | "PageUp" | "PageDown" | "Left" | "Right"
                | "Up" | "Down" => name.into(),
                _ => format!("Ctrl+{}", name.strip_prefix("C-").expect("valid test key")),
            };
            output.push(key);
            keys = remainder;
        } else {
            let grapheme = keys.graphemes(true).next().unwrap();
            output.push(grapheme.into());
            keys = &keys[grapheme.len()..];
        }
    }
    output
}

/// Run the same `case` through the editor, using the reference's initial viewport.
fn editor(case: &Case, initial_top: usize, directory: &Path) -> State {
    let path = directory.join("native-close.txt");
    let mut vim = if case.close {
        fs::write(&path, case.text).unwrap();
        Vim::new(Document::open(&path).unwrap())
    } else {
        Vim::new(Document::new(case.text))
    };
    let mut registers = Register::default();
    vim.set_cursor(vim.doc.starts[case.row] + case.col);
    vim.set_view(initial_top, HEIGHT);
    let mut closed = false;
    for key in strokes(case.keys) {
        let action = vim.key(&key, &mut registers);
        match action {
            Action::None => assert_ne!(key, "Ctrl+s"),
            Action::Save(None) => assert_eq!(case.shortcut, "Ctrl+s"),
            Action::SaveClose if case.close => {
                vim.doc.save(None).unwrap();
                closed = true;
            }
            Action::Close(true) | Action::QuitAll(true) if case.close => closed = true,
            Action::Close(false) | Action::QuitAll(false) if case.close && !vim.doc.modified() => {
                closed = true;
            }
            Action::Filter { range, command } => {
                let output = run_filter(directory, &command, &vim.doc.text[range.clone()]).unwrap();
                vim.apply_filter(range, &output);
            }
            other => panic!("{} requested an unexpected action: {other:?}", case.keys),
        }
    }
    assert_eq!(closed, case.close, "{}", case.keys);
    let location = |byte: usize| {
        let row = vim.doc.location(byte).0;
        [row, byte - vim.doc.starts[row]]
    };
    let mode = if closed {
        "closed"
    } else {
        match vim.mode {
            Mode::Normal => "n",
            Mode::Visual => "v",
            Mode::VisualLine => "V",
            Mode::VisualBlock => "\u{16}",
            Mode::Insert => "i",
            Mode::Replace => "R",
            Mode::Command => ":",
            Mode::Search => "/",
        }
    }
    .to_owned();
    let register = |name| {
        let value = registers.read(name);
        let kind = match value.kind {
            Kind::Character => "v".into(),
            Kind::Line => "V".into(),
            Kind::Block(width) => format!("\u{16}{width}"),
        };
        RegisterState(value.text, kind)
    };
    State {
        text: if closed {
            fs::read_to_string(path).unwrap().replace("\r\n", "\n")
        } else {
            vim.doc.text.clone()
        },
        caret: if closed {
            [0, 0]
        } else {
            location(vim.doc.cursor)
        },
        mode,
        anchor: vim.mode.is_visual().then(|| location(vim.anchor)),
        unnamed: register(None),
        named: register(Some('a')),
        top: case.top.map(|_| vim.take_view_top().unwrap_or(initial_top)),
        initial_top,
    }
}

/// Report only fields that differ, preserving every failure from a complete audit run.
fn compare(case: &Case, want: &State, got: &State, failures: &mut Vec<String>) {
    let mut differences = Vec::new();
    if got.text != want.text {
        differences.push(format!("text {:?} → {:?}", want.text, got.text));
    }
    if got.caret != want.caret {
        differences.push(format!("caret {:?} → {:?}", want.caret, got.caret));
    }
    if got.mode != want.mode {
        differences.push(format!("mode {:?} → {:?}", want.mode, got.mode));
    }
    if got.anchor != want.anchor {
        differences.push(format!("anchor {:?} → {:?}", want.anchor, got.anchor));
    }
    if got.unnamed != want.unnamed {
        differences.push(format!("unnamed {:?} → {:?}", want.unnamed, got.unnamed));
    }
    if got.named != want.named {
        differences.push(format!("register a {:?} → {:?}", want.named, got.named));
    }
    if got.top != want.top {
        differences.push(format!("top {:?} → {:?}", want.top, got.top));
    }
    if !differences.is_empty() {
        failures.push(format!(
            "{:?} [{}] {}",
            case.shortcut,
            case.keys,
            differences.join(", ")
        ));
    }
}

/// Every active character, chord and sequence in help has an executable comparison.
#[test]
fn character_keyboard_catalog_is_covered() {
    let cases = cases();
    for &(key, _, _) in CATALOG.iter().chain(NAMED_KEYS) {
        assert!(
            cases.iter().any(|case| case.shortcut == key),
            "missing comparison for {key}"
        );
    }
    for digit in "0123456789".chars() {
        assert!(cases.iter().any(|case| case.shortcut == digit.to_string()));
    }
}

/// Use a standalone reference executable during this test only.
#[test]
#[ignore = "requires Neovim 0.12 on PATH"]
fn character_keyboard_matches_neovim() {
    let directory = tempfile::tempdir().unwrap();
    let cases = cases();
    let reference = reference(&cases, directory.path());
    assert_eq!(reference.states.len(), cases.len());
    let mut failures = Vec::new();
    for (case, want) in cases.iter().zip(&reference.states) {
        let got = editor(case, want.initial_top, directory.path());
        compare(case, want, &got, &mut failures);
    }
    eprintln!("{} cases against Neovim {}", cases.len(), reference.version);
    assert!(
        failures.is_empty(),
        "{} differences (Neovim → Nanobug):\n{}",
        failures.len(),
        failures.join("\n")
    );
}
