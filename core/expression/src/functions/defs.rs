use crate::functions::arguments::Arguments;
use crate::variable::VariableType;
use crate::Variable;
use std::collections::HashSet;
use std::rc::Rc;

pub trait FunctionDefinition {
    fn required_parameters(&self) -> usize;
    fn optional_parameters(&self) -> usize;
    fn check_types(&self, args: &[Rc<VariableType>]) -> FunctionTypecheck;
    fn call(&self, args: Arguments) -> anyhow::Result<Variable>;
    fn param_type(&self, index: usize) -> Option<VariableType>;
    fn param_type_str(&self, index: usize) -> String;
    fn return_type(&self) -> VariableType;
    fn return_type_str(&self) -> String;
}

#[derive(Debug, Default)]
pub struct FunctionTypecheck {
    pub general: Option<String>,
    pub arguments: Vec<(usize, String)>,
    pub return_type: VariableType,
}

#[derive(Clone)]
pub struct FunctionSignature {
    pub parameters: Vec<VariableType>,
    pub return_type: VariableType,
}

impl FunctionSignature {
    pub fn single(parameter: VariableType, return_type: VariableType) -> Self {
        Self {
            parameters: vec![parameter],
            return_type,
        }
    }
}

#[derive(Clone)]
pub struct StaticFunction {
    pub signature: FunctionSignature,
    pub implementation: Rc<dyn Fn(Arguments) -> anyhow::Result<Variable>>,
}

impl FunctionDefinition for StaticFunction {
    fn required_parameters(&self) -> usize {
        self.signature.parameters.len()
    }

    fn optional_parameters(&self) -> usize {
        0
    }

    fn check_types(&self, args: &[Rc<VariableType>]) -> FunctionTypecheck {
        let mut typecheck = FunctionTypecheck::default();
        typecheck.return_type = self.signature.return_type.clone();

        if args.len() != self.required_parameters() {
            typecheck.general = Some(format!(
                "Expected `{}` arguments, got `{}`.",
                self.required_parameters(),
                args.len()
            ));
        }

        // Check each parameter type
        for (i, (arg, expected_type)) in args
            .iter()
            .zip(self.signature.parameters.iter())
            .enumerate()
        {
            if !arg.satisfies(expected_type) {
                typecheck.arguments.push((
                    i,
                    format!(
                        "Argument of type `{arg}` is not assignable to parameter of type `{expected_type}`.",
                    ),
                ));
            }
        }

        typecheck
    }

    fn call(&self, args: Arguments) -> anyhow::Result<Variable> {
        (&self.implementation)(args)
    }

    fn param_type(&self, index: usize) -> Option<VariableType> {
        self.signature.parameters.get(index).cloned()
    }

    fn param_type_str(&self, index: usize) -> String {
        self.signature
            .parameters
            .get(index)
            .map(|x| x.to_string())
            .unwrap_or_else(|| "never".to_string())
    }

    fn return_type(&self) -> VariableType {
        self.signature.return_type.clone()
    }

    fn return_type_str(&self) -> String {
        self.signature.return_type.to_string()
    }
}

#[derive(Clone)]
pub struct CompositeFunction {
    pub signatures: Vec<FunctionSignature>,
    pub implementation: Rc<dyn Fn(Arguments) -> anyhow::Result<Variable>>,
}

impl FunctionDefinition for CompositeFunction {
    fn required_parameters(&self) -> usize {
        self.signatures
            .iter()
            .map(|x| x.parameters.len())
            .min()
            .unwrap_or(0)
    }

    fn optional_parameters(&self) -> usize {
        let required_params = self.required_parameters();
        let max = self
            .signatures
            .iter()
            .map(|x| x.parameters.len())
            .max()
            .unwrap_or(0);

        max - required_params
    }

    fn check_types(&self, args: &[Rc<VariableType>]) -> FunctionTypecheck {
        let mut typecheck = FunctionTypecheck::default();
        if self.signatures.is_empty() {
            typecheck.general = Some("No implementation".to_string());
            return typecheck;
        }

        let required_params = self.required_parameters();
        let optional_params = self.optional_parameters();
        let total_params = required_params + optional_params;

        if args.len() < required_params || args.len() > total_params {
            typecheck.general = Some(format!(
                "Expected `{required_params} - {total_params}` arguments, got `{}`.",
                args.len()
            ))
        }

        for signature in &self.signatures {
            let all_match = args
                .iter()
                .zip(signature.parameters.iter())
                .all(|(arg, param)| arg.satisfies(param));
            if all_match {
                typecheck.return_type = signature.return_type.clone();
                return typecheck;
            }
        }

        for (i, arg) in args.iter().enumerate() {
            let possible_types: Vec<&VariableType> = self
                .signatures
                .iter()
                .filter_map(|sig| sig.parameters.get(i))
                .collect();

            if !possible_types.iter().any(|param| arg.satisfies(param)) {
                let type_union = self.param_type_str(i);
                typecheck.arguments.push((
                    i,
                    format!(
                        "Argument of type `{arg}` is not assignable to parameter of type `{type_union}`.",
                    ),
                ))
            }
        }

        let available_signatures = self
            .signatures
            .iter()
            .map(|sig| {
                let param_list = sig
                    .parameters
                    .iter()
                    .map(|x| x.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("`({param_list}) -> {}`", sig.return_type)
            })
            .collect::<Vec<_>>()
            .join("\n");
        typecheck.general = Some(format!("No function overload matches provided arguments. Available overloads:\n{available_signatures}"));

        typecheck
    }

    fn call(&self, args: Arguments) -> anyhow::Result<Variable> {
        (&self.implementation)(args)
    }

    fn param_type(&self, index: usize) -> Option<VariableType> {
        self.signatures
            .iter()
            .filter_map(|sig| sig.parameters.get(index))
            .cloned()
            .reduce(|a, b| a.merge(&b))
    }

    fn param_type_str(&self, index: usize) -> String {
        let possible_types: Vec<String> = self
            .signatures
            .iter()
            .filter_map(|sig| sig.parameters.get(index))
            .map(|x| x.to_string())
            .collect();
        if possible_types.is_empty() {
            return String::from("never");
        }

        let is_optional = possible_types.len() != self.signatures.len();
        let possible_types: Vec<String> = possible_types
            .into_iter()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        let type_union = possible_types.join(" | ");
        if is_optional {
            return format!("Optional<{type_union}>");
        }

        type_union
    }

    fn return_type(&self) -> VariableType {
        self.signatures
            .iter()
            .map(|sig| &sig.return_type)
            .cloned()
            .reduce(|a, b| a.merge(&b))
            .unwrap_or(VariableType::Null)
    }

    fn return_type_str(&self) -> String {
        let possible_types: Vec<String> = self
            .signatures
            .iter()
            .map(|sig| sig.return_type.clone())
            .map(|x| x.to_string())
            .collect();
        if possible_types.is_empty() {
            return String::from("never");
        }

        possible_types
            .into_iter()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(" | ")
    }
}
