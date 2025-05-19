use crate::model::{PolicyBundle, PolicyDecisionTable, PolicyDocument, PolicyRule};
use ahash::{HashMap, HashMapExt};
use bumpalo::Bump;
use petgraph::algo::toposort;
use petgraph::graph::DiGraph;
use std::sync::Arc;
use zen_expression::intellisense::{DependencyProviderResponse, IntelliSense};
use zen_expression::lexer::Lexer;

pub struct PolicyCompiler {}

// Ultimately, backed by an array of execution steps needed to be performed
#[derive(Debug, Clone)]
pub enum PolicyExecutionStep {
    DecisionTable(Arc<PolicyDecisionTable>),
    Rule(Arc<PolicyRule>),
}

#[derive(Debug, Clone)]
struct PolicyCompilerStep {
    execution_step: PolicyExecutionStep,
    dependency_provider: DependencyProviderResponse,
}

// Computes the execution order for Decision Tables and Rules
impl PolicyCompiler {
    fn compile_bundle(policy_bundle: PolicyBundle) {
        let steps = policy_bundle
            .documents
            .iter()
            .flat_map(|d| match d.as_ref() {
                PolicyDocument::DecisionTable { content } => vec![PolicyCompilerStep {
                    dependency_provider: Default::default(),
                    execution_step: PolicyExecutionStep::DecisionTable(content.clone()),
                }],
                PolicyDocument::RuleSet { content } => content
                    .rules
                    .iter()
                    .map(|r| Self::compile_rule(r.clone()))
                    .collect::<Vec<_>>(),
            })
            .collect::<Vec<_>>();

        println!("{:#?}", steps);

        let sorted_steps = Self::sort_steps_by_dependencies(steps);
        println!("{:#?}", sorted_steps);

        let sorted_expressions = sorted_steps
            .iter()
            .map(|s| match &s.execution_step {
                PolicyExecutionStep::DecisionTable(_) => String::from("decisionTable"),
                PolicyExecutionStep::Rule(r) => r.outcome.clone(),
            })
            .collect::<Vec<_>>();

        println!("{:#?}", sorted_expressions);
    }

    fn compile_rule(rule: Arc<PolicyRule>) -> PolicyCompilerStep {
        let mut is = IntelliSense::new();
        let outcome = {
            let outcome_str = rule.outcome.as_str();
            is.dependencies(outcome_str).unwrap_or_default()
        };

        let Some(condition) = &rule.conditions else {
            return PolicyCompilerStep {
                execution_step: PolicyExecutionStep::Rule(rule.clone()),
                dependency_provider: outcome,
            };
        };

        let complete_outcome = condition.to_vec().iter().fold(outcome, |acc, c| {
            let condition_provider = is.dependencies(c);
            acc.merge(condition_provider.unwrap_or_default())
        });

        PolicyCompilerStep {
            execution_step: PolicyExecutionStep::Rule(rule.clone()),
            dependency_provider: complete_outcome,
        }
    }

    // fn compile_decision_table(
    //     decision_table: Arc<PolicyDecisionTable>,
    //     c: &mut InternalCompiler,
    // ) -> PolicyCompilerStep {
    // }

    fn sort_steps_by_dependencies(steps: Vec<PolicyCompilerStep>) -> Vec<PolicyCompilerStep> {
        // Create a directed graph
        let mut graph = DiGraph::<usize, ()>::new();
        let mut nodes = Vec::new();
        let mut provides_map: HashMap<_, _> = HashMap::new();

        // Create nodes for each step
        for (idx, step) in steps.iter().enumerate() {
            let node_idx = graph.add_node(idx);
            nodes.push(node_idx);

            // Map provided values to step indices
            for provided in &step.dependency_provider.provides {
                provides_map.insert(provided.clone(), idx);
            }
        }

        // Add edges based on dependencies
        for (idx, step) in steps.iter().enumerate() {
            for dependency in &step.dependency_provider.dependencies {
                if let Some(&provider_idx) = provides_map.get(dependency) {
                    if provider_idx != idx {
                        // Avoid self-dependencies
                        graph.add_edge(nodes[provider_idx], nodes[idx], ());
                    }
                }
            }
        }

        // Perform topological sort
        match toposort(&graph, None) {
            Ok(sorted_indices) => {
                // Reconstruct the sorted steps
                sorted_indices
                    .into_iter()
                    .map(|node_idx| {
                        let step_idx = graph[node_idx];
                        steps[step_idx].clone()
                    })
                    .collect()
            }
            Err(_cycle) => {
                // Handle cycles in the dependency graph
                // For simplicity, return the original order
                eprintln!("Warning: Dependency cycle detected in policy steps");
                steps
            }
        }
    }
}

struct InternalCompiler<'arena> {
    arena: &'arena mut Bump,
    lexer: Lexer<'arena>,
}

#[cfg(test)]
mod test {
    use crate::model::PolicyBundle;
    use crate::policy::compiler::PolicyCompiler;

    #[test]
    fn initial() {
        let policy_bundle: PolicyBundle =
            serde_json::from_str(include_str!("policy.json")).unwrap();

        let s = PolicyCompiler::compile_bundle(policy_bundle);
    }
}
