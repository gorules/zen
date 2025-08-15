use proc_macro::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, Variant, parse_macro_input};

pub fn to_variable_impl(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;

    match &input.data {
        Data::Struct(data_struct) => generate_struct_impl(name, data_struct),
        Data::Enum(data_enum) => generate_enum_impl(name, data_enum),
        _ => syn::Error::new_spanned(&input, "ToVariable only supports structs and enums")
            .to_compile_error()
            .into(),
    }
}

fn generate_struct_impl(name: &syn::Ident, data_struct: &syn::DataStruct) -> TokenStream {
    let fields = match &data_struct.fields {
        Fields::Named(fields_named) => &fields_named.named,
        _ => {
            return syn::Error::new_spanned(
                name,
                "ToVariable only supports structs with named fields",
            )
            .to_compile_error()
            .into();
        }
    };

    let field_count = fields.len();
    let field_mappings = fields.iter().map(|field| {
        let field_name = field.ident.as_ref().unwrap();
        let field_name_str = field_name.to_string();

        quote! {
            map.insert(
                std::rc::Rc::from(#field_name_str),
                (&self.#field_name).to_variable()
            );
        }
    });

    let expanded = quote! {
        impl zen_expression::variable::ToVariable for #name {
            fn to_variable(&self) -> zen_expression::Variable {
                use ahash::{HashMap, HashMapExt};
                use std::rc::Rc;

                let mut map = HashMap::with_capacity(#field_count);
                #(#field_mappings)*
                zen_expression::Variable::from_object(map)
            }
        }
    };

    TokenStream::from(expanded)
}

fn generate_enum_impl(name: &syn::Ident, data_enum: &syn::DataEnum) -> TokenStream {
    let variants = data_enum
        .variants
        .iter()
        .map(|variant| generate_variant_match(name, variant));

    let expanded = quote! {
        impl zen_expression::variable::ToVariable for #name {
            fn to_variable(&self) -> zen_expression::Variable {
                use ahash::{HashMap, HashMapExt};
                use std::rc::Rc;
                use zen_expression::Variable;
                use zen_expression::variable::{ToVariable};

                match self {
                    #(#variants)*
                }
            }
        }
    };

    TokenStream::from(expanded)
}

fn generate_variant_match(enum_name: &syn::Ident, variant: &Variant) -> proc_macro2::TokenStream {
    let variant_name = &variant.ident;
    let variant_str = variant_name.to_string();

    match &variant.fields {
        // Unit variant: MyEnum::Variant
        Fields::Unit => {
            quote! {
                #enum_name::#variant_name => Variable::String(Rc::from(#variant_str)),
            }
        }

        // Tuple variant: MyEnum::Variant(T1, T2, ...)
        Fields::Unnamed(fields) => {
            let field_count = fields.unnamed.len();
            let field_patterns: Vec<_> = (0..field_count)
                .map(|i| quote::format_ident!("field_{}", i))
                .collect();

            if field_count == 1 {
                // Single field - flatten directly with type
                quote! {
                    #enum_name::#variant_name(#(#field_patterns),*) => {
                        let mut map = HashMap::with_capacity(2);
                        map.insert(Rc::from("type"), Variable::String(Rc::from(#variant_str)));
                        map.insert(Rc::from("value"), (#(#field_patterns)*).to_variable());
                        Variable::from_object(map)
                    },
                }
            } else {
                // Multiple fields - flatten as indexed fields
                let field_mappings = field_patterns.iter().enumerate().map(|(i, pattern)| {
                    let field_name = format!("field_{}", i);
                    quote! {
                        map.insert(Rc::from(#field_name), (#pattern).to_variable());
                    }
                });

                let total_capacity = field_count + 1; // +1 for "type"

                quote! {
                    #enum_name::#variant_name(#(#field_patterns),*) => {
                        let mut map = HashMap::with_capacity(#total_capacity);
                        map.insert(Rc::from("type"), Variable::String(Rc::from(#variant_str)));
                        #(#field_mappings)*
                        Variable::from_object(map)
                    },
                }
            }
        }

        Fields::Named(fields) => {
            let field_count = fields.named.len() + 1; // +1 for the "type" field
            let field_mappings = fields.named.iter().map(|field| {
                let field_name = field.ident.as_ref().unwrap();
                let field_name_str = field_name.to_string();

                quote! {
                    map.insert(Rc::from(#field_name_str), (#field_name).to_variable());
                }
            });

            let field_patterns = fields.named.iter().map(|field| {
                let field_name = field.ident.as_ref().unwrap();
                quote! { #field_name }
            });

            quote! {
                #enum_name::#variant_name { #(#field_patterns),* } => {
                    let mut map = HashMap::with_capacity(#field_count);
                    map.insert(Rc::from("type"), Variable::String(Rc::from(#variant_str)));
                    #(#field_mappings)*
                    Variable::from_object(map)
                },
            }
        }
    }
}
