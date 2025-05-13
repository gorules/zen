mod compiler;
mod decision_policy;

#[cfg(test)]
mod test {
    use crate::model::{PolicyBundle, PolicyDocument, PolicyRuleCondition};
    use ahash::{HashMap, HashMapExt};
    use bumpalo::Bump;
    use petgraph::algo::toposort;
    use petgraph::graph::DiGraph;
    use std::ops::Deref;
    use zen_expression::intellisense::{DependencyProvider, DependencyProviderResponse};
    use zen_expression::lexer::Lexer;
    use zen_expression::parser::Parser;

    #[tokio::test]
    async fn policy_test() {
        let policy_book: PolicyBundle = serde_json::from_str(include_str!("policy.json")).unwrap();

        let policy = match policy_book.documents[0].deref() {
            PolicyDocument::DecisionTable { .. } => return,
            PolicyDocument::RuleSet { content } => content,
        };

        // println!("{:#?}", policy);

        let s = policy
            .rules
            .iter()
            .map(|p| {
                let d = via_str(p.outcome.deref());
                let response = match p.conditions.as_ref() {
                    None => d,
                    Some(s) => d.merge(policy_data(s, Default::default())),
                };

                (p.outcome.clone(), response)
            })
            .collect::<Vec<_>>();

        let mut graph = DiGraph::new();

        let mut node_indices = HashMap::new();
        for (expr, r) in &s {
            for p in r.provides.iter().chain(r.dependencies.iter()) {
                node_indices
                    .entry(p.clone())
                    .or_insert_with(|| graph.add_node(p.clone()));
            }
        }

        for (expr, r) in &s {
            for p in r.provides.iter() {
                let provider_idx = node_indices[p];

                for dep in r.dependencies.iter() {
                    let dep_idx = node_indices[dep];
                    graph.add_edge(dep_idx, provider_idx, ());
                }
            }
        }

        // Perform topological sort to get resolution order
        match toposort(&graph, None) {
            Ok(sorted) => {
                println!("Resolution order:");
                for (i, node_idx) in sorted.into_iter().enumerate() {
                    println!("{}. {}", i + 1, graph[node_idx]);
                }
            }
            Err(_) => {
                println!("Cyclic dependency detected!");
            }
        }

        // Print the dependency graph
        println!("\nDependency graph:");
        for (_, r) in &s {
            for p in r.provides.iter() {
                let provider_idx = node_indices[p];
                println!("{} depends on:", p);

                for neighbor in
                    graph.neighbors_directed(provider_idx, petgraph::Direction::Incoming)
                {
                    println!("  - {}", graph[neighbor]);
                }
            }
        }
    }

    fn policy_data(
        condition: &PolicyRuleCondition,
        response: DependencyProviderResponse,
    ) -> DependencyProviderResponse {
        match condition {
            PolicyRuleCondition::Simple { expression } => response.merge(via_str(expression)),
            PolicyRuleCondition::All { items: a }
            | PolicyRuleCondition::Any { items: a }
            | PolicyRuleCondition::None { items: a } => {
                a.iter().fold(response, |acc, p| policy_data(p, acc))
            }
        }
    }

    fn via_str(expression: &str) -> DependencyProviderResponse {
        let arena = Bump::new();
        let mut lexer = Lexer::new();
        let tokens = lexer.tokenize(expression).unwrap();
        let parser = Parser::try_new(tokens, &arena)
            .map(|p| p.standard())
            .unwrap();

        let parser_result = parser.with_metadata().parse();
        let ast = parser_result.root;

        DependencyProvider::generate(&ast, &arena)
    }
}
