use log::info;
use tantivy::{schema::*, ReloadPolicy};
use tantivy::{Index, IndexWriter};
use tower_lsp::lsp_types::{
    DocumentHighlight, DocumentHighlightKind, Location, MessageType, Position, Range,
    TextDocumentItem, TextDocumentPositionParams, Url,
};
use tower_lsp::Client;

// use ::phf::{phf_map, Map};

// use std::fs::DirEntry;

use lib_ruby_parser::source::DecodedInput;
// use lib_ruby_parser::traverse::finder::PatternError;
// ---
// Importing tantivy...
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, Query, QueryParser, TermQuery};
use tantivy::schema::{self, *};
// use tempfile::TempDir;

use std::any::Any;
use std::error::Error;
use std::fs::{self, read_to_string};
use std::ops::AddAssign;
use std::path::Path;

use filetime::FileTime;
use lib_ruby_parser::{nodes::*, Node, Parser, ParserOptions};
use walkdir::WalkDir;

pub struct Persistence {
    schema: Schema,
    schema_fields: SchemaFields,
    index: Index,
    workspace_path: String,
}

struct SchemaFields {
    file_path_id: Field,
    file_path: Field,
    category_field: Field,
    fuzzy_ruby_scope_field: Field,
    name_field: Field,
    node_type_field: Field,
    line_field: Field,
    start_column_field: Field,
    end_column_field: Field,
    columns_field: Field,
}

#[derive(Debug)]
struct FuzzyNode<'a> {
    category: &'a str,
    fuzzy_ruby_scope: Vec<String>,
    name: String,
    node_type: &'a str,
    line: usize,
    start_column: usize,
    end_column: usize,
}

use phf::phf_map;

static USAGE_TYPE_RESTRICTIONS: phf::Map<&'static str, &[&str]> = phf_map! {
    "Alias" => &["Alias", "Def", "Defs"],
    "Const" => &["Casgn", "Class", "Module"],
    "CSend" => &["Alias", "Def", "Defs"],
    "Cvar" => &["Cvasgn"],
    "Gvar" => &["Gvasgn"],
    "Ivar" => &["Ivasgn"],
    "Lvar" => &["Arg", "Kwarg", "Kwoptarg", "Kwrestarg", "Lvasgn", "MatchVar", "Optarg", "Restarg", "Shadowarg"],
    "Send" => &["Alias", "Def", "Defs"],
    "Super" => &["Alias", "Def", "Defs"],
    "ZSuper" => &["Alias", "Def", "Defs"],
};

static ASSIGNMENT_TYPE_RESTRICTIONS: phf::Map<&'static str, &[&str]> = phf_map! {
    "Alias" => &["Alias", "CSend", "Send", "Super", "ZSuper"],
    "Arg" => &["Lvar"],
    "Casgn" => &["Const"],
    "Class" => &["Const"],
    "Cvasgn" => &["Cvar"],
    "Def" => &["Alias", "CSend", "Send", "Super", "ZSuper"],
    "Defs" => &["Alias", "CSend", "Send", "Super", "ZSuper"],
    "Gvasgn" => &["Gvar"],
    "Ivasgn" => &["Ivar"],
    "Kwarg" => &["Lvar"],
    "Kwoptarg" => &["Lvar"],
    "Kwrestarg" => &["Lvar"],
    "Lvasgn" => &["Lvar"],
    "MatchVar" => &["Lvar"],
    "Module" => &["Const"],
    "Optarg" => &["Lvar"],
    "Restarg" => &["Lvar"],
    "Shadowarg" => &["Lvar"],
};

impl Persistence {
    pub fn new() -> tantivy::Result<Persistence> {
        let mut schema_builder = Schema::builder();
        let schema_fields = SchemaFields {
            // file_path_id: schema_builder.add_text_field("file_path_id", TEXT | STORED),
            file_path_id: schema_builder.add_text_field(
                "file_path_id",
                TextOptions::default()
                    .set_indexing_options(
                        TextFieldIndexing::default()
                            .set_tokenizer("raw")
                            .set_index_option(IndexRecordOption::Basic),
                    )
                    .set_stored(),
            ),
            file_path: schema_builder.add_text_field(
                "file_path",
                TextOptions::default()
                    .set_indexing_options(
                        TextFieldIndexing::default()
                            .set_tokenizer("raw")
                            .set_index_option(IndexRecordOption::Basic),
                    )
                    .set_stored(),
            ),
            category_field: schema_builder.add_text_field(
                "category",
                TextOptions::default()
                    .set_indexing_options(
                        TextFieldIndexing::default()
                            .set_tokenizer("raw")
                            .set_index_option(IndexRecordOption::Basic),
                    )
                    .set_stored(),
            ),
            fuzzy_ruby_scope_field: schema_builder.add_text_field(
                "fuzzy_ruby_scope",
                TextOptions::default()
                    .set_indexing_options(
                        TextFieldIndexing::default()
                            .set_tokenizer("raw")
                            .set_index_option(IndexRecordOption::Basic),
                    )
                    .set_stored(),
            ),
            name_field: schema_builder.add_text_field(
                "name",
                TextOptions::default()
                    .set_indexing_options(
                        TextFieldIndexing::default()
                            .set_tokenizer("raw")
                            .set_index_option(IndexRecordOption::Basic),
                    )
                    .set_stored(),
            ),
            node_type_field: schema_builder.add_text_field(
                "node_type",
                TextOptions::default()
                    .set_indexing_options(
                        TextFieldIndexing::default()
                            .set_tokenizer("raw")
                            .set_index_option(IndexRecordOption::Basic),
                    )
                    .set_stored(),
            ),
            line_field: schema_builder.add_u64_field("line", INDEXED | STORED),
            start_column_field: schema_builder.add_u64_field("start_column", INDEXED | STORED),
            end_column_field: schema_builder.add_u64_field("end_column", INDEXED | STORED),
            columns_field: schema_builder.add_u64_field("columns", INDEXED | STORED),
        };

        let schema = schema_builder.build();
        let index = Index::create_in_ram(schema.clone());
        let workspace_path = "".to_string();

        Ok(Self {
            schema,
            schema_fields,
            index,
            workspace_path,
        })
    }

    pub fn set_workspace_path(&mut self, root_uri: Option<tower_lsp::lsp_types::Url>) {
        if let Some(uri) = root_uri {
            self.workspace_path = uri.path().to_string();
        }
    }

    pub fn reindex_modified_files(&self) {}

    pub fn reindex_modified_file(&self, text_document: TextDocumentItem) -> tantivy::Result<()> {
        let mut index_writer = self.index.writer(50_000_000)?;
        let mut documents = Vec::new();

        parse(text_document.text, &mut documents);

        let relative_path = text_document.uri.path().replace(&self.workspace_path, "");
        let file_path_id = blake3::hash(&relative_path.as_bytes());

        let file_path_id_term =
            Term::from_field_text(self.schema_fields.file_path_id, &file_path_id.to_string());
        index_writer.delete_term(file_path_id_term);

        for document in documents {
            let mut fuzzy_doc = Document::default();

            fuzzy_doc.add_text(self.schema_fields.file_path_id, &file_path_id.to_string());

            for path_part in relative_path.split("/") {
                if path_part.len() > 0 {
                    fuzzy_doc.add_text(self.schema_fields.file_path, path_part);
                }
            }

            for fuzzy_scope in document.fuzzy_ruby_scope {
                fuzzy_doc.add_text(self.schema_fields.fuzzy_ruby_scope_field, fuzzy_scope);
            }

            fuzzy_doc.add_text(
                self.schema_fields.category_field,
                document.category.to_string(),
            );
            fuzzy_doc.add_text(self.schema_fields.name_field, document.name);
            fuzzy_doc.add_text(self.schema_fields.node_type_field, document.node_type);
            fuzzy_doc.add_u64(
                self.schema_fields.line_field,
                document.line.try_into().unwrap(),
            );
            fuzzy_doc.add_u64(
                self.schema_fields.start_column_field,
                document.start_column.try_into().unwrap(),
            );
            fuzzy_doc.add_u64(
                self.schema_fields.end_column_field,
                document.end_column.try_into().unwrap(),
            );

            let start_col = document.start_column;
            let end_col = document.end_column;
            let col_range = start_col..(end_col + 1);
            for col in col_range {
                fuzzy_doc.add_u64(self.schema_fields.columns_field, col as u64);
            }

            info!("fuzzy_doc:");
            info!("{:#?}", fuzzy_doc);

            index_writer.add_document(fuzzy_doc)?;
        }

        index_writer.commit()?;

        Ok(())
    }

    pub fn find_definitions(
        &self,
        params: TextDocumentPositionParams,
    ) -> tantivy::Result<Vec<Location>> {
        let uri = params.text_document.uri.path();
        let relative_path = params
            .text_document
            .uri
            .path()
            .replace(&self.workspace_path, "");
        info!("{:#?}", relative_path);

        let position = params.position;

        let reader = self
            .index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommit)
            .try_into()?;

        let searcher = reader.searcher();
        // let query_parser = QueryParser::for_index(&self.index, vec![self.schema_fields.category_field, self.schema_fields.file_path_id, self.schema_fields.line_field, self.schema_fields.columns_field]);

        let character_position = position.character;
        let character_line = position.line;
        // let query_string = format!("+category:usage AND +file_path_id:\"{relative_path}\" AND +line:{character_line} AND +columns:{character_position}");
        // let query = query_parser.parse_query(&query_string)?;

        let file_path_id = blake3::hash(&relative_path.as_bytes());

        info!("file_path_id:");
        info!("{}", &file_path_id);

        let file_path_query: Box<dyn Query> = Box::new(TermQuery::new(
            Term::from_field_text(self.schema_fields.file_path_id, &file_path_id.to_string()),
            IndexRecordOption::Basic,
        ));
        let category_query: Box<dyn Query> = Box::new(TermQuery::new(
            Term::from_field_text(self.schema_fields.category_field, "usage"),
            IndexRecordOption::Basic,
        ));
        let line_query: Box<dyn Query> = Box::new(TermQuery::new(
            Term::from_field_u64(self.schema_fields.line_field, character_line.into()),
            IndexRecordOption::Basic,
        ));
        let column_query: Box<dyn Query> = Box::new(TermQuery::new(
            Term::from_field_u64(self.schema_fields.columns_field, character_position.into()),
            IndexRecordOption::Basic,
        ));

        info!("{:#?}", file_path_query);

        let query = BooleanQuery::new(vec![
            (Occur::Must, file_path_query),
            (Occur::Must, category_query),
            (Occur::Must, line_query),
            (Occur::Must, column_query),
        ]);

        let usage_top_docs = searcher.search(&query, &TopDocs::with_limit(1))?;

        let mut locations = Vec::new();

        if usage_top_docs.len() == 0 {
            info!("No usages docs found");
            return Ok(locations);
        }

        let doc_address = usage_top_docs[0].1;
        let retrieved_doc = searcher.doc(doc_address)?;

        info!("retrieved usage doc:");
        info!("{}", self.schema.to_json(&retrieved_doc));

        let category_query: Box<dyn Query> = Box::new(TermQuery::new(
            Term::from_field_text(self.schema_fields.category_field, "assignment"),
            IndexRecordOption::Basic,
        ));

        let usage_name = retrieved_doc
            .get_first(self.schema_fields.name_field)
            .unwrap()
            .as_text()
            .unwrap();
        let usage_type = retrieved_doc
            .get_first(self.schema_fields.node_type_field)
            .unwrap()
            .as_text()
            .unwrap();

        let name_query: Box<dyn Query> = Box::new(TermQuery::new(
            Term::from_field_text(self.schema_fields.name_field, usage_name),
            IndexRecordOption::Basic,
        ));

        let mut assignment_type_queries = vec![];

        for possible_assignment_type in USAGE_TYPE_RESTRICTIONS.get(usage_type).unwrap().iter() {
            let assignment_type_query: Box<dyn Query> = Box::new(TermQuery::new(
                Term::from_field_text(self.schema_fields.node_type_field, possible_assignment_type),
                IndexRecordOption::Basic,
            ));

            assignment_type_queries.push((Occur::Should, assignment_type_query));
        }

        let assignment_type_query = BooleanQuery::new(assignment_type_queries);

        let mut queries = vec![
            (Occur::Must, category_query),
            (Occur::Must, name_query),
            (Occur::Must, Box::new(assignment_type_query)),
        ];

        let usage_fuzzy_scope = retrieved_doc.get_all(self.schema_fields.fuzzy_ruby_scope_field);

        match usage_type {
            // "Alias" => {},
            // "Const" => {},
            // "CSend" => {},
            // todo: improved indexed scopes so there is a separate class scope, etc
            // "Cvar" => {},
            // "Gvar" => {},
            // todo: improved indexed scopes so there is a separate class scope, etc
            // "Ivar" => {},
            // todo: improved to be more accurate
            "Lvar" => {
                for scope_name in usage_fuzzy_scope {
                    let scope_query: Box<dyn Query> = Box::new(TermQuery::new(
                        Term::from_field_text(
                            self.schema_fields.fuzzy_ruby_scope_field,
                            scope_name.as_text().unwrap(),
                        ),
                        IndexRecordOption::Basic,
                    ));

                    queries.push((Occur::Must, scope_query));
                }
            }
            // "Send" => {},
            // "Super" => {},
            // "ZSuper" => {},
            _ => {
                for scope_name in usage_fuzzy_scope {
                    let scope_query: Box<dyn Query> = Box::new(TermQuery::new(
                        Term::from_field_text(
                            self.schema_fields.fuzzy_ruby_scope_field,
                            scope_name.as_text().unwrap(),
                        ),
                        IndexRecordOption::Basic,
                    ));

                    queries.push((Occur::Should, scope_query));
                }
            }
        };

        let query = BooleanQuery::new(queries);
        let assignments_top_docs = searcher.search(&query, &TopDocs::with_limit(50))?;

        // let query_parser = QueryParser::for_index(&self.index, vec![self.schema_fields.file_path_id, self.schema_fields.name_field]);
        // let query_string = format!("category:assignment AND name:\"{usage_name}\"");
        // let query = query_parser.parse_query(&query_string)?;
        // let assignments_top_docs = searcher.search(&query, &TopDocs::with_limit(50))?;

        for (_score, doc_address) in assignments_top_docs {
            let retrieved_doc = searcher.doc(doc_address)?;

            info!("retrieved doc:");
            info!("{}", self.schema.to_json(&retrieved_doc));

            let file_path: String = retrieved_doc
                .get_all(self.schema_fields.file_path)
                .flat_map(Value::as_text)
                .collect::<Vec<&str>>()
                .join("/");

            let absolute_file_path = format!("{}/{}", &self.workspace_path, &file_path);
            let doc_uri = Url::from_file_path(&absolute_file_path).unwrap();

            let start_line = retrieved_doc
                .get_first(self.schema_fields.line_field)
                .unwrap()
                .as_u64()
                .unwrap() as u32;
            let start_column = retrieved_doc
                .get_first(self.schema_fields.start_column_field)
                .unwrap()
                .as_u64()
                .unwrap() as u32;
            let start_position = Position::new(start_line, start_column);
            let end_column = retrieved_doc
                .get_first(self.schema_fields.end_column_field)
                .unwrap()
                .as_u64()
                .unwrap() as u32;
            let end_position = Position::new(start_line, end_column);

            let doc_range = Range::new(start_position, end_position);
            let location = Location::new(doc_uri, doc_range);

            info!("location:");
            info!("{:#?}", location);

            locations.push(location);
        }

        Ok(locations)
    }

    pub fn find_highlights(
        &self,
        params: TextDocumentPositionParams,
    ) -> tantivy::Result<Vec<DocumentHighlight>> {
        let uri = params.text_document.uri.path();
        let relative_path = params
            .text_document
            .uri
            .path()
            .replace(&self.workspace_path, "");

        let position = params.position;

        let reader = self
            .index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommit)
            .try_into()?;

        let searcher = reader.searcher();
        // let query_parser = QueryParser::for_index(&self.index, vec![self.schema_fields.category_field, self.schema_fields.file_path_id, self.schema_fields.line_field, self.schema_fields.columns_field]);

        let character_position = position.character;
        let character_line = position.line;
        // let query_string = format!("+category:usage AND +file_path_id:\"{relative_path}\" AND +line:{character_line} AND +columns:{character_position}");
        // let query = query_parser.parse_query(&query_string)?;

        let file_path_id = blake3::hash(&relative_path.as_bytes());

        info!("file_path_id:");
        info!("{}", &file_path_id);

        let file_path_query: Box<dyn Query> = Box::new(TermQuery::new(
            Term::from_field_text(self.schema_fields.file_path_id, &file_path_id.to_string()),
            IndexRecordOption::Basic,
        ));
        let line_query: Box<dyn Query> = Box::new(TermQuery::new(
            Term::from_field_u64(self.schema_fields.line_field, character_line.into()),
            IndexRecordOption::Basic,
        ));
        let column_query: Box<dyn Query> = Box::new(TermQuery::new(
            Term::from_field_u64(self.schema_fields.columns_field, character_position.into()),
            IndexRecordOption::Basic,
        ));

        let query = BooleanQuery::new(vec![
            (Occur::Must, file_path_query),
            (Occur::Must, line_query),
            (Occur::Must, column_query),
        ]);

        let usage_top_docs = searcher.search(&query, &TopDocs::with_limit(1))?;

        let mut highlights = Vec::new();

        if usage_top_docs.len() == 0 {
            info!("No highlight usages docs found");
            return Ok(highlights);
        }

        let doc_address = usage_top_docs[0].1;
        let retrieved_doc = searcher.doc(doc_address)?;

        // info!("retrieved highlight usage doc:");
        // info!("{}", self.schema.to_json(&retrieved_doc));

        let usage_name = retrieved_doc
            .get_first(self.schema_fields.name_field)
            .unwrap()
            .as_text()
            .unwrap();
        let token_type = retrieved_doc
            .get_first(self.schema_fields.node_type_field)
            .unwrap()
            .as_text()
            .unwrap();

        let file_path_query: Box<dyn Query> = Box::new(TermQuery::new(
            Term::from_field_text(self.schema_fields.file_path_id, &file_path_id.to_string()),
            IndexRecordOption::Basic,
        ));

        let name_query: Box<dyn Query> = Box::new(TermQuery::new(
            Term::from_field_text(self.schema_fields.name_field, usage_name),
            IndexRecordOption::Basic,
        ));

        let mut highlight_token_queries = vec![];

        for possible_assignment_type in USAGE_TYPE_RESTRICTIONS
            .get(token_type)
            .unwrap_or(&[].as_slice())
            .iter()
        {
            let assignment_type_query: Box<dyn Query> = Box::new(TermQuery::new(
                Term::from_field_text(self.schema_fields.node_type_field, possible_assignment_type),
                IndexRecordOption::Basic,
            ));

            highlight_token_queries.push((Occur::Should, assignment_type_query));
        }
        for possible_usage_type in ASSIGNMENT_TYPE_RESTRICTIONS
            .get(token_type)
            .unwrap_or(&[].as_slice())
            .iter()
        {
            let usage_type_query: Box<dyn Query> = Box::new(TermQuery::new(
                Term::from_field_text(self.schema_fields.node_type_field, possible_usage_type),
                IndexRecordOption::Basic,
            ));

            highlight_token_queries.push((Occur::Should, usage_type_query));
        }

        let usage_type_query: Box<dyn Query> = Box::new(TermQuery::new(
            Term::from_field_text(self.schema_fields.node_type_field, token_type),
            IndexRecordOption::Basic,
        ));

        highlight_token_queries.push((Occur::Should, usage_type_query));

        let token_type_query = BooleanQuery::new(highlight_token_queries);

        let mut queries = vec![
            (Occur::Must, file_path_query),
            (Occur::Must, name_query),
            (Occur::Must, Box::new(token_type_query)),
        ];

        let usage_fuzzy_scope = retrieved_doc.get_all(self.schema_fields.fuzzy_ruby_scope_field);

        match token_type {
            // "Alias" => {},
            // "Const" => {},
            // "CSend" => {},
            // todo: improved indexed scopes so there is a separate class scope, etc
            // "Cvar" => {},
            // "Gvar" => {},
            // todo: improved indexed scopes so there is a separate class scope, etc
            // "Ivar" => {},
            // todo: improved to be more accurate
            "Lvar" => {
                for scope_name in usage_fuzzy_scope {
                    let scope_query: Box<dyn Query> = Box::new(TermQuery::new(
                        Term::from_field_text(
                            self.schema_fields.fuzzy_ruby_scope_field,
                            scope_name.as_text().unwrap(),
                        ),
                        IndexRecordOption::Basic,
                    ));

                    queries.push((Occur::Must, scope_query));
                }
            }
            // "Send" => {},
            // "Super" => {},
            // "ZSuper" => {},
            _ => {
                for scope_name in usage_fuzzy_scope {
                    let scope_query: Box<dyn Query> = Box::new(TermQuery::new(
                        Term::from_field_text(
                            self.schema_fields.fuzzy_ruby_scope_field,
                            scope_name.as_text().unwrap(),
                        ),
                        IndexRecordOption::Basic,
                    ));

                    queries.push((Occur::Should, scope_query));
                }
            }
        };

        let query = BooleanQuery::new(queries);
        let top_docs = searcher.search(&query, &TopDocs::with_limit(100))?;

        // let query_parser = QueryParser::for_index(&self.index, vec![self.schema_fields.file_path_id, self.schema_fields.name_field]);
        // let query_string = format!("category:assignment AND name:\"{usage_name}\"");
        // let query = query_parser.parse_query(&query_string)?;
        // let assignments_top_docs = searcher.search(&query, &TopDocs::with_limit(50))?;

        for (_score, doc_address) in top_docs {
            let retrieved_doc = searcher.doc(doc_address)?;

            // info!("retrieved doc:");
            // info!("{}", self.schema.to_json(&retrieved_doc));

            let file_path: String = retrieved_doc
                .get_all(self.schema_fields.file_path)
                .flat_map(Value::as_text)
                .collect::<Vec<&str>>()
                .join("/");

            let absolute_file_path = format!("{}/{}", &self.workspace_path, &file_path);
            let doc_uri = Url::from_file_path(&absolute_file_path).unwrap();

            let start_line = retrieved_doc
                .get_first(self.schema_fields.line_field)
                .unwrap()
                .as_u64()
                .unwrap() as u32;
            let start_column = retrieved_doc
                .get_first(self.schema_fields.start_column_field)
                .unwrap()
                .as_u64()
                .unwrap() as u32;
            let start_position = Position::new(start_line, start_column);
            let end_column = retrieved_doc
                .get_first(self.schema_fields.end_column_field)
                .unwrap()
                .as_u64()
                .unwrap() as u32;
            let end_position = Position::new(start_line, end_column);

            let range = Range::new(start_position, end_position);

            let category = retrieved_doc
                .get_first(self.schema_fields.category_field)
                .unwrap()
                .as_text()
                .unwrap();

            let kind = if category == "assignment" {
                Some(DocumentHighlightKind::WRITE)
            } else {
                Some(DocumentHighlightKind::READ)
            };

            let document_highlight = DocumentHighlight { range, kind };

            // info!("location:");
            // info!("{:#?}", location);

            highlights.push(document_highlight);
        }

        Ok(highlights)
    }
}

fn parse(contents: String, documents: &mut Vec<FuzzyNode>) {
    // let contents = fs::read_to_string(entry.path()).expect("Unable to read");
    let options = ParserOptions {
        buffer_name: "(eval)".to_string(),
        record_tokens: false,
        ..Default::default()
    };
    let parser = Parser::new(contents, options);
    let parser_result = parser.do_parse();
    let input = parser_result.input;
    let ast = match parser_result.ast {
        Some(a) => *a,
        None => return,
    };

    let mut scope = Vec::new();

    serialize(&ast, documents, &mut scope, &input);
}

fn serialize(
    node: &Node,
    documents: &mut Vec<FuzzyNode>,
    fuzzy_scope: &mut Vec<String>,
    input: &DecodedInput,
) {
    match &node {
        Node::Alias(Alias { to, from, .. }) => {
            if let Node::Sym(sym) = *to.to_owned() {
                let (lineno, begin_pos) = input.line_col_for_pos(sym.expression_l.begin).unwrap();
                let (_lineno, end_pos) = input.line_col_for_pos(sym.expression_l.end).unwrap();

                documents.push(FuzzyNode {
                    category: "assignment",
                    fuzzy_ruby_scope: fuzzy_scope.clone(),
                    name: sym.name.to_string_lossy(),
                    node_type: "Alias",
                    line: lineno,
                    start_column: begin_pos,
                    end_column: end_pos,
                });
            }

            if let Node::Sym(sym) = *from.to_owned() {
                let (lineno, begin_pos) = input.line_col_for_pos(sym.expression_l.begin).unwrap();
                let (_lineno, end_pos) = input.line_col_for_pos(sym.expression_l.end).unwrap();

                documents.push(FuzzyNode {
                    category: "usage",
                    fuzzy_ruby_scope: fuzzy_scope.clone(),
                    name: sym.name.to_string_lossy(),
                    node_type: "Alias",
                    line: lineno,
                    start_column: begin_pos,
                    end_column: end_pos,
                });
            }
        }

        Node::And(And { lhs, rhs, .. }) => {
            serialize(lhs, documents, fuzzy_scope, input);
            serialize(rhs, documents, fuzzy_scope, input);
        }

        Node::AndAsgn(AndAsgn { recv, value, .. }) => {
            serialize(recv, documents, fuzzy_scope, input);
            serialize(value, documents, fuzzy_scope, input);
        }

        Node::Arg(Arg { name, expression_l }) => {
            let (lineno, begin_pos) = input.line_col_for_pos(expression_l.begin).unwrap();
            let (_lineno, end_pos) = input.line_col_for_pos(expression_l.end).unwrap();

            documents.push(FuzzyNode {
                category: "assignment",
                fuzzy_ruby_scope: fuzzy_scope.clone(),
                name: name.to_string(),
                node_type: "Arg",
                line: lineno,
                start_column: begin_pos,
                end_column: end_pos,
            });
        }

        Node::Args(Args { args, .. }) => {
            for node in args {
                serialize(node, documents, fuzzy_scope, input);
            }
        }

        Node::Array(Array { elements, .. }) => {
            for node in elements {
                serialize(node, documents, fuzzy_scope, input);
            }
        }

        Node::ArrayPattern(ArrayPattern { elements, .. }) => {
            for node in elements {
                serialize(node, documents, fuzzy_scope, input);
            }
        }

        Node::ArrayPatternWithTail(ArrayPatternWithTail { elements, .. }) => {
            for node in elements {
                serialize(node, documents, fuzzy_scope, input);
            }
        }

        // Node::BackRef(BackRef { .. }) => {}
        Node::Begin(Begin { statements, .. }) => {
            // println!("{:#?}", node);

            for child_node in statements {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        Node::Block(Block {
            call, args, body, ..
        }) => {
            serialize(call, documents, fuzzy_scope, input);

            for child_node in args {
                serialize(child_node, documents, fuzzy_scope, input);
            }

            if let Some(child_node) = body {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        // Node::Blockarg(Blockarg { .. }) => {}
        Node::BlockPass(BlockPass { value, .. }) => {
            if let Some(child_node) = value {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        Node::Break(Break { args, .. }) => {
            for child_node in args {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        Node::Case(Case {
            expr,
            when_bodies,
            else_body,
            ..
        }) => {
            if let Some(child_node) = expr {
                serialize(child_node, documents, fuzzy_scope, input);
            }

            for child_node in when_bodies {
                serialize(child_node, documents, fuzzy_scope, input);
            }

            if let Some(child_node) = else_body {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        Node::CaseMatch(CaseMatch {
            expr,
            in_bodies,
            else_body,
            ..
        }) => {
            serialize(expr, documents, fuzzy_scope, input);

            for child_node in in_bodies {
                serialize(child_node, documents, fuzzy_scope, input);
            }

            if let Some(child_node) = else_body {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        Node::Casgn(Casgn {
            scope,
            name,
            value,
            name_l,
            ..
        }) => {
            // todo: improve fuzzy_scope by using scope

            let (lineno, begin_pos) = input.line_col_for_pos(name_l.begin).unwrap();
            let (_lineno, end_pos) = input.line_col_for_pos(name_l.end).unwrap();

            documents.push(FuzzyNode {
                category: "assignment",
                fuzzy_ruby_scope: fuzzy_scope.clone(),
                name: name.to_string(),
                node_type: "Casgn",
                line: lineno,
                start_column: begin_pos,
                end_column: end_pos,
            });

            if let Some(child_node) = scope {
                serialize(child_node, documents, fuzzy_scope, input);
            }

            if let Some(child_node) = value {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        // Node::Cbase(Cbase { .. }) => {}
        Node::Class(Class {
            name,
            superclass,
            body,
            ..
        }) => {
            if let Node::Const(const_node) = *name.to_owned() {
                let (lineno, begin_pos) = input
                    .line_col_for_pos(const_node.expression_l.begin)
                    .unwrap();
                let (_lineno, end_pos) =
                    input.line_col_for_pos(const_node.expression_l.end).unwrap();
                let class_name = const_node.name.to_string();

                documents.push(FuzzyNode {
                    category: "assignment",
                    fuzzy_ruby_scope: fuzzy_scope.clone(),
                    name: class_name.clone(),
                    node_type: "Class",
                    line: lineno,
                    start_column: begin_pos,
                    end_column: end_pos,
                });

                fuzzy_scope.push(class_name);

                if let Some(superclass_node) = superclass {
                    serialize(superclass_node, documents, fuzzy_scope, input);
                }

                for child_node in body {
                    serialize(child_node, documents, fuzzy_scope, input);
                }

                fuzzy_scope.pop();
            }
        }

        // Node::Complex(Complex { .. }) => {}
        Node::Const(Const {
            scope,
            name,
            name_l,
            ..
        }) => {
            // todo: improve fuzzy_scope by using scope

            let (lineno, begin_pos) = input.line_col_for_pos(name_l.begin).unwrap();
            let (_lineno, end_pos) = input.line_col_for_pos(name_l.end).unwrap();

            documents.push(FuzzyNode {
                category: "usage",
                fuzzy_ruby_scope: fuzzy_scope.clone(),
                name: name.to_string(),
                node_type: "Const",
                line: lineno,
                start_column: begin_pos,
                end_column: end_pos,
            });

            if let Some(child_node) = scope {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        Node::ConstPattern(ConstPattern {
            const_, pattern, ..
        }) => {
            serialize(const_, documents, fuzzy_scope, input);
            serialize(pattern, documents, fuzzy_scope, input);
        }

        Node::CSend(CSend {
            recv,
            method_name,
            args,
            selector_l,
            ..
        }) => {
            if let Some(loc) = selector_l {
                let (lineno, begin_pos) = input.line_col_for_pos(loc.begin).unwrap();
                let (_lineno, end_pos) = input.line_col_for_pos(loc.end).unwrap();

                documents.push(FuzzyNode {
                    category: "usage",
                    fuzzy_ruby_scope: fuzzy_scope.clone(),
                    name: method_name.to_string(),
                    node_type: "CSend",
                    line: lineno,
                    start_column: begin_pos,
                    end_column: end_pos,
                });
            }

            serialize(recv, documents, fuzzy_scope, input);

            for child_node in args {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        Node::Cvar(Cvar { name, expression_l }) => {
            let (lineno, begin_pos) = input.line_col_for_pos(expression_l.begin).unwrap();
            let (_lineno, end_pos) = input.line_col_for_pos(expression_l.end).unwrap();

            documents.push(FuzzyNode {
                category: "usage",
                fuzzy_ruby_scope: fuzzy_scope.clone(),
                name: name.to_string(),
                node_type: "Cvar",
                line: lineno,
                start_column: begin_pos,
                end_column: end_pos,
            });
        }

        Node::Cvasgn(Cvasgn {
            name,
            value,
            name_l,
            ..
        }) => {
            let (lineno, begin_pos) = input.line_col_for_pos(name_l.begin).unwrap();
            let (_lineno, end_pos) = input.line_col_for_pos(name_l.end).unwrap();

            documents.push(FuzzyNode {
                category: "assignment",
                fuzzy_ruby_scope: fuzzy_scope.clone(),
                name: name.to_string(),
                node_type: "Cvasgn",
                line: lineno,
                start_column: begin_pos,
                end_column: end_pos,
            });

            if let Some(child_node) = value {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        Node::Def(Def {
            name,
            args,
            body,
            name_l,
            ..
        }) => {
            let (lineno, begin_pos) = input.line_col_for_pos(name_l.begin).unwrap();
            let (_lineno, end_pos) = input.line_col_for_pos(name_l.end).unwrap();

            documents.push(FuzzyNode {
                category: "assignment",
                fuzzy_ruby_scope: fuzzy_scope.clone(),
                name: name.to_string(),
                node_type: "Def",
                line: lineno,
                start_column: begin_pos,
                end_column: end_pos,
            });

            fuzzy_scope.push(name.to_string());

            if let Some(child_node) = args {
                serialize(child_node, documents, fuzzy_scope, input);
            }

            if let Some(child_node) = body {
                serialize(child_node, documents, fuzzy_scope, input);
            }

            fuzzy_scope.pop();
        }

        Node::Defined(Defined { value, .. }) => {
            serialize(value, documents, fuzzy_scope, input);
        }

        Node::Defs(Defs {
            name,
            args,
            body,
            name_l,
            ..
        }) => {
            let (lineno, begin_pos) = input.line_col_for_pos(name_l.begin).unwrap();
            let (_lineno, end_pos) = input.line_col_for_pos(name_l.end).unwrap();

            documents.push(FuzzyNode {
                category: "assignment",
                fuzzy_ruby_scope: fuzzy_scope.clone(),
                name: name.to_string(),
                node_type: "Defs",
                line: lineno,
                start_column: begin_pos,
                end_column: end_pos,
            });

            let mut scope_name = "self.".to_owned();
            scope_name.push_str(name);

            fuzzy_scope.push(scope_name);

            if let Some(child_node) = args {
                serialize(child_node, documents, fuzzy_scope, input);
            }

            if let Some(child_node) = body {
                serialize(child_node, documents, fuzzy_scope, input);
            }

            fuzzy_scope.pop();
        }

        Node::Dstr(Dstr { parts, .. }) => {
            for child_node in parts {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        Node::Dsym(Dsym { parts, .. }) => {
            for child_node in parts {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        Node::EFlipFlop(EFlipFlop { left, right, .. }) => {
            if let Some(child_node) = left {
                serialize(child_node, documents, fuzzy_scope, input);
            }

            if let Some(child_node) = right {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        // Node::EmptyElse(EmptyElse { .. }) => {}
        // Node::Encoding(Encoding { .. }) => {}
        Node::Ensure(Ensure { body, ensure, .. }) => {
            if let Some(child_node) = body {
                serialize(child_node, documents, fuzzy_scope, input);
            }

            if let Some(child_node) = ensure {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        Node::Erange(Erange { left, right, .. }) => {
            if let Some(child_node) = left {
                serialize(child_node, documents, fuzzy_scope, input);
            }

            if let Some(child_node) = right {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        // Node::False(False { .. }) => {}
        // Node::File(File { .. }) => {}
        Node::FindPattern(FindPattern { elements, .. }) => {
            for child_node in elements {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        // Node::Float(Float { .. }) => {}
        Node::For(For {
            iterator,
            iteratee,
            body,
            ..
        }) => {
            serialize(iterator, documents, fuzzy_scope, input);
            serialize(iteratee, documents, fuzzy_scope, input);

            for child_node in body {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        // Node::ForwardArg(ForwardArg { .. }) => {}
        // Node::ForwardedArgs(ForwardedArgs { .. }) => {}
        Node::Gvar(Gvar { name, expression_l }) => {
            let (lineno, begin_pos) = input.line_col_for_pos(expression_l.begin).unwrap();
            let (_lineno, end_pos) = input.line_col_for_pos(expression_l.end).unwrap();

            documents.push(FuzzyNode {
                category: "usage",
                fuzzy_ruby_scope: fuzzy_scope.clone(),
                name: name.to_string(),
                node_type: "Gvar",
                line: lineno,
                start_column: begin_pos,
                end_column: end_pos,
            });
        }

        Node::Gvasgn(Gvasgn {
            name,
            value,
            name_l,
            ..
        }) => {
            let (lineno, begin_pos) = input.line_col_for_pos(name_l.begin).unwrap();
            let (_lineno, end_pos) = input.line_col_for_pos(name_l.end).unwrap();

            documents.push(FuzzyNode {
                category: "assignment",
                fuzzy_ruby_scope: fuzzy_scope.clone(),
                name: name.to_string(),
                node_type: "Gvasgn",
                line: lineno,
                start_column: begin_pos,
                end_column: end_pos,
            });

            if let Some(child_node) = value {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        Node::Hash(Hash { pairs, .. }) => {
            for child_node in pairs {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        Node::HashPattern(HashPattern { elements, .. }) => {
            for child_node in elements {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        Node::Heredoc(Heredoc { parts, .. }) => {
            for child_node in parts {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        Node::If(If {
            cond,
            if_true,
            if_false,
            ..
        }) => {
            serialize(cond, documents, fuzzy_scope, input);

            if let Some(child_node) = if_true {
                serialize(child_node, documents, fuzzy_scope, input);
            }

            if let Some(child_node) = if_false {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        Node::IfGuard(IfGuard { cond, .. }) => {
            serialize(cond, documents, fuzzy_scope, input);
        }

        Node::IFlipFlop(IFlipFlop { left, right, .. }) => {
            if let Some(child_node) = left {
                serialize(child_node, documents, fuzzy_scope, input);
            }

            if let Some(child_node) = right {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        Node::IfMod(IfMod {
            cond,
            if_true,
            if_false,
            ..
        }) => {
            serialize(cond, documents, fuzzy_scope, input);

            if let Some(child_node) = if_true {
                serialize(child_node, documents, fuzzy_scope, input);
            }

            if let Some(child_node) = if_false {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        Node::IfTernary(IfTernary {
            cond,
            if_true,
            if_false,
            ..
        }) => {
            serialize(cond, documents, fuzzy_scope, input);
            serialize(if_true, documents, fuzzy_scope, input);
            serialize(if_false, documents, fuzzy_scope, input);
        }

        Node::Index(lib_ruby_parser::nodes::Index { recv, indexes, .. }) => {
            serialize(recv, documents, fuzzy_scope, input);

            for child_node in indexes {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        Node::IndexAsgn(IndexAsgn {
            recv,
            indexes,
            value,
            ..
        }) => {
            serialize(recv, documents, fuzzy_scope, input);

            for child_node in indexes {
                serialize(child_node, documents, fuzzy_scope, input);
            }

            if let Some(child_node) = value {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        Node::InPattern(InPattern {
            pattern,
            guard,
            body,
            ..
        }) => {
            serialize(pattern, documents, fuzzy_scope, input);

            if let Some(child_node) = guard {
                serialize(child_node, documents, fuzzy_scope, input);
            }

            if let Some(child_node) = body {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        // Node::Int(Int { .. }) => {}
        Node::Irange(Irange { left, right, .. }) => {
            if let Some(child_node) = left {
                serialize(child_node, documents, fuzzy_scope, input);
            }

            if let Some(child_node) = right {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        Node::Ivar(Ivar { name, expression_l }) => {
            let (lineno, begin_pos) = input.line_col_for_pos(expression_l.begin).unwrap();
            let (_lineno, end_pos) = input.line_col_for_pos(expression_l.end).unwrap();

            documents.push(FuzzyNode {
                category: "usage",
                fuzzy_ruby_scope: fuzzy_scope.clone(),
                name: name.to_string(),
                node_type: "Ivar",
                line: lineno,
                start_column: begin_pos,
                end_column: end_pos,
            });
        }

        Node::Ivasgn(Ivasgn {
            name,
            value,
            name_l,
            ..
        }) => {
            let (lineno, begin_pos) = input.line_col_for_pos(name_l.begin).unwrap();
            let (_lineno, end_pos) = input.line_col_for_pos(name_l.end).unwrap();

            documents.push(FuzzyNode {
                category: "assignment",
                fuzzy_ruby_scope: fuzzy_scope.clone(),
                name: name.to_string(),
                node_type: "Ivasgn",
                line: lineno,
                start_column: begin_pos,
                end_column: end_pos,
            });

            if let Some(child_node) = value {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        Node::Kwarg(Kwarg { name, name_l, .. }) => {
            let (lineno, begin_pos) = input.line_col_for_pos(name_l.begin).unwrap();
            let (_lineno, end_pos) = input.line_col_for_pos(name_l.end).unwrap();

            documents.push(FuzzyNode {
                category: "assignment",
                fuzzy_ruby_scope: fuzzy_scope.clone(),
                name: name.to_string(),
                node_type: "Kwarg",
                line: lineno,
                start_column: begin_pos,
                end_column: end_pos,
            });
        }

        Node::Kwargs(Kwargs { pairs, .. }) => {
            for node in pairs {
                serialize(node, documents, fuzzy_scope, input);
            }
        }

        Node::KwBegin(KwBegin { statements, .. }) => {
            for node in statements {
                serialize(node, documents, fuzzy_scope, input);
            }
        }

        // Node::Kwnilarg(Kwnilarg { .. }) => {}
        Node::Kwoptarg(Kwoptarg {
            name,
            default,
            name_l,
            ..
        }) => {
            let (lineno, begin_pos) = input.line_col_for_pos(name_l.begin).unwrap();
            let (_lineno, end_pos) = input.line_col_for_pos(name_l.end).unwrap();

            documents.push(FuzzyNode {
                category: "assignment",
                fuzzy_ruby_scope: fuzzy_scope.clone(),
                name: name.to_string(),
                node_type: "Kwoptarg",
                line: lineno,
                start_column: begin_pos,
                end_column: end_pos,
            });

            serialize(default, documents, fuzzy_scope, input);
        }

        Node::Kwrestarg(Kwrestarg { name, name_l, .. }) => {
            if let Some(node_name) = name {
                if let Some(loc) = name_l {
                    let (lineno, begin_pos) = input.line_col_for_pos(loc.begin).unwrap();
                    let (_lineno, end_pos) = input.line_col_for_pos(loc.end).unwrap();

                    documents.push(FuzzyNode {
                        category: "assignment",
                        fuzzy_ruby_scope: fuzzy_scope.clone(),
                        name: node_name.to_string(),
                        node_type: "Kwrestarg",
                        line: lineno,
                        start_column: begin_pos,
                        end_column: end_pos,
                    });
                }
            }
        }

        Node::Kwsplat(Kwsplat { value, .. }) => {
            serialize(value, documents, fuzzy_scope, input);
        }

        // Node::Lambda(Lambda { .. }) => {}
        // Node::Line(Line { .. }) => {}
        Node::Lvar(Lvar { name, expression_l }) => {
            let (lineno, begin_pos) = input.line_col_for_pos(expression_l.begin).unwrap();
            let (_lineno, end_pos) = input.line_col_for_pos(expression_l.end).unwrap();

            documents.push(FuzzyNode {
                category: "usage",
                fuzzy_ruby_scope: fuzzy_scope.clone(),
                name: name.to_string(),
                node_type: "Lvar",
                line: lineno,
                start_column: begin_pos,
                end_column: end_pos,
            });
        }

        Node::Lvasgn(Lvasgn {
            name,
            value,
            name_l,
            ..
        }) => {
            let (lineno, begin_pos) = input.line_col_for_pos(name_l.begin).unwrap();
            let (_lineno, end_pos) = input.line_col_for_pos(name_l.end).unwrap();

            documents.push(FuzzyNode {
                category: "assignment",
                fuzzy_ruby_scope: fuzzy_scope.clone(),
                name: name.to_string(),
                node_type: "Lvasgn",
                line: lineno,
                start_column: begin_pos,
                end_column: end_pos,
            });

            if let Some(child_node) = value {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        Node::Masgn(Masgn { lhs, rhs, .. }) => {
            serialize(lhs, documents, fuzzy_scope, input);
            serialize(rhs, documents, fuzzy_scope, input);
        }

        Node::MatchAlt(MatchAlt { lhs, rhs, .. }) => {
            serialize(lhs, documents, fuzzy_scope, input);
            serialize(rhs, documents, fuzzy_scope, input);
        }

        Node::MatchAs(MatchAs { value, as_, .. }) => {
            serialize(value, documents, fuzzy_scope, input);
            serialize(as_, documents, fuzzy_scope, input);
        }

        Node::MatchCurrentLine(MatchCurrentLine { re, .. }) => {
            serialize(re, documents, fuzzy_scope, input);
        }

        // Node::MatchNilPattern(MatchNilPattern { .. }) => {}
        Node::MatchPattern(MatchPattern { value, pattern, .. }) => {
            serialize(value, documents, fuzzy_scope, input);
            serialize(pattern, documents, fuzzy_scope, input);
        }

        Node::MatchPatternP(MatchPatternP { value, pattern, .. }) => {
            serialize(value, documents, fuzzy_scope, input);
            serialize(pattern, documents, fuzzy_scope, input);
        }

        Node::MatchRest(MatchRest { name, .. }) => {
            if let Some(child_node) = name {
                serialize(child_node, documents, fuzzy_scope, input);
            }
        }

        Node::MatchVar(MatchVar { name, name_l, .. }) => {
            let (lineno, begin_pos) = input.line_col_for_pos(name_l.begin).unwrap();
            let (_lineno, end_pos) = input.line_col_for_pos(name_l.end).unwrap();

            documents.push(FuzzyNode {
                category: "assignment",
                fuzzy_ruby_scope: fuzzy_scope.clone(),
                name: name.to_string(),
                node_type: "MatchVar",
                line: lineno,
                start_column: begin_pos,
                end_column: end_pos,
            });
        }

        Node::MatchWithLvasgn(MatchWithLvasgn { re, value, .. }) => {
            serialize(re, documents, fuzzy_scope, input);
            serialize(value, documents, fuzzy_scope, input);
        }

        Node::Mlhs(Mlhs { items, .. }) => {
            for node in items {
                serialize(node, documents, fuzzy_scope, input);
            }
        }

        Node::Module(Module { name, body, .. }) => {
            if let Node::Const(const_node) = *name.to_owned() {
                let (lineno, begin_pos) = input
                    .line_col_for_pos(const_node.expression_l.begin)
                    .unwrap();
                let (_lineno, end_pos) =
                    input.line_col_for_pos(const_node.expression_l.end).unwrap();
                let class_name = const_node.name.to_string();

                documents.push(FuzzyNode {
                    category: "assignment",
                    fuzzy_ruby_scope: fuzzy_scope.clone(),
                    name: class_name.clone(),
                    node_type: "Module",
                    line: lineno,
                    start_column: begin_pos,
                    end_column: end_pos,
                });

                fuzzy_scope.push(class_name);

                for child_node in body {
                    serialize(child_node, documents, fuzzy_scope, input);
                }

                fuzzy_scope.pop();
            }
        }

        Node::Next(Next { args, .. }) => {
            for node in args {
                serialize(node, documents, fuzzy_scope, input);
            }
        }

        // Node::Nil(Nil { .. }) => {}
        // Node::NthRef(NthRef { .. }) => {}
        Node::Numblock(Numblock { call, body, .. }) => {
            serialize(call, documents, fuzzy_scope, input);
            serialize(body, documents, fuzzy_scope, input);
        }

        Node::OpAsgn(OpAsgn { recv, value, .. }) => {
            serialize(recv, documents, fuzzy_scope, input);
            serialize(value, documents, fuzzy_scope, input);
        }

        Node::Optarg(Optarg {
            name,
            default,
            name_l,
            ..
        }) => {
            let (lineno, begin_pos) = input.line_col_for_pos(name_l.begin).unwrap();
            let (_lineno, end_pos) = input.line_col_for_pos(name_l.end).unwrap();

            documents.push(FuzzyNode {
                category: "assignment",
                fuzzy_ruby_scope: fuzzy_scope.clone(),
                name: name.to_string(),
                node_type: "Optarg",
                line: lineno,
                start_column: begin_pos,
                end_column: end_pos,
            });

            serialize(default, documents, fuzzy_scope, input);
        }

        Node::Or(Or { lhs, rhs, .. }) => {
            serialize(lhs, documents, fuzzy_scope, input);
            serialize(rhs, documents, fuzzy_scope, input);
        }

        Node::OrAsgn(OrAsgn { recv, value, .. }) => {
            serialize(recv, documents, fuzzy_scope, input);
            serialize(value, documents, fuzzy_scope, input);
        }

        Node::Pair(Pair { key, value, .. }) => {
            serialize(key, documents, fuzzy_scope, input);
            serialize(value, documents, fuzzy_scope, input);
        }

        Node::Pin(Pin { var, .. }) => {
            serialize(var, documents, fuzzy_scope, input);
        }

        Node::Postexe(Postexe { body, .. }) => {
            for node in body {
                serialize(node, documents, fuzzy_scope, input);
            }
        }

        Node::Preexe(Preexe { body, .. }) => {
            for node in body {
                serialize(node, documents, fuzzy_scope, input);
            }
        }

        Node::Procarg0(Procarg0 { args, .. }) => {
            for node in args {
                serialize(node, documents, fuzzy_scope, input);
            }
        }

        // Node::Rational(Rational { .. }) => {}
        // Node::Redo(Redo { .. }) => {}
        Node::Regexp(Regexp { parts, options, .. }) => {
            for node in parts {
                serialize(node, documents, fuzzy_scope, input);
            }

            for node in options {
                serialize(node, documents, fuzzy_scope, input);
            }
        }

        // Node::RegOpt(RegOpt { .. }) => {}
        Node::Rescue(Rescue {
            body,
            rescue_bodies,
            ..
        }) => {
            for node in body {
                serialize(node, documents, fuzzy_scope, input);
            }

            for node in rescue_bodies {
                serialize(node, documents, fuzzy_scope, input);
            }
        }

        Node::RescueBody(RescueBody {
            exc_list,
            exc_var,
            body,
            ..
        }) => {
            for node in exc_list {
                serialize(node, documents, fuzzy_scope, input);
            }

            for node in exc_var {
                serialize(node, documents, fuzzy_scope, input);
            }

            for node in body {
                serialize(node, documents, fuzzy_scope, input);
            }
        }

        Node::Restarg(Restarg { name, name_l, .. }) => {
            if let Some(name_str) = name {
                if let Some(loc) = name_l {
                    let (lineno, begin_pos) = input.line_col_for_pos(loc.begin).unwrap();
                    let (_lineno, end_pos) = input.line_col_for_pos(loc.end).unwrap();

                    documents.push(FuzzyNode {
                        category: "assignment",
                        fuzzy_ruby_scope: fuzzy_scope.clone(),
                        name: name_str.to_string(),
                        node_type: "Restarg",
                        line: lineno,
                        start_column: begin_pos,
                        end_column: end_pos,
                    });
                }
            }
        }

        // Node::Retry(Retry { .. }) => {}
        Node::Return(Return { args, .. }) => {
            for node in args {
                serialize(node, documents, fuzzy_scope, input);
            }
        }

        Node::SClass(SClass { expr, body, .. }) => {
            serialize(expr, documents, fuzzy_scope, input);

            for node in body {
                serialize(node, documents, fuzzy_scope, input);
            }
        }

        // Node::Self_(Self_ { .. }) => {}
        Node::Send(Send {
            recv,
            method_name,
            args,
            selector_l,
            ..
        }) => {
            if let Some(node) = recv {
                serialize(node, documents, fuzzy_scope, input);
            }

            if let Some(loc) = selector_l {
                let (lineno, begin_pos) = input.line_col_for_pos(loc.begin).unwrap();
                let (_lineno, end_pos) = input.line_col_for_pos(loc.end).unwrap();

                documents.push(FuzzyNode {
                    category: "usage",
                    fuzzy_ruby_scope: fuzzy_scope.clone(),
                    name: method_name.to_string(),
                    node_type: "Send",
                    line: lineno,
                    start_column: begin_pos,
                    end_column: end_pos,
                });
            }

            for node in args {
                serialize(node, documents, fuzzy_scope, input);
            }
        }

        Node::Shadowarg(Shadowarg { name, expression_l }) => {
            let (lineno, begin_pos) = input.line_col_for_pos(expression_l.begin).unwrap();
            let (_lineno, end_pos) = input.line_col_for_pos(expression_l.end).unwrap();

            documents.push(FuzzyNode {
                category: "assignment",
                fuzzy_ruby_scope: fuzzy_scope.clone(),
                name: name.to_string(),
                node_type: "Shadowarg",
                line: lineno,
                start_column: begin_pos,
                end_column: end_pos,
            });
        }

        Node::Splat(Splat { value, .. }) => {
            for node in value {
                serialize(node, documents, fuzzy_scope, input);
            }
        }

        // Node::Str(Str { .. }) => {}
        Node::Super(Super {
            args, keyword_l, ..
        }) => {
            if let Some(last_scope_name) = fuzzy_scope.last() {
                let (lineno, begin_pos) = input.line_col_for_pos(keyword_l.begin).unwrap();
                let (_lineno, end_pos) = input.line_col_for_pos(keyword_l.end).unwrap();

                documents.push(FuzzyNode {
                    category: "usage",
                    fuzzy_ruby_scope: fuzzy_scope.clone(),
                    name: last_scope_name.to_string(),
                    node_type: "Super",
                    line: lineno,
                    start_column: begin_pos,
                    end_column: end_pos,
                });
            }

            for node in args {
                serialize(node, documents, fuzzy_scope, input);
            }
        }

        // Node::Sym(Sym { .. }) => {}
        // Node::True(True { .. }) => {}
        Node::Undef(Undef { names, .. }) => {
            for node in names {
                serialize(node, documents, fuzzy_scope, input);
            }
        }

        Node::UnlessGuard(UnlessGuard { cond, .. }) => {
            serialize(cond, documents, fuzzy_scope, input);
        }

        Node::Until(Until { cond, body, .. }) => {
            serialize(cond, documents, fuzzy_scope, input);

            for node in body {
                serialize(node, documents, fuzzy_scope, input);
            }
        }

        Node::UntilPost(UntilPost { cond, body, .. }) => {
            serialize(cond, documents, fuzzy_scope, input);
            serialize(body, documents, fuzzy_scope, input);
        }

        Node::When(When { patterns, body, .. }) => {
            for node in patterns {
                serialize(node, documents, fuzzy_scope, input);
            }

            for node in body {
                serialize(node, documents, fuzzy_scope, input);
            }
        }

        Node::While(While { cond, body, .. }) => {
            serialize(cond, documents, fuzzy_scope, input);

            for node in body {
                serialize(node, documents, fuzzy_scope, input);
            }
        }

        Node::WhilePost(WhilePost { cond, body, .. }) => {
            serialize(cond, documents, fuzzy_scope, input);
            serialize(body, documents, fuzzy_scope, input);
        }

        Node::XHeredoc(XHeredoc { parts, .. }) => {
            for node in parts {
                serialize(node, documents, fuzzy_scope, input);
            }
        }

        Node::Xstr(Xstr { parts, .. }) => {
            for node in parts {
                serialize(node, documents, fuzzy_scope, input);
            }
        }

        Node::Yield(Yield { args, .. }) => {
            for node in args {
                serialize(node, documents, fuzzy_scope, input);
            }
        }

        Node::ZSuper(ZSuper { expression_l, .. }) => {
            if let Some(last_scope_name) = fuzzy_scope.last() {
                let (lineno, begin_pos) = input.line_col_for_pos(expression_l.begin).unwrap();
                let (_lineno, end_pos) = input.line_col_for_pos(expression_l.end).unwrap();

                documents.push(FuzzyNode {
                    category: "usage",
                    fuzzy_ruby_scope: fuzzy_scope.clone(),
                    name: last_scope_name.to_string(),
                    node_type: "ZSuper",
                    line: lineno,
                    start_column: begin_pos,
                    end_column: end_pos,
                });
            }
        }

        _ => {}
    };
}
