//! Small source lexers with cached multiline state. Syntax only changes foreground
//! colors; selections, search matches and diff backgrounds stay with the renderer.
use crate::model::Session;
use std::{borrow::Cow, collections::HashMap, ops::Range, path::Path};

pub type Token = (Range<usize>, &'static str);
const MAX_LINE_BYTES: usize = 256 * 1024;
const MAX_CACHED_VIEWS: usize = 128;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Language {
    #[default]
    Plain,
    Al,
    Xml,
    Rust,
    C,
    JavaScript,
    Python,
    Json,
    Toml,
    Yaml,
    Lua,
    Shell,
    Sql,
    Markdown,
}

pub fn language(path: &Path) -> Language {
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    if matches!(
        name.as_str(),
        ".bashrc" | ".zshrc" | ".profile" | ".bash_profile"
    ) {
        return Language::Shell;
    }
    match path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase()
        .as_str()
    {
        "al" => Language::Al,
        "xml" | "xlf" | "rdlc" | "rdl" | "xsd" | "config" | "csproj" | "props" | "targets"
        | "resx" | "html" | "htm" | "svg" => Language::Xml,
        "rs" => Language::Rust,
        "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "cs" | "java" | "go" | "swift" | "kt"
        | "kts" => Language::C,
        "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" => Language::JavaScript,
        "py" | "pyw" | "pyi" => Language::Python,
        "json" | "jsonc" => Language::Json,
        "toml" | "ini" | "cfg" => Language::Toml,
        "yml" | "yaml" => Language::Yaml,
        "lua" => Language::Lua,
        "sh" | "bash" | "zsh" | "fish" => Language::Shell,
        "sql" => Language::Sql,
        "md" | "markdown" => Language::Markdown,
        _ => Language::Plain,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    Normal,
    Block {
        end: &'static str,
        depth: u32,
        nested: bool,
    },
    String {
        end: &'static str,
        escapes: bool,
        doubled: bool,
        multiline: bool,
    },
    Long {
        end: String,
        comment: bool,
    },
    MarkdownFence,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct State {
    mode: Mode,
    in_tag: bool,
}

#[derive(Default)]
struct DocumentCache {
    revision: u64,
    language: Language,
    // states[n] is the lexical state at the beginning of line n.
    states: Vec<State>,
    views: HashMap<(usize, usize, usize), Vec<Token>>,
}

#[derive(Default)]
pub struct HighlightCache {
    documents: [DocumentCache; 2],
}

impl HighlightCache {
    pub fn ranges(
        &mut self,
        session: &Session,
        side: usize,
        line: usize,
        visible: Range<usize>,
    ) -> Vec<Token> {
        let syntax = language(&session.path);
        if syntax == Language::Plain {
            return Vec::new();
        }
        let lines = &session.docs[side].lines;
        let Some(text) = lines.get(line) else {
            return Vec::new();
        };
        let cache = &mut self.documents[side];
        if cache.revision != session.revision || cache.language != syntax {
            *cache = DocumentCache {
                revision: session.revision,
                language: syntax,
                states: vec![State::default()],
                views: HashMap::new(),
            };
        }
        let key = (line, visible.start, visible.end);
        if let Some(tokens) = cache.views.get(&key) {
            return tokens.clone();
        }
        while cache.states.len() <= line {
            let previous = cache.states.len() - 1;
            let (_, state) = scan_line(
                &lines[previous],
                syntax,
                cache.states[previous].clone(),
                0..0,
            );
            cache.states.push(state);
        }
        let (tokens, state) = scan_line(text, syntax, cache.states[line].clone(), visible);
        if cache.states.len() == line + 1 {
            cache.states.push(state);
        }
        if cache.views.len() >= MAX_CACHED_VIEWS {
            cache.views.clear();
        }
        cache.views.insert(key, tokens.clone());
        tokens
    }
}

pub fn single_line(text: &str, syntax: Language, visible: Range<usize>) -> Vec<Token> {
    scan_line(text, syntax, State::default(), visible).0
}

fn emit(tokens: &mut Vec<Token>, visible: &Range<usize>, range: Range<usize>, color: &'static str) {
    if range.start < visible.end && range.end > visible.start {
        if let Some((previous, previous_color)) = tokens.last_mut()
            && previous.end == range.start
            && *previous_color == color
        {
            previous.end = range.end;
        } else {
            tokens.push((range, color));
        }
    }
}

fn line_comment(text: &str, syntax: Language) -> bool {
    match syntax {
        Language::Al | Language::Rust | Language::C | Language::JavaScript | Language::Json => {
            text.starts_with("//")
        }
        Language::Python | Language::Toml | Language::Yaml | Language::Shell => {
            text.starts_with('#')
        }
        Language::Lua | Language::Sql => text.starts_with("--"),
        _ => false,
    }
}

fn c_comments(syntax: Language) -> bool {
    matches!(
        syntax,
        Language::Al
            | Language::Rust
            | Language::C
            | Language::JavaScript
            | Language::Json
            | Language::Sql
    )
}

fn long_lua(text: &str) -> Option<(usize, String)> {
    let rest = text.strip_prefix('[')?;
    let equals = rest.bytes().take_while(|&b| b == b'=').count();
    if rest.as_bytes().get(equals) == Some(&b'[') {
        Some((equals + 2, format!("]{}]", "=".repeat(equals))))
    } else {
        None
    }
}

fn raw_rust(text: &str) -> Option<(usize, String)> {
    let rest = text.strip_prefix("br").or_else(|| text.strip_prefix('r'))?;
    let hashes = rest.bytes().take_while(|&b| b == b'#').count();
    if rest.as_bytes().get(hashes) == Some(&b'"') {
        Some((
            text.len() - rest.len() + hashes + 1,
            format!("\"{}", "#".repeat(hashes)),
        ))
    } else {
        None
    }
}

/// Finds the end of the current multiline construct, preserving state if it
/// crosses the line. Nested Rust comments are handled without allocating tokens.
fn continue_mode(text: &str, index: &mut usize, state: &mut State) -> &'static str {
    let start = *index;
    match state.mode.clone() {
        Mode::Normal | Mode::MarkdownFence => "fg",
        Mode::Block {
            end,
            mut depth,
            nested,
        } => {
            while *index < text.len() {
                let tail = &text[*index..];
                if tail.starts_with(end) {
                    *index += end.len();
                    depth -= 1;
                    if depth == 0 {
                        state.mode = Mode::Normal;
                        return "muted";
                    }
                } else if nested && tail.starts_with("/*") {
                    depth = depth.saturating_add(1);
                    *index += 2;
                } else {
                    *index += tail.chars().next().unwrap().len_utf8();
                }
            }
            state.mode = Mode::Block { end, depth, nested };
            "muted"
        }
        Mode::String {
            end,
            escapes,
            doubled,
            multiline,
        } => {
            while *index < text.len() {
                let tail = &text[*index..];
                if escapes && tail.starts_with('\\') {
                    *index += 1;
                    if *index < text.len() {
                        *index += text[*index..].chars().next().unwrap().len_utf8();
                    }
                } else if tail.starts_with(end) {
                    *index += end.len();
                    if doubled && text[*index..].starts_with(end) {
                        *index += end.len();
                    } else {
                        state.mode = Mode::Normal;
                        return "logo_merge";
                    }
                } else {
                    *index += tail.chars().next().unwrap().len_utf8();
                }
            }
            if !multiline {
                state.mode = Mode::Normal;
            }
            "logo_merge"
        }
        Mode::Long { end, comment } => {
            if let Some(found) = text[start..].find(&end) {
                *index += found + end.len();
                state.mode = Mode::Normal;
            } else {
                *index = text.len();
            }
            if comment { "muted" } else { "logo_merge" }
        }
    }
}

fn scan_line(
    text: &str,
    syntax: Language,
    mut state: State,
    visible: Range<usize>,
) -> (Vec<Token>, State) {
    if syntax == Language::Plain || text.len() > MAX_LINE_BYTES {
        return (Vec::new(), State::default());
    }
    let mut tokens = Vec::new();
    if syntax == Language::Markdown {
        let trimmed = text.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            state.mode = if matches!(state.mode, Mode::MarkdownFence) {
                Mode::Normal
            } else {
                Mode::MarkdownFence
            };
            emit(&mut tokens, &visible, 0..text.len(), "minor");
            return (tokens, state);
        }
        if matches!(state.mode, Mode::MarkdownFence) {
            emit(&mut tokens, &visible, 0..text.len(), "logo_merge");
            return (tokens, state);
        }
        if trimmed.starts_with('#') {
            emit(&mut tokens, &visible, 0..text.len(), "logo_lazy");
            return (tokens, state);
        }
    }
    if state.mode == Mode::Normal
        && matches!(syntax, Language::Al | Language::C | Language::Rust)
        && text.trim_start().starts_with('#')
    {
        emit(&mut tokens, &visible, 0..text.len(), "different");
        return (tokens, state);
    }
    let mut index = 0;
    while index < text.len() {
        let start = index;
        if state.mode != Mode::Normal {
            let color = continue_mode(text, &mut index, &mut state);
            emit(&mut tokens, &visible, start..index, color);
            continue;
        }
        let tail = &text[index..];
        let ch = tail.chars().next().unwrap();
        // Lua --[=[...]=] comments must be recognized before ordinary -- comments.
        if syntax == Language::Lua
            && tail.starts_with("--")
            && let Some((opening, end)) = long_lua(&tail[2..])
        {
            index += opening + 2;
            state.mode = Mode::Long { end, comment: true };
            let color = continue_mode(text, &mut index, &mut state);
            emit(&mut tokens, &visible, start..index, color);
            continue;
        }
        if line_comment(tail, syntax) {
            emit(&mut tokens, &visible, start..text.len(), "muted");
            break;
        }
        if (c_comments(syntax) && tail.starts_with("/*"))
            || (syntax == Language::Xml && tail.starts_with("<!--"))
        {
            let xml = syntax == Language::Xml;
            index += if xml { 4 } else { 2 };
            state.mode = Mode::Block {
                end: if xml { "-->" } else { "*/" },
                depth: 1,
                nested: syntax == Language::Rust,
            };
            let color = continue_mode(text, &mut index, &mut state);
            emit(&mut tokens, &visible, start..index, color);
            continue;
        }
        if syntax == Language::Xml && tail.starts_with("<![CDATA[") {
            index += 9;
            state.mode = Mode::Long {
                end: "]]>".into(),
                comment: false,
            };
            let color = continue_mode(text, &mut index, &mut state);
            emit(&mut tokens, &visible, start..index, color);
            continue;
        }
        if syntax == Language::Rust
            && let Some((opening, end)) = raw_rust(tail)
        {
            index += opening;
            state.mode = Mode::Long {
                end,
                comment: false,
            };
            let color = continue_mode(text, &mut index, &mut state);
            emit(&mut tokens, &visible, start..index, color);
            continue;
        }
        if syntax == Language::Lua
            && let Some((opening, end)) = long_lua(tail)
        {
            index += opening;
            state.mode = Mode::Long {
                end,
                comment: false,
            };
            let color = continue_mode(text, &mut index, &mut state);
            emit(&mut tokens, &visible, start..index, color);
            continue;
        }
        let quote = match ch {
            '\'' if syntax != Language::Rust || rust_char_literal(tail) => Some("'"),
            '"' => Some("\""),
            '`' if matches!(
                syntax,
                Language::JavaScript | Language::Shell | Language::Markdown
            ) =>
            {
                Some("`")
            }
            _ => None,
        };
        if let Some(quote) = quote.filter(|_| {
            (syntax != Language::Xml || state.in_tag) && (syntax != Language::Markdown || ch == '`')
        }) {
            let triple = matches!(syntax, Language::Python | Language::Toml)
                && tail.starts_with(&quote.repeat(3));
            let end = if triple {
                if ch == '\'' { "'''" } else { "\"\"\"" }
            } else {
                quote
            };
            index += end.len();
            state.mode = Mode::String {
                end,
                escapes: !matches!(syntax, Language::Al | Language::Sql)
                    && !(syntax == Language::Toml && ch == '\''),
                doubled: matches!(syntax, Language::Al | Language::Sql),
                multiline: triple
                    || matches!(syntax, Language::Al | Language::Sql | Language::Shell)
                    || ch == '`',
            };
            let color = continue_mode(text, &mut index, &mut state);
            emit(
                &mut tokens,
                &visible,
                start..index,
                if syntax == Language::Al && ch == '"' {
                    "accent"
                } else {
                    color
                },
            );
            continue;
        }
        if syntax == Language::Xml && ch == '<' {
            index += 1;
            if text[index..].starts_with('/') || text[index..].starts_with('?') {
                index += 1;
            }
            while index < text.len() && is_xml_name(text[index..].chars().next().unwrap()) {
                index += text[index..].chars().next().unwrap().len_utf8();
            }
            state.in_tag = true;
            emit(&mut tokens, &visible, start..index, "minor");
            continue;
        }
        if syntax == Language::Xml && state.in_tag && matches!(ch, '>' | '/' | '=' | '?') {
            index += ch.len_utf8();
            if ch == '>' {
                state.in_tag = false;
            }
            emit(&mut tokens, &visible, start..index, "minor");
            continue;
        }
        if syntax == Language::Shell && ch == '$' {
            index += 1;
            if text[index..].starts_with('{') {
                index = text[index + 1..]
                    .find('}')
                    .map_or(text.len(), |n| index + n + 2);
            } else {
                while index < text.len() && identifier(text[index..].chars().next().unwrap()) {
                    index += text[index..].chars().next().unwrap().len_utf8();
                }
            }
            emit(&mut tokens, &visible, start..index, "minor");
            continue;
        }
        if ch.is_ascii_digit()
            && (start == 0 || !identifier(text[..start].chars().next_back().unwrap()))
        {
            index += 1;
            while index < text.len()
                && (text.as_bytes()[index].is_ascii_alphanumeric()
                    || matches!(text.as_bytes()[index], b'_' | b'.'))
            {
                index += 1;
            }
            emit(&mut tokens, &visible, start..index, "accent");
            continue;
        }
        if (ch.is_alphabetic() || ch == '_') && (syntax != Language::Xml || state.in_tag) {
            index += ch.len_utf8();
            while index < text.len() {
                let next = text[index..].chars().next().unwrap();
                if !(if syntax == Language::Xml {
                    is_xml_name(next)
                } else {
                    identifier(next)
                }) {
                    break;
                }
                index += next.len_utf8();
            }
            if index <= visible.start || start >= visible.end {
                continue;
            }
            let word = &text[start..index];
            let after = text[index..].trim_start();
            let color = if syntax == Language::Xml {
                after.starts_with('=').then_some("accent")
            } else if matches!(syntax, Language::Yaml | Language::Toml)
                && (after.starts_with(':') || after.starts_with('='))
            {
                Some("accent")
            } else if word.len() <= 40 && keyword(syntax, word) {
                Some("logo_lazy")
            } else if (word.len() <= 40 && type_name(syntax, word))
                || (after.starts_with('(')
                    && !matches!(
                        syntax,
                        Language::Json | Language::Toml | Language::Yaml | Language::Markdown
                    ))
            {
                Some("minor")
            } else {
                None
            };
            if let Some(color) = color {
                emit(&mut tokens, &visible, start..index, color);
            }
            continue;
        }
        index += ch.len_utf8();
        if syntax == Language::Markdown && matches!(ch, '*' | '_' | '[' | ']' | '(' | ')') {
            emit(&mut tokens, &visible, start..index, "minor");
        }
    }
    (tokens, state)
}

fn rust_char_literal(text: &str) -> bool {
    let mut chars = text[1..].chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if first == '\\' {
        text[1..].find('\'').is_some_and(|end| end <= 12)
    } else {
        chars.next() == Some('\'')
    }
}

fn identifier(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}
fn is_xml_name(ch: char) -> bool {
    identifier(ch) || matches!(ch, ':' | '.' | '-')
}

fn keyword(syntax: Language, word: &str) -> bool {
    let word = if matches!(syntax, Language::Al | Language::Sql) {
        Cow::Owned(word.to_ascii_lowercase())
    } else {
        Cow::Borrowed(word)
    };
    let words = match syntax {
        Language::Rust => {
            "as async await break const continue crate dyn else enum extern false fn for if impl in let loop match mod move mut pub ref return self Self static struct super trait true type unsafe use where while union"
        }
        Language::C => {
            "abstract as async await base break case catch class const constexpr continue default delegate delete do else enum event explicit export extends extern false final finally for foreach friend func function get goto if implements import in inline interface internal is lambda namespace native new nil null nullptr operator override package partial private protected public readonly record ref register return sealed set sizeof static strictfp struct super switch synchronized template this throw throws true try type typedef typename union unsafe unsigned using var virtual void volatile when while yield"
        }
        Language::JavaScript => {
            "as async await break case catch class const continue debugger declare default delete do else enum export extends false finally for from function get if implements import in instanceof interface let module namespace new null of package private protected public readonly require return set static super switch this throw true try type typeof undefined var void while with yield"
        }
        Language::Python => {
            "and as assert async await break case class continue def del elif else except False finally for from global if import in is lambda match None nonlocal not or pass raise return True try type while with yield"
        }
        Language::Al => {
            "begin end if then else case of for to downto do while repeat until exit with var local procedure trigger internal protected and or not xor div mod in true false codeunit table tableextension page pageextension report reportextension query xmlport enum enumextension interface permissionset permissionsetextension profile controladdin entitlement dotnet assembly type field fields key keys fieldgroup fieldgroups area group part action actions layout repeater systempart usercontrol column dataitem dataset requestpage labels elements schema textelement tableelement fieldelement fieldattribute textattribute event implements extends foreach asserterror record value modify addafter addbefore addfirst addlast movefirst movelast moveafter movebefore views view"
        }
        Language::Json => "true false null",
        Language::Toml => "true false inf nan",
        Language::Yaml => "true false null yes no on off",
        Language::Lua => {
            "and break do else elseif end false for function goto if in local nil not or repeat return then true until while"
        }
        Language::Shell => {
            "if then else elif fi for while do done case esac in function select until time export readonly local declare typeset unset return break continue source"
        }
        Language::Sql => {
            "select from where join inner outer left right full cross on as insert into values update set delete create alter drop table view index procedure function trigger begin end if else case when then distinct order by group having limit offset union all exists in not null true false and or is like between primary key foreign references constraint default grant revoke with recursive returning asc desc count sum avg min max"
        }
        _ => "",
    };
    words
        .split_ascii_whitespace()
        .any(|candidate| candidate == word)
}

fn type_name(syntax: Language, word: &str) -> bool {
    let words = match syntax {
        Language::Rust => {
            "bool char str String usize isize u8 u16 u32 u64 u128 i8 i16 i32 i64 i128 f32 f64 Vec Option Result Some None Ok Err Box Arc Rc HashMap HashSet"
        }
        Language::C | Language::JavaScript => {
            "bool boolean byte char decimal double float int integer long number object short signed size_t string String unsigned void wchar_t Any Array List Map Set Promise"
        }
        Language::Python => {
            "bool bytes complex dict float frozenset int list object set str tuple type super print len range enumerate zip isinstance Exception ValueError TypeError"
        }
        Language::Al => {
            "integer decimal boolean text code date time datetime duration guid option biginteger byte char blob media mediaset recordid recordref fieldref keyref jsonobject jsonarray jsontoken jsonvalue httpclient httpcontent httprequestmessage httpresponsemessage httpheaders instream outstream list dictionary label errorinfo notification dialog textbuilder xmldocument xmlelement xmlnode xmlattribute secrettext variant"
        }
        _ => "",
    };
    words.split_ascii_whitespace().any(|candidate| {
        if syntax == Language::Al {
            candidate.eq_ignore_ascii_case(word)
        } else {
            candidate == word
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Document;

    fn tokens<'a>(text: &'a str, extension: &str) -> Vec<(&'a str, &'static str)> {
        single_line(text, language(Path::new(extension)), 0..text.len())
            .into_iter()
            .map(|(range, color)| (&text[range], color))
            .collect()
    }

    #[test]
    fn colors_common_source_and_config_languages() {
        for (name, source, expected) in [
            ("main.rs", "pub fn main() { let n: u32 = 42; }", "pub"),
            ("main.cpp", "const int answer = 42; // hello", "const"),
            ("main.ts", "export const value: number = 42;", "export"),
            ("main.py", "def answer(): return True # hello", "def"),
            ("main.lua", "local value = true -- hello", "local"),
            ("main.sh", "if test -n \"$HOME\"; then echo yes; fi", "if"),
            (
                "query.sql",
                "SELECT value FROM items WHERE id = 42",
                "SELECT",
            ),
            ("a.json", "{\"enabled\": true}", "true"),
            ("a.toml", "enabled = true", "true"),
            ("a.yaml", "enabled: true", "true"),
        ] {
            let found = tokens(source, name);
            assert!(
                found.contains(&(expected, "logo_lazy")),
                "{name}: {found:?}"
            );
        }
        assert!(tokens("let size = 42;", "main.rs").contains(&("42", "accent")));
        assert!(
            tokens("pub fn main() {} // comment", "main.rs").contains(&("// comment", "muted"))
        );
    }

    #[test]
    fn cached_multiline_state_survives_scrolling_and_invalidates_after_edit() {
        let source = "/* open\nstill comment\n*/ pub fn main() {}\n";
        let mut session = Session::new(
            "main.rs".into(),
            [
                Document::from_bytes(Some(source.as_bytes().to_vec())),
                Document::from_bytes(None),
            ],
            false,
        );
        let mut cache = HighlightCache::default();
        let found = cache.ranges(&session, 0, 1, 0..13);
        assert_eq!(found[0].1, "muted");
        let found = cache.ranges(&session, 0, 2, 0..22);
        assert!(
            found
                .iter()
                .any(|(r, c)| &session.docs[0].lines[2][r.clone()] == "pub" && *c == "logo_lazy")
        );
        let revision = session.revision;
        session.replace(0, 0..1, "// closed\n", false).unwrap();
        assert_ne!(session.revision, revision);
        assert!(cache.ranges(&session, 0, 1, 0..13).is_empty());
    }

    #[test]
    fn multiline_strings_nested_comments_and_lua_long_brackets() {
        for (file, source, line, color) in [
            (
                "a.py",
                "x = \"\"\"open\nstill string\n\"\"\"\n",
                1,
                "logo_merge",
            ),
            (
                "a.rs",
                "let s = r#\"open\nstill string\n\"#;\n",
                1,
                "logo_merge",
            ),
            ("a.lua", "--[=[ comment\nstill comment\n]=]\n", 1, "muted"),
            (
                "a.lua",
                "value = [==[ string\nstill string\n]==]\n",
                1,
                "logo_merge",
            ),
            (
                "a.rs",
                "/* first /* nested */\nstill comment\n*/\n",
                1,
                "muted",
            ),
            ("a.xml", "<!-- comment\nstill comment\n-->\n", 1, "muted"),
        ] {
            let session = Session::new(
                file.into(),
                [
                    Document::from_bytes(Some(source.as_bytes().to_vec())),
                    Document::from_bytes(None),
                ],
                false,
            );
            let found = HighlightCache::default().ranges(
                &session,
                0,
                line,
                0..session.docs[0].lines[line].len(),
            );
            assert_eq!(found[0].1, color, "{file}");
        }
    }

    #[test]
    fn pathological_generated_lines_use_bounded_plain_text_fallback() {
        let huge = "x".repeat(4 * 1024 * 1024);
        assert!(single_line(&huge, Language::Rust, 1_000_000..1_000_080).is_empty());
        let session = Session::new(
            "a.rs".into(),
            [
                Document::from_bytes(Some(format!("{huge}\npub fn main() {{}}\n").into_bytes())),
                Document::from_bytes(None),
            ],
            false,
        );
        let found = HighlightCache::default().ranges(&session, 0, 1, 0..16);
        assert!(found.iter().any(|(_, c)| *c == "logo_lazy"));
    }

    #[test]
    fn xml_attributes_al_strings_and_utf8_ranges_remain_correct() {
        let source = "<t name='ä界'>text</t>";
        assert!(tokens(source, "a.xml").contains(&("name", "accent")));
        assert!(tokens(source, "a.xml").contains(&("'ä界'", "logo_merge")));
        assert!(tokens("'it''s' + 12 // comment", "a.al").contains(&("'it''s'", "logo_merge")));
        assert!(
            tokens("fn get<'a>(x: &'a str) {}", "a.rs")
                .iter()
                .all(|(s, _)| *s != "'a>(x: &''")
        );
    }
}
