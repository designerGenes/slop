use std::path::{Path, PathBuf};

use tantivy::schema::{Field, IndexRecordOption, OwnedValue, Schema, TextFieldIndexing, TextOptions, STRING, STORED, FAST};
use tantivy::tokenizer::{Token, TokenStream, Tokenizer};
use tantivy::{doc, Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument};

use crate::config::Config;
use crate::error::SlopError;

pub const CODE_TOKENIZER_NAME: &str = "code";

#[derive(Clone)]
struct CodeTokenizer;

impl Tokenizer for CodeTokenizer {
    type TokenStream<'a> = CodeTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> CodeTokenStream {
        let tokens = tokenize_code(text);
        CodeTokenStream {
            tokens,
            index: 0,
            current: Token::default(),
        }
    }
}

struct CodeTokenStream {
    tokens: Vec<String>,
    index: usize,
    current: Token,
}

impl TokenStream for CodeTokenStream {
    fn advance(&mut self) -> bool {
        if self.index >= self.tokens.len() {
            return false;
        }
        self.current.text = self.tokens[self.index].clone();
        self.current.position = self.index;
        self.current.position_length = 1;
        self.index += 1;
        true
    }

    fn token(&self) -> &Token {
        &self.current
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.current
    }
}

fn tokenize_code(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    for raw_token in text.split(|c: char| !c.is_alphanumeric()) {
        if raw_token.is_empty() {
            continue;
        }
        let lower = raw_token.to_lowercase();
        result.push(lower.clone());
        for sub in split_identifier(&lower) {
            if sub != lower && !sub.is_empty() {
                result.push(sub);
            }
        }
    }
    result
}

fn split_identifier(token: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = token.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '_' {
            if !current.is_empty() {
                parts.push(current.clone());
                current.clear();
            }
        } else if i > 0 && c.is_uppercase() && chars[i - 1].is_lowercase() {
            if !current.is_empty() {
                parts.push(current.clone());
            }
            current.clear();
            current.push(c);
        } else if i > 0 && c.is_uppercase() && chars[i - 1].is_uppercase() && i + 1 < chars.len() && chars[i + 1].is_lowercase() {
            if !current.is_empty() {
                parts.push(current.clone());
            }
            current.clear();
            current.push(c);
        } else {
            current.push(c);
        }
        i += 1;
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

pub struct IndexFields {
    pub path: Field,
    pub rel_path: Field,
    pub content: Field,
    pub symbols: Field,
    pub mtime: Field,
    pub size: Field,
}

impl IndexFields {
    fn build_schema() -> (Schema, Self) {
        let mut builder = Schema::builder();

        let code_indexing = TextFieldIndexing::default()
            .set_tokenizer(CODE_TOKENIZER_NAME)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions);

        let content_opts = TextOptions::default()
            .set_indexing_options(code_indexing.clone());

        let symbols_opts = TextOptions::default()
            .set_indexing_options(code_indexing);

        let path = builder.add_text_field("path", STRING | STORED);
        let rel_path = builder.add_text_field("rel_path", STRING | STORED);
        let content = builder.add_text_field("content", content_opts);
        let symbols = builder.add_text_field("symbols", symbols_opts);
        let mtime = builder.add_u64_field("mtime", STORED | FAST);
        let size = builder.add_u64_field("size", STORED | FAST);

        let schema = builder.build();
        (schema, Self { path, rel_path, content, symbols, mtime, size })
    }
}

pub fn corpus_fingerprint(corpus_root: &Path) -> String {
    let canonical = corpus_root.to_string_lossy();
    let hash = blake3::hash(canonical.as_bytes());
    hash.to_hex().as_str()[..16].to_string()
}

pub fn resolve_index_dir(config: &Config) -> PathBuf {
    config
        .index_dir
        .as_deref()
        .map(|p| crate::pathing::expand_tilde(p))
        .or_else(crate::config::default_index_dir)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join(".cache")
                .join("slop")
                .join("index")
        })
}

pub fn ensure_index(
    corpus_root: &Path,
    config: &Config,
    force_reindex: bool,
) -> Result<(Index, IndexReader, IndexFields), SlopError> {
    let index_base = resolve_index_dir(config);
    let fingerprint = corpus_fingerprint(corpus_root);
    let index_path = index_base.join(&fingerprint);

    std::fs::create_dir_all(&index_path).map_err(|e| {
        SlopError::IndexBuildFailure(format!("mkdir {}: {}", index_path.display(), e))
    })?;

    let (schema, fields) = IndexFields::build_schema();

    let index = Index::open_in_dir(&index_path)
        .or_else(|_| Index::create_in_dir(&index_path, schema.clone()))
        .map_err(|e| {
            SlopError::IndexBuildFailure(format!("open_or_create: {}", e))
        })?;

    index
        .tokenizers()
        .register(CODE_TOKENIZER_NAME, CodeTokenizer);

    let mut writer = index.writer(50_000_000).map_err(|e| {
        SlopError::IndexBuildFailure(format!("writer: {}", e))
    })?;

    if force_reindex {
        writer.delete_all_documents().map_err(|e| {
            SlopError::IndexBuildFailure(format!("delete_all: {}", e))
        })?;
        index_corpus(&mut writer, &fields, corpus_root)?;
    } else {
        incremental_update(&index, &mut writer, &fields, corpus_root)?;
    }

    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::OnCommitWithDelay)
        .try_into()
        .map_err(|e| SlopError::IndexBuildFailure(format!("reader: {}", e)))?;

    Ok((index, reader, fields))
}

fn index_corpus(
    writer: &mut IndexWriter,
    fields: &IndexFields,
    corpus_root: &Path,
) -> Result<(), SlopError> {
    let files = crate::repomap::discover_source_files(corpus_root);
    for file_path in &files {
        index_file(writer, fields, corpus_root, Path::new(file_path))?;
    }
    writer.commit().map_err(|e| {
        SlopError::IndexBuildFailure(format!("commit: {}", e))
    })?;
    Ok(())
}

fn incremental_update(
    index: &Index,
    writer: &mut IndexWriter,
    fields: &IndexFields,
    corpus_root: &Path,
) -> Result<(), SlopError> {
    let files = crate::repomap::discover_source_files(corpus_root);

    let reader = index.reader().map_err(|e| {
        SlopError::IndexBuildFailure(format!("reader for update: {}", e))
    })?;
    let searcher = reader.searcher();

    let mut indexed_paths: std::collections::HashSet<String> = std::collections::HashSet::new();

    for file_path_str in &files {
        let file_path = Path::new(file_path_str);
        let abs_str = file_path_str.clone();
        indexed_paths.insert(abs_str.clone());

        let mtime = match std::fs::metadata(file_path) {
            Ok(m) => m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0),
            Err(_) => continue,
        };

        let need_reindex = check_need_reindex(&searcher, fields, &abs_str, mtime);

        if need_reindex {
            writer.delete_term(tantivy::Term::from_field_text(fields.path, &abs_str));
            index_file(writer, fields, corpus_root, file_path)?;
        }
    }

    writer.commit().map_err(|e| {
        SlopError::IndexBuildFailure(format!("commit: {}", e))
    })?;
    Ok(())
}

fn check_need_reindex(
    searcher: &tantivy::Searcher,
    fields: &IndexFields,
    abs_path: &str,
    mtime: u64,
) -> bool {
    use tantivy::query::QueryParser;
    use tantivy::collector::TopDocs;

    let query_parser = QueryParser::for_index(searcher.index(), vec![fields.path]);
    let Ok(query) = query_parser.parse_query(&abs_path) else {
        return true;
    };
    let Ok(top) = searcher.search(&query, &TopDocs::with_limit(1)) else {
        return true;
    };
    if top.is_empty() {
        return true;
    }
    let (_, doc_addr) = top[0];
    let Ok(doc) = searcher.doc::<TantivyDocument>(doc_addr) else {
        return true;
    };
    let stored_mtime = doc
        .get_first(fields.mtime)
        .and_then(|v| match v {
            OwnedValue::U64(v) => Some(*v),
            _ => None,
        })
        .unwrap_or(0);
    stored_mtime < mtime
}

fn index_file(
    writer: &mut IndexWriter,
    fields: &IndexFields,
    corpus_root: &Path,
    file_path: &Path,
) -> Result<(), SlopError> {
    let abs_str = file_path.to_string_lossy().to_string();
    let rel_path = file_path
        .strip_prefix(corpus_root)
        .unwrap_or(file_path)
        .to_string_lossy()
        .to_string();

    let content = std::fs::read_to_string(file_path).unwrap_or_default();
    let mtime = std::fs::metadata(file_path)
        .ok()
        .and_then(|m| {
            m.modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
        })
        .unwrap_or(0);
    let size = std::fs::metadata(file_path).map(|m| m.len()).unwrap_or(0);

    let tags = crate::repomap::tags::extract_tags(&abs_str, &rel_path);
    let symbols: Vec<String> = tags
        .iter()
        .filter(|t| t.kind == crate::repomap::tags::TagKind::Def)
        .map(|t| t.name.clone())
        .collect();
    let symbols_str = symbols.join(" ");

    writer
        .add_document(doc!(
            fields.path => abs_str,
            fields.rel_path => rel_path,
            fields.content => content,
            fields.symbols => symbols_str,
            fields.mtime => mtime,
            fields.size => size,
        ))
        .map_err(|e| SlopError::IndexBuildFailure(format!("add_doc: {}", e)))?;

    Ok(())
}

pub fn query_index(
    index: &Index,
    reader: &IndexReader,
    fields: &IndexFields,
    query_text: &str,
    top_k: usize,
) -> Result<Vec<(f32, String, String)>, SlopError> {
    use tantivy::collector::TopDocs;
    use tantivy::query::QueryParser;

    let mut query_parser = QueryParser::for_index(index, vec![fields.content, fields.symbols]);
    query_parser.set_field_boost(fields.symbols, 3.0);

    let query = query_parser
        .parse_query(query_text)
        .map_err(|e| SlopError::RetrievalQueryFailure(format!("parse: {}", e)))?;

    let searcher = reader.searcher();
    let top_docs = searcher
        .search(&query, &TopDocs::with_limit(top_k * 4))
        .map_err(|e| SlopError::RetrievalQueryFailure(format!("search: {}", e)))?;

    let mut results = Vec::new();
    for (score, doc_addr) in top_docs {
        let doc: TantivyDocument = searcher
            .doc(doc_addr)
            .map_err(|e| SlopError::RetrievalQueryFailure(format!("doc: {}", e)))?;
        let path = doc
            .get_first(fields.path)
            .and_then(|v| match v {
                OwnedValue::Str(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let rel = doc
            .get_first(fields.rel_path)
            .and_then(|v| match v {
                OwnedValue::Str(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_default();
        results.push((score, path, rel));
    }

    Ok(results)
}
