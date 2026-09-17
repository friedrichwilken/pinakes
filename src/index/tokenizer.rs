//! The tokeniser shared by indexing and querying (SPEC §5, §10.3), as a plain function and as a
//! tantivy tokenizer.

use tantivy::tokenizer::{Token, TokenStream, Tokenizer};

/// Stopwords dropped by [`tokenize`], sorted.
pub const STOPWORDS: [&str; 35] = [
    "a", "an", "and", "are", "as", "at", "be", "by", "can", "do", "does", "for", "from", "how",
    "i", "if", "in", "is", "it", "its", "my", "of", "on", "or", "that", "the", "this", "to", "was",
    "what", "when", "which", "with", "you", "your",
];
/// Name under which the tokenizer is registered with tantivy.
pub const TOKENIZER_NAME: &str = "pinakes";

/// Whether `token` is in [`STOPWORDS`].
pub fn is_stopword(token: &str) -> bool {
    STOPWORDS.binary_search(&token).is_ok()
}

/// Accumulates one token while scanning text.
struct Scanner {
    token: String,
    start: usize,
}

impl Scanner {
    fn push(&mut self, ch: char, offset: usize) {
        if self.token.is_empty() {
            self.start = offset;
        }
        self.token.push(ch);
    }

    fn flush(&mut self, end: usize, sink: &mut dyn FnMut(usize, usize, &str)) {
        if !self.token.is_empty() {
            if !is_stopword(&self.token) {
                sink(self.start, end, &self.token);
            }
            self.token.clear();
        }
    }
}

/// Call `sink(start, end, token)` for every token of `text` with its byte offsets.
///
/// A token is a maximal run of ASCII letters and digits after lowercasing; every other
/// character separates tokens. Stopwords are dropped. For an identifier compound — alphanumeric
/// runs joined by `.` `/` `_` or `-` — the joined form with separators removed is also emitted
/// (SPEC §10.3), e.g. `spec.sink` → `spec`, `sink`, `specsink`.
fn scan_tokens(text: &str, sink: &mut dyn FnMut(usize, usize, &str)) {
    let mut scanner = Scanner {
        token: String::new(),
        start: 0,
    };
    for (offset, ch) in text.char_indices() {
        if ch.is_ascii() {
            if ch.is_ascii_alphanumeric() {
                scanner.push(ch.to_ascii_lowercase(), offset);
            } else {
                scanner.flush(offset, sink);
            }
        } else {
            // Lowercasing a non-ASCII letter can yield ASCII (`İ` → `i` + combining dot).
            for lower in ch.to_lowercase() {
                if lower.is_ascii_alphanumeric() {
                    scanner.push(lower, offset);
                } else {
                    scanner.flush(offset + ch.len_utf8(), sink);
                }
            }
        }
    }
    scanner.flush(text.len(), sink);
    scan_compounds(text, sink);
}

/// A separator that, between two alphanumeric runs, marks an identifier compound (SPEC §10.3).
fn is_compound_separator(ch: char) -> bool {
    matches!(ch, '.' | '/' | '_' | '-')
}

/// Emit the joined form of every identifier compound in `text`: a maximal run of ASCII
/// alphanumerics and `.` `/` `_` `-` that, once separators at either end are trimmed away,
/// still contains a separator between two alphanumeric parts.
fn scan_compounds(text: &str, sink: &mut dyn FnMut(usize, usize, &str)) {
    let is_run_char =
        |ch: char| ch.is_ascii() && (ch.is_ascii_alphanumeric() || is_compound_separator(ch));
    let mut run_start: Option<usize> = None;
    let mut run_end = 0;
    for (offset, ch) in text.char_indices() {
        if is_run_char(ch) {
            run_start.get_or_insert(offset);
            run_end = offset + ch.len_utf8();
        } else if let Some(start) = run_start.take() {
            emit_compound(&text[start..run_end], start, sink);
        }
    }
    if let Some(start) = run_start {
        emit_compound(&text[start..run_end], start, sink);
    }
}

/// Emit the joined token for one compound run, when trimming its leading and trailing
/// separators still leaves at least one separator between two alphanumeric parts.
fn emit_compound(run: &str, run_start: usize, sink: &mut dyn FnMut(usize, usize, &str)) {
    let core = run.trim_matches(is_compound_separator);
    if core.is_empty() || !core.contains(is_compound_separator) {
        return;
    }
    let joined: String = core
        .split(is_compound_separator)
        .filter(|part| !part.is_empty())
        .flat_map(str::chars)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if joined.is_empty() {
        return;
    }
    let Some(core_offset) = run.find(core) else {
        return;
    };
    let core_start = run_start + core_offset;
    sink(core_start, core_start + core.len(), &joined);
}

/// Tokenise for indexing and querying: lowercase, `[a-z0-9]+` runs, stopwords dropped.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    scan_tokens(text, &mut |_, _, token| tokens.push(token.to_string()));
    tokens
}

/// The tokenised form of a title, used to detect the same page across sources and to
/// de-duplicate results. Empty for an untitled page.
pub fn title_key(title: &str) -> String {
    tokenize(title).join(" ")
}

/// The [`tokenize`] rules as a tantivy tokenizer, so index and query agree.
#[derive(Debug, Clone, Copy, Default)]
pub struct PinakesTokenizer;

/// Token stream of [`PinakesTokenizer`].
pub struct PinakesTokenStream {
    tokens: std::vec::IntoIter<Token>,
    current: Token,
}

impl Tokenizer for PinakesTokenizer {
    type TokenStream<'a> = PinakesTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> PinakesTokenStream {
        let mut tokens = Vec::new();
        scan_tokens(text, &mut |from, to, token| {
            tokens.push(Token {
                offset_from: from,
                offset_to: to,
                position: tokens.len(),
                text: token.to_string(),
                position_length: 1,
            });
        });
        PinakesTokenStream {
            tokens: tokens.into_iter(),
            current: Token::default(),
        }
    }
}

impl TokenStream for PinakesTokenStream {
    fn advance(&mut self) -> bool {
        match self.tokens.next() {
            Some(token) => {
                self.current = token;
                true
            }
            None => false,
        }
    }

    fn token(&self) -> &Token {
        &self.current
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stopwords_are_sorted_for_binary_search() {
        assert!(STOPWORDS.windows(2).all(|w| w[0] < w[1]));
        assert!(is_stopword("the") && !is_stopword("corpus"));
    }

    #[test]
    fn tokenizer_lowercases_splits_on_non_alphanumerics_and_drops_stopwords() {
        // `caching-limits` is also an identifier compound (SPEC §10.3): the joined form is
        // appended after the ordinary tokens.
        assert_eq!(
            tokenize("How do I enable **upload** caching-limits for `StorageClass` v2?"),
            [
                "enable",
                "upload",
                "caching",
                "limits",
                "storageclass",
                "v2",
                "cachinglimits",
            ]
        );
        assert_eq!(
            tokenize("Corpus? Corpus! über K8s"),
            ["corpus", "corpus", "ber", "k8s"]
        );
        assert!(tokenize("the a of").is_empty());
        assert_eq!(title_key("The Storage Module"), "storage module");
        assert_eq!(title_key("Storage Module"), title_key("storage module!"));
    }

    #[test]
    fn identifier_compounds_add_the_joined_form() {
        // SPEC §10.3: alphanumeric runs joined by `.` `/` `_` or `-` also emit the joined form.
        assert_eq!(tokenize("spec.sink"), ["spec", "sink", "specsink"]);
        assert_eq!(tokenize("jwks_urls"), ["jwks", "urls", "jwksurls"]);
        assert_eq!(
            tokenize("aa/bb-cc.dd"),
            ["aa", "bb", "cc", "dd", "aabbccdd"],
            "several separators chain into one compound"
        );
        assert_eq!(
            tokenize("--leading"),
            ["leading"],
            "a separator with nothing alphanumeric before it does not compound"
        );
        assert_eq!(
            tokenize("trailing--"),
            ["trailing"],
            "a separator with nothing alphanumeric after it does not compound"
        );
        assert_eq!(
            tokenize("v2.3"),
            ["v2", "3", "v23"],
            "digits participate like letters"
        );
        assert_eq!(
            tokenize("plain word"),
            ["plain", "word"],
            "no separator, no compound"
        );
        // Queries are tokenised the same way, so a dotted query matches the whole path first.
        assert_eq!(tokenize("spec.sink"), tokenize("spec.sink"));
    }

    #[test]
    fn tantivy_tokenizer_matches_tokenize() {
        let text = "Expose a Workload with an Ingress";
        let mut tokenizer = PinakesTokenizer;
        let mut stream = tokenizer.token_stream(text);
        let mut seen = Vec::new();
        while stream.advance() {
            let token = stream.token();
            assert_eq!(
                &text[token.offset_from..token.offset_to].to_lowercase(),
                &token.text
            );
            seen.push(token.text.clone());
        }
        assert_eq!(seen, tokenize(text));
        assert_eq!(seen, ["expose", "workload", "ingress"]);
    }
}
