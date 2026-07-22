use std::sync::Arc;

use serde_json::Value;
use zen_types::decision::{DecisionNodeKind, FunctionNodeContent};

use crate::model::DecisionContent;
use crate::policy::raw::BlockDoc;
use crate::workspace::db::Db;
use crate::workspace::types::{SearchHit, SearchHitKind, Span};

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 500;
const SNIPPET_CHARS: usize = 200;

pub(crate) struct SearchMatcher {
    tokens: Vec<Vec<char>>,
}

impl SearchMatcher {
    pub(crate) fn new(query: &str) -> Option<Self> {
        let tokens: Vec<Vec<char>> = query
            .split_whitespace()
            .map(|t| t.chars().map(lower).collect::<Vec<char>>())
            .filter(|t| !t.is_empty())
            .collect();
        (!tokens.is_empty()).then_some(Self { tokens })
    }

    fn score(&self, text: &str) -> Option<(u32, Span)> {
        if text.is_empty() {
            return None;
        }
        let hay: Vec<char> = text.chars().map(lower).collect();
        let mut total = 0u32;
        let mut span: Option<Span> = None;
        for token in &self.tokens {
            let (score, token_span) = score_token(&hay, token)?;
            total += score;
            span = Some(match span {
                Some(current) if current.0 <= token_span.0 => current,
                _ => token_span,
            });
        }
        let length_penalty = (hay.len() as u32 / 8).min(40);
        Some((total.saturating_sub(length_penalty), span?))
    }
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

fn score_token(hay: &[char], token: &[char]) -> Option<(u32, Span)> {
    if let Some(idx) = find_substring(hay, token) {
        let span = (idx as u32, (idx + token.len()) as u32);
        let score = if idx == 0 && token.len() == hay.len() {
            1000
        } else if idx == 0 {
            800
        } else if !hay[idx - 1].is_alphanumeric() {
            650u32.saturating_sub(idx.min(150) as u32)
        } else {
            500u32.saturating_sub(idx.min(400) as u32)
        };
        return Some((score, span));
    }

    let mut first: Option<usize> = None;
    let mut last = 0usize;
    let mut ti = 0usize;
    for (i, &c) in hay.iter().enumerate() {
        if ti < token.len() && c == token[ti] {
            first.get_or_insert(i);
            last = i;
            ti += 1;
            if ti == token.len() {
                break;
            }
        }
    }
    if ti < token.len() {
        return None;
    }
    let first = first?;
    let spread = (last - first + 1) as u32;
    if spread > token.len() as u32 * 3 {
        return None;
    }
    let compactness = ((token.len() as u32 * 100) / spread).min(100);
    Some((100 + compactness, (first as u32, last as u32 + 1)))
}

fn find_substring(hay: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == *needle)
}

fn snippet(text: &str, span: Span) -> (String, Span) {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= SNIPPET_CHARS {
        return (text.to_string(), span);
    }
    let mid = ((span.0 + span.1) / 2) as usize;
    let start = mid
        .saturating_sub(SNIPPET_CHARS / 2)
        .min(chars.len() - SNIPPET_CHARS);
    let end = (start + SNIPPET_CHARS).min(chars.len());
    let clipped = (
        (span.0 as usize).saturating_sub(start) as u32,
        ((span.1 as usize).saturating_sub(start)).min(end - start) as u32,
    );
    (chars[start..end].iter().collect(), clipped)
}

#[derive(Default, Clone)]
struct HitSite {
    block_id: Option<Arc<str>>,
    node_id: Option<Arc<str>>,
    expression_id: Option<Arc<str>>,
    row: Option<u32>,
    column: Option<Arc<str>>,
    context: Option<String>,
}

struct Collector<'a> {
    matcher: &'a SearchMatcher,
    path: Arc<str>,
    hits: Vec<SearchHit>,
}

impl Collector<'_> {
    fn add(&mut self, kind: SearchHitKind, text: &str, site: HitSite) {
        let Some((base, span)) = self.matcher.score(text) else {
            return;
        };
        let (text, span) = snippet(text, span);
        self.hits.push(SearchHit {
            path: self.path.clone(),
            block_id: site.block_id,
            node_id: site.node_id,
            expression_id: site.expression_id,
            row: site.row,
            column: site.column,
            kind,
            text,
            context: site.context,
            span,
            score: base * kind.weight() / 100,
        });
    }

    fn add_best(&mut self, kind: SearchHitKind, candidates: &[&str], site: HitSite) {
        let best = candidates
            .iter()
            .filter_map(|text| self.matcher.score(text).map(|(score, _)| (score, *text)))
            .max_by_key(|(score, _)| *score);
        if let Some((_, text)) = best {
            self.add(kind, text, site);
        }
    }
}

impl Db {
    pub fn search(&self, query: &str, limit: Option<u32>) -> Vec<SearchHit> {
        let Some(matcher) = SearchMatcher::new(query) else {
            return Vec::new();
        };
        let limit = limit.map_or(DEFAULT_LIMIT, |l| l as usize).min(MAX_LIMIT);

        let mut paths = self.document_paths();
        paths.sort();

        let mut hits: Vec<SearchHit> = Vec::new();
        for path in paths {
            let Some(document) = self.raw_document(&path) else {
                continue;
            };
            let mut collector = Collector {
                matcher: &matcher,
                path: path.clone(),
                hits: Vec::new(),
            };
            let file_name = path.rsplit('/').next().unwrap_or(&path);
            collector.add(SearchHitKind::Document, file_name, HitSite::default());
            match document.as_ref() {
                DecisionContent::Policy(policy) => {
                    for block in &policy.0.blocks {
                        search_policy_block(&mut collector, block);
                    }
                }
                DecisionContent::Graph(graph) => {
                    for node in &graph.nodes {
                        search_graph_node(&mut collector, node);
                    }
                }
            }
            hits.append(&mut collector.hits);
        }

        hits.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.path.cmp(&b.path))
                .then_with(|| a.block_id.cmp(&b.block_id))
                .then_with(|| a.node_id.cmp(&b.node_id))
                .then_with(|| a.row.cmp(&b.row))
                .then_with(|| a.span.0.cmp(&b.span.0))
        });
        hits.truncate(limit);
        hits
    }
}

fn block_site(block_id: &Arc<str>) -> HitSite {
    HitSite {
        block_id: Some(block_id.clone()),
        ..HitSite::default()
    }
}

fn search_policy_block(collector: &mut Collector, block: &BlockDoc) {
    match block {
        BlockDoc::Assertion { id, data } => {
            collector.add(SearchHitKind::AssertionOutput, &data.output, block_site(id));
            for condition in &data.conditions {
                collector.add(
                    SearchHitKind::AssertionCondition,
                    &condition.expression,
                    HitSite {
                        expression_id: Some(condition.id.clone()),
                        ..block_site(id)
                    },
                );
            }
        }
        BlockDoc::Expression { id, data } => {
            collector.add(SearchHitKind::ExpressionKey, &data.key, block_site(id));
            collector.add(SearchHitKind::Expression, &data.value, block_site(id));
        }
        BlockDoc::Match { id, data } => {
            collector.add(SearchHitKind::MatchKey, &data.key, block_site(id));
            for arm in &data.arms {
                let site = HitSite {
                    expression_id: Some(arm.id.clone()),
                    ..block_site(id)
                };
                collector.add(SearchHitKind::MatchCondition, &arm.condition, site.clone());
                collector.add(SearchHitKind::MatchValue, &arm.value, site);
            }
        }
        BlockDoc::DecisionTable { id, data } => {
            let mut column_names: Vec<(Arc<str>, String)> = Vec::new();
            for input in &data.inputs {
                let field = input.field.as_deref().unwrap_or("");
                collector.add_best(
                    SearchHitKind::TableColumn,
                    &[&input.name, field],
                    HitSite {
                        column: Some(input.id.clone()),
                        ..block_site(id)
                    },
                );
                column_names.push((input.id.clone(), display_name(&input.name, field)));
            }
            for output in &data.outputs {
                collector.add_best(
                    SearchHitKind::TableColumn,
                    &[&output.name, &output.field],
                    HitSite {
                        column: Some(output.id.clone()),
                        ..block_site(id)
                    },
                );
                column_names.push((output.id.clone(), display_name(&output.name, &output.field)));
            }
            for (row_idx, rule) in data.rules.iter().enumerate() {
                for (column_id, cell) in rule {
                    if column_id.starts_with('_') {
                        continue;
                    }
                    let context = column_names
                        .iter()
                        .find(|(id, _)| id == column_id)
                        .map(|(_, name)| name.clone());
                    collector.add(
                        SearchHitKind::TableCell,
                        cell,
                        HitSite {
                            row: Some(row_idx as u32),
                            column: Some(column_id.clone()),
                            context,
                            ..block_site(id)
                        },
                    );
                }
            }
        }
        BlockDoc::DataModel { id, data } => {
            collector.add(SearchHitKind::DataModel, &data.name, block_site(id));
            for property in &data.properties {
                collector.add(
                    SearchHitKind::DataModelProperty,
                    &property.name,
                    HitSite {
                        expression_id: Some(property.id.clone()),
                        context: Some(data.name.to_string()),
                        ..block_site(id)
                    },
                );
            }
        }
        BlockDoc::Dictionary { id, data } => {
            collector.add(SearchHitKind::Dictionary, &data.name, block_site(id));
            for entry in &data.entries {
                collector.add_best(
                    SearchHitKind::DictionaryEntry,
                    &[&entry.label, &entry.value],
                    HitSite {
                        expression_id: Some(entry.id.clone()),
                        context: Some(data.name.to_string()),
                        ..block_site(id)
                    },
                );
            }
        }
        BlockDoc::Ignored(value) => search_ignored_block(collector, value),
    }
}

fn search_ignored_block(collector: &mut Collector, value: &Value) {
    let site = HitSite {
        block_id: value
            .get("id")
            .and_then(Value::as_str)
            .map(Arc::<str>::from),
        ..HitSite::default()
    };
    let kind = match value.get("type").and_then(Value::as_str) {
        Some("heading") => SearchHitKind::Heading,
        Some("paragraph") => SearchHitKind::Paragraph,
        Some("bulletListItem") | Some("numberedListItem") => SearchHitKind::ListItem,
        Some("codeBlock") => {
            if let Some(code) = value.pointer("/props/data/code").and_then(Value::as_str) {
                collector.add(SearchHitKind::CodeBlock, code, site);
            }
            return;
        }
        _ => return,
    };
    if let Some(text) = prose_text(value) {
        collector.add(kind, &text, site);
    }
}

fn prose_text(value: &Value) -> Option<String> {
    let runs = value.get("content")?.as_array()?;
    let mut out = String::new();
    for run in runs {
        match run.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = run.get("text").and_then(Value::as_str) {
                    out.push_str(text);
                }
            }
            Some("link") => {
                for inner in run
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(text) = inner.get("text").and_then(Value::as_str) {
                        out.push_str(text);
                    }
                }
            }
            Some("fileRef") => {
                if let Some(label) = run.pointer("/props/label").and_then(Value::as_str) {
                    out.push_str(label);
                }
            }
            _ => {}
        }
    }
    (!out.trim().is_empty()).then_some(out)
}

fn display_name(name: &str, field: &str) -> String {
    if !name.is_empty() {
        name.to_string()
    } else {
        field.to_string()
    }
}

fn search_graph_node(collector: &mut Collector, node: &zen_types::decision::DecisionNode) {
    let node_site = HitSite {
        node_id: Some(node.id.clone()),
        ..HitSite::default()
    };
    collector.add(SearchHitKind::GraphNode, &node.name, node_site.clone());
    let in_node = |site: HitSite| HitSite {
        node_id: Some(node.id.clone()),
        context: Some(node.name.to_string()),
        ..site
    };
    match &node.kind {
        DecisionNodeKind::ExpressionNode { content } => {
            for expression in content.expressions.iter() {
                let site = in_node(HitSite {
                    expression_id: Some(expression.id.clone()),
                    ..HitSite::default()
                });
                collector.add(SearchHitKind::ExpressionKey, &expression.key, site.clone());
                collector.add(SearchHitKind::Expression, &expression.value, site);
            }
        }
        DecisionNodeKind::DecisionTableNode { content } => {
            let mut column_names: Vec<(Arc<str>, String)> = Vec::new();
            for input in content.inputs.iter() {
                let field = input.field.as_deref().unwrap_or("");
                collector.add_best(
                    SearchHitKind::TableColumn,
                    &[&input.name, field],
                    in_node(HitSite {
                        column: Some(input.id.clone()),
                        ..HitSite::default()
                    }),
                );
                column_names.push((input.id.clone(), display_name(&input.name, field)));
            }
            for output in content.outputs.iter() {
                collector.add_best(
                    SearchHitKind::TableColumn,
                    &[&output.name, &output.field],
                    in_node(HitSite {
                        column: Some(output.id.clone()),
                        ..HitSite::default()
                    }),
                );
                column_names.push((output.id.clone(), display_name(&output.name, &output.field)));
            }
            for (row_idx, rule) in content.rules.iter().enumerate() {
                for (column_id, cell) in rule {
                    if column_id.starts_with('_') {
                        continue;
                    }
                    let context = column_names
                        .iter()
                        .find(|(id, _)| id == column_id)
                        .map(|(_, name)| name.clone());
                    collector.add(
                        SearchHitKind::TableCell,
                        cell,
                        HitSite {
                            node_id: Some(node.id.clone()),
                            row: Some(row_idx as u32),
                            column: Some(column_id.clone()),
                            context,
                            ..HitSite::default()
                        },
                    );
                }
            }
        }
        DecisionNodeKind::SwitchNode { content } => {
            for statement in content.statements.iter() {
                collector.add(
                    SearchHitKind::SwitchCondition,
                    &statement.condition,
                    in_node(HitSite {
                        expression_id: Some(statement.id.clone()),
                        ..HitSite::default()
                    }),
                );
            }
        }
        DecisionNodeKind::FunctionNode { content } => {
            let source = match content {
                FunctionNodeContent::Version2(function) => &function.source,
                FunctionNodeContent::Version1(source) => source,
            };
            collector.add(SearchHitKind::Function, source, in_node(HitSite::default()));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;

    fn workspace_with_fixtures() -> Workspace {
        let mut ws = Workspace::new();
        ws.set_policy(
            "pricing.policy",
            serde_json::from_value(serde_json::json!({
                "blocks": [
                    {
                        "type": "paragraph",
                        "id": "b-intro",
                        "props": {},
                        "content": [{"type": "text", "text": "Discounts apply to loyal customers only.", "styles": {}}]
                    },
                    {
                        "type": "decisionTable",
                        "id": "b-table",
                        "props": {"data": {
                            "hitPolicy": "first",
                            "inputs": [{"id": "c-in", "name": "Customer Tier", "field": "customer.tier"}],
                            "outputs": [{"id": "c-out", "name": "Discount Amount", "field": "order.discountAmount"}],
                            "rules": [
                                {"_id": "r1", "c-in": "'gold'", "c-out": "42"},
                                {"_id": "r2", "c-in": "'silver'", "c-out": "10"}
                            ]
                        }}
                    },
                    {
                        "type": "dictionary",
                        "id": "b-dict",
                        "props": {"data": {
                            "name": "Tiers",
                            "entries": [{"id": "e1", "value": "gold", "label": "Gold Tier"}]
                        }}
                    },
                    {
                        "type": "expression",
                        "id": "b-expr",
                        "props": {"data": {"key": "order.total", "value": "order.discountAmount * 2"}}
                    }
                ]
            }))
            .unwrap(),
        );
        ws.set_document(
            "flow.graph",
            serde_json::from_value(serde_json::json!({
                "nodes": [
                    {"id": "n1", "name": "Discount Router", "type": "switchNode", "content": {
                        "statements": [{"id": "s1", "condition": "customer.tier == 'gold'"}]
                    }}
                ],
                "edges": []
            }))
            .unwrap(),
        );
        ws
    }

    #[test]
    fn finds_table_column_above_cells() {
        let ws = workspace_with_fixtures();
        let hits = ws.search("discount amount", None);
        assert!(!hits.is_empty());
        let first = &hits[0];
        assert_eq!(first.kind, SearchHitKind::TableColumn);
        assert_eq!(first.path.as_ref(), "pricing.policy");
        assert_eq!(first.column.as_deref(), Some("c-out"));
    }

    #[test]
    fn finds_paragraph_prose() {
        let ws = workspace_with_fixtures();
        let hits = ws.search("loyal customers", None);
        let prose = hits
            .iter()
            .find(|h| h.kind == SearchHitKind::Paragraph)
            .expect("paragraph hit");
        assert_eq!(prose.block_id.as_deref(), Some("b-intro"));
        let (start, end) = prose.span;
        let matched: String = prose
            .text
            .chars()
            .skip(start as usize)
            .take((end - start) as usize)
            .collect();
        assert_eq!(matched.to_lowercase(), "loyal");
    }

    #[test]
    fn finds_cells_with_row_and_column() {
        let ws = workspace_with_fixtures();
        let hits = ws.search("silver", None);
        let cell = hits
            .iter()
            .find(|h| h.kind == SearchHitKind::TableCell)
            .expect("cell hit");
        assert_eq!(cell.row, Some(1));
        assert_eq!(cell.column.as_deref(), Some("c-in"));
        assert_eq!(cell.context.as_deref(), Some("Customer Tier"));
    }

    #[test]
    fn finds_graph_nodes_and_switch_conditions() {
        let ws = workspace_with_fixtures();
        let hits = ws.search("router", None);
        let node = hits
            .iter()
            .find(|h| h.kind == SearchHitKind::GraphNode)
            .expect("graph node hit");
        assert_eq!(node.node_id.as_deref(), Some("n1"));

        let hits = ws.search("tier == 'gold'", None);
        assert!(hits
            .iter()
            .any(|h| h.kind == SearchHitKind::SwitchCondition));
    }

    #[test]
    fn dictionary_entry_matches_label() {
        let ws = workspace_with_fixtures();
        let hits = ws.search("gold tier", None);
        let entry = hits
            .iter()
            .find(|h| h.kind == SearchHitKind::DictionaryEntry)
            .expect("dictionary entry hit");
        assert_eq!(entry.context.as_deref(), Some("Tiers"));
    }

    #[test]
    fn scattered_subsequence_is_rejected() {
        let ws = workspace_with_fixtures();
        let hits = ws.search("dplc", None);
        assert!(!hits.iter().any(|h| h.kind == SearchHitKind::Paragraph));
    }

    #[test]
    fn fuzzy_subsequence_matches() {
        let ws = workspace_with_fixtures();
        let hits = ws.search("dscamt", None);
        assert!(hits
            .iter()
            .any(|h| h.kind == SearchHitKind::TableColumn && h.column.as_deref() == Some("c-out")));
    }

    #[test]
    fn empty_query_returns_nothing() {
        let ws = workspace_with_fixtures();
        assert!(ws.search("  ", None).is_empty());
        assert!(ws.search("", None).is_empty());
    }

    #[test]
    fn respects_limit() {
        let ws = workspace_with_fixtures();
        let hits = ws.search("o", Some(3));
        assert!(hits.len() <= 3);
    }
}
