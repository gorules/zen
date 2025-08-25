use proc_macro::TokenStream;
use quote::quote;
use syn::parse_macro_input;

pub fn to_variable_impl(input: TokenStream) -> TokenStream {
    let mut input = parse_macro_input!(input as syn::DeriveInput);

    serde_derive_internals::replace_receiver(&mut input);

    let ctxt = serde_derive_internals::Ctxt::new();
    let container = match serde_derive_internals::ast::Container::from_ast(
        &ctxt,
        &input,
        serde_derive_internals::Derive::Serialize,
    ) {
        Some(container) => container,
        None => return ctxt.check().unwrap_err().into_compile_error().into(),
    };

    if let Err(err) = ctxt.check() {
        return err.into_compile_error().into();
    }

    let ident = &container.ident;
    let (impl_generics, ty_generics, where_clause) = container.generics.split_for_impl();

    let body = match &container.data {
        serde_derive_internals::ast::Data::Struct(_, fields) => generate_struct_body(fields),
        serde_derive_internals::ast::Data::Enum(variants) => {
            generate_enum_body(variants, &container)
        }
    };

    let impl_block = quote! {
        #[automatically_derived]
        impl #impl_generics _ToVariable for #ident #ty_generics #where_clause {
            fn to_variable(&self) -> _Variable {
                #body
            }
        }
    };

    quote! {
        #[doc(hidden)]
        #[allow(non_upper_case_globals, unused_attributes, unused_qualifications, clippy::absolute_paths)]
        const _: () = {
            extern crate zen_expression as _zen_expression;

            use _zen_expression::variable::{Variable as _Variable, VariableMap as _VariableMap, VariableMapExt, ToVariable as _ToVariable};
            use ::std::rc::Rc as _Rc;

            #impl_block
        };
    }.into()
}

fn generate_struct_body(fields: &[serde_derive_internals::ast::Field]) -> proc_macro2::TokenStream {
    let active_fields: Vec<_> = fields
        .iter()
        .filter(|field| !field.attrs.skip_serializing())
        .collect();

    let field_count = active_fields.len();

    let field_mappings = active_fields.iter().map(|field| {
        let field_ident = match &field.member {
            syn::Member::Named(ident) => ident,
            syn::Member::Unnamed(_) => panic!("ToVariable only supports named fields"),
        };

        let serialized_name = field.attrs.name().serialize_name();

        quote! {
            map.insert(
                _Rc::from(#serialized_name),
                self.#field_ident.to_variable()
            );
        }
    });

    quote! {
        let mut map = _VariableMap::with_capacity(#field_count);
        #(#field_mappings)*
        _Variable::from_object(map)
    }
}

fn generate_enum_body(
    variants: &[serde_derive_internals::ast::Variant],
    container: &serde_derive_internals::ast::Container,
) -> proc_macro2::TokenStream {
    let enum_ident = &container.ident;

    let active_variants: Vec<_> = variants
        .iter()
        .filter(|variant| !variant.attrs.skip_serializing())
        .collect();

    let variant_arms = active_variants
        .iter()
        .map(|variant| generate_variant_arm(enum_ident, variant, container));

    quote! {
        match self {
            #(#variant_arms)*
        }
    }
}

fn generate_variant_arm(
    enum_ident: &syn::Ident,
    variant: &serde_derive_internals::ast::Variant,
    container: &serde_derive_internals::ast::Container,
) -> proc_macro2::TokenStream {
    let variant_ident = &variant.ident;

    let variant_name = variant.attrs.name().serialize_name();
    let rename_rule = container.attrs.rename_all_rules().serialize;
    let type_key = rename_rule.apply_to_field("type");
    let value_key = rename_rule.apply_to_field("value");

    match variant.style {
        serde_derive_internals::ast::Style::Unit => {
            quote! {
                #enum_ident::#variant_ident => {
                    _Variable::String(_Rc::from(#variant_name))
                }
            }
        }

        serde_derive_internals::ast::Style::Newtype => {
            quote! {
                #enum_ident::#variant_ident(value) => {
                    let mut map = _VariableMap::with_capacity(2);
                    map.insert(_Rc::from(#type_key), _Variable::String(_Rc::from(#variant_name)));
                    map.insert(_Rc::from(#value_key), value.to_variable());
                    _Variable::from_object(map)
                }
            }
        }

        serde_derive_internals::ast::Style::Tuple => {
            let field_count = variant.fields.len();
            let field_patterns: Vec<_> = (0..field_count)
                .map(|i| quote::format_ident!("field_{}", i))
                .collect();

            if field_count == 1 {
                quote! {
                    #enum_ident::#variant_ident(#(#field_patterns),*) => {
                        let mut map = _VariableMap::with_capacity(2);
                        map.insert(_Rc::from(#type_key), _Variable::String(_Rc::from(#variant_name)));
                        map.insert(_Rc::from(#value_key), (#(#field_patterns)*).to_variable());
                        _Variable::from_object(map)
                    }
                }
            } else {
                let field_mappings = field_patterns.iter().enumerate().map(|(i, pattern)| {
                    let field_key = rename_rule.apply_to_field(&format!("field_{}", i));
                    quote! {
                        map.insert(_Rc::from(#field_key), (#pattern).to_variable());
                    }
                });

                quote! {
                    #enum_ident::#variant_ident(#(#field_patterns),*) => {
                        let mut map = _VariableMap::with_capacity(#field_count + 1);
                        map.insert(_Rc::from(#type_key), _Variable::String(_Rc::from(#variant_name)));
                        #(#field_mappings)*
                        _Variable::from_object(map)
                    }
                }
            }
        }

        serde_derive_internals::ast::Style::Struct => {
            let active_fields: Vec<_> = variant
                .fields
                .iter()
                .filter(|field| !field.attrs.skip_serializing())
                .collect();

            let field_mappings = active_fields.iter().map(|field| {
                let field_ident = match &field.member {
                    syn::Member::Named(ident) => ident,
                    syn::Member::Unnamed(_) => panic!("Unexpected unnamed field in struct variant"),
                };

                let field_name = field.attrs.name().serialize_name();

                quote! {
                    map.insert(_Rc::from(#field_name), #field_ident.to_variable());
                }
            });

            let field_patterns = active_fields.iter().map(|field| match &field.member {
                syn::Member::Named(ident) => quote! { #ident },
                syn::Member::Unnamed(_) => panic!("Unexpected unnamed field in struct variant"),
            });

            let field_count = active_fields.len() + 1;
            quote! {
                #enum_ident::#variant_ident { #(#field_patterns),* } => {
                    let mut map = _VariableMap::with_capacity(#field_count);
                    map.insert(_Rc::from(#type_key), _Variable::String(_Rc::from(#variant_name)));
                    #(#field_mappings)*
                    _Variable::from_object(map)
                }
            }
        }
    }
}
