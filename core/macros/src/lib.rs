use proc_macro::TokenStream;

mod to_variable;

#[proc_macro_derive(ToVariable, attributes(serde))]
pub fn derive_to_variable(input: TokenStream) -> TokenStream {
    to_variable::to_variable_impl(input)
}
