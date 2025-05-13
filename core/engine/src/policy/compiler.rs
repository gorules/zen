use crate::model::{PolicyBundle, PolicyDecisionTable, PolicyDocument, PolicyRule};
use bumpalo::Bump;
use std::sync::Arc;
use zen_expression::intellisense::{DependencyProvider, DependencyProviderResponse, IntelliSense};
use zen_expression::lexer::Lexer;
use zen_expression::parser::Parser;
use zen_expression::ExpressionKind;

pub struct PolicyCompiler {}

// Ultimately, backed by an array of execution steps needed to be performed
pub enum PolicyExecutionStep {
    DecisionTable(Arc<PolicyDecisionTable>),
    Rule(Arc<PolicyRule>),
}

struct PolicyCompilerStep {
    execution_step: PolicyExecutionStep,
    dependency_provider: DependencyProviderResponse,
}

// Computes the execution order for Decision Tables and Rules
impl PolicyCompiler {
    fn compile_bundle(policy_bundle: PolicyBundle) {
        let mut arena = Bump::new();
        let is = IntelliSense::new();

        for document in policy_bundle.documents.iter() {
            match document.as_ref() {
                PolicyDocument::DecisionTable { content } => {}
                PolicyDocument::RuleSet { content } => {}
            }
        }
    }

    fn compile_rule(rule: Arc<PolicyRule>) -> PolicyCompilerStep {
        let mut is = IntelliSense::new();
        let outcome = {
            let outcome_str = rule.outcome.as_str();
            is.dependencies(outcome_str).unwrap_or_default()
        };
        
        if let Some(condition) = &rule.conditions {
            
        }
        
        rule.conditions.iter().fold(outcome, |acc, c| {
            let rule_str = 
        })

        PolicyCompilerStep {
            execution_step: PolicyExecutionStep::Rule(rule),
            dependency_provider: outcome_deps.unwrap_or_default(),
        }
    }

    fn compile_decision_table(
        decision_table: Arc<PolicyDecisionTable>,
        c: &mut InternalCompiler,
    ) -> PolicyCompilerStep {
    }
}

struct InternalCompiler<'arena> {
    arena: &'arena mut Bump,
    lexer: Lexer<'arena>,
}
