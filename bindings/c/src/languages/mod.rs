/// Language specific bindings, loaders and helpers

#[cfg(not(feature = "cdylib"))]
pub(crate) mod go;
