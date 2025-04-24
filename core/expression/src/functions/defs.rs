use crate::functions::arguments::Arguments;
use crate::functions::registry::FunctionDefinition;
use crate::variable::VariableType;
use crate::Variable;

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
    pub implementation: fn(Arguments) -> anyhow::Result<Variable>,
}

impl FunctionDefinition for StaticFunction {
    fn required_parameters(&self) -> usize {
        self.signature.parameters.len()
    }

    fn optional_parameters(&self) -> usize {
        0
    }

    fn check_types(&self, args: &[VariableType]) -> Result<VariableType, String> {
        if args.len() != self.required_parameters() {
            return Err(format!(
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
                return Err(format!(
                    "Argument {} of type `{}` is not assignable to parameter of type `{}`.",
                    i + 1,
                    arg,
                    expected_type
                ));
            }
        }

        Ok(self.signature.return_type.clone())
    }

    fn call(&self, args: Arguments) -> anyhow::Result<Variable> {
        (&self.implementation)(args)
    }
}

#[derive(Clone)]
pub struct CompositeFunction {
    pub signatures: Vec<FunctionSignature>,
    pub implementation: fn(Arguments) -> anyhow::Result<Variable>,
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

    fn check_types(&self, args: &[VariableType]) -> Result<VariableType, String> {
        if self.signatures.is_empty() {
            return Err("No implementation".to_string());
        }

        let required_params = self.required_parameters();
        let optional_params = self.optional_parameters();
        let total_params = required_params + optional_params;

        if args.len() < required_params || args.len() > total_params {
            return Err(format!(
                "Expected `{required_params} - {total_params}` arguments, got `{}`.",
                args.len()
            ));
        }

        for signature in &self.signatures {
            let all_match = args
                .iter()
                .zip(signature.parameters.iter())
                .all(|(arg, param)| arg.satisfies(param));

            if all_match {
                return Ok(signature.return_type.clone());
            }
        }

        for (i, arg) in args.iter().enumerate() {
            let possible_types: Vec<&VariableType> = self
                .signatures
                .iter()
                .filter_map(|sig| sig.parameters.get(i))
                .collect();

            if !possible_types.iter().any(|param| arg.satisfies(param)) {
                let type_union = possible_types
                    .iter()
                    .map(|t| t.to_string())
                    .collect::<Vec<_>>()
                    .join(" | ");

                return Err(format!(
                    "Argument of type `{}` is not assignable to parameter of type `{}`.",
                    arg, type_union
                ));
            }
        }

        Err("No overload of function matches the argument types.".to_string())
    }

    fn call(&self, args: Arguments) -> anyhow::Result<Variable> {
        (&self.implementation)(args)
    }
}
